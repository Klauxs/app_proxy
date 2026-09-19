//! Immutable protected core configurations and a compare-and-swap runtime journal.
//! Journal state is evidence of intent/ownership, never evidence of network health.
use crate::{
    Error, Result, storage_security as security,
    store::{self, Store},
};
use app_proxy_core::{ProcessIdentity, model::Endpoint, singbox};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ};

const RECORD_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreState {
    Stopped {},
    Down {
        generation: Uuid,
    },
    Starting {
        generation: Uuid,
    },
    Running {
        generation: Uuid,
        process: ProcessIdentity,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    store_id: Uuid,
    state: CoreState,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreProfile {
    pub id: Uuid,
    pub endpoint: Endpoint,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    schema_version: u32,
    store_id: Uuid,
    generation: Uuid,
    digest: [u8; 32],
    profiles: Vec<CoreProfile>,
}

/// Keep alive through check and spawn. Denies config replacement/writes and
/// directory moves. Drop never deletes configurations or stops a process.
pub struct CoreGeneration {
    header: Header,
    path: PathBuf,
    _file: File,
    _directories: Vec<OwnedHandle>,
}
impl CoreGeneration {
    pub fn id(&self) -> Uuid {
        self.header.generation
    }
    pub fn config_path(&self) -> &Path {
        &self.path
    }
    pub fn profiles(&self) -> &[CoreProfile] {
        &self.header.profiles
    }
}

impl Store {
    fn core_directories(&self, create: bool) -> Result<Vec<OwnedHandle>> {
        let sid = self.load()?.owner_sid;
        let mut pins = Vec::new();
        for relative in ["state/core", "state/core/generations"] {
            let path = self.root().join(relative);
            if create && !path.try_exists()? {
                fs::create_dir(&path)?;
            }
            let pin = security::directory(&path, false)?;
            security::verify(pin.as_raw_handle(), &sid, false)?;
            pins.push(pin);
        }
        Ok(pins)
    }

    /// Creates only a candidate. Active journal and manifest are unchanged.
    pub fn prepare_core_generation(&self, profile_ids: &[Uuid]) -> Result<CoreGeneration> {
        let manifest = self.load()?;
        let config = singbox::compile(&manifest, profile_ids, |id| {
            self.read_secret(id)
                .map_err(|_| app_proxy_core::model::ValidationError("CORE_SECRET_UNAVAILABLE"))
        })
        .map_err(|e| Error::Invalid(e.0))?;
        let _pins = self.core_directories(true)?;
        let generation = Uuid::new_v4();
        let path = self
            .root()
            .join(format!("state/core/generations/{generation}"));
        fs::create_dir(&path)?;
        let pin = security::directory(&path, false)?;
        security::verify(pin.as_raw_handle(), &manifest.owner_sid, false)?;
        let header = Header {
            schema_version: 1,
            store_id: manifest.store_id,
            generation,
            digest: Sha256::digest(config.bytes()).into(),
            profiles: config
                .profiles()
                .iter()
                .map(|id| {
                    let profile = manifest
                        .profiles
                        .iter()
                        .find(|p| p.id == *id)
                        .expect("compiled profile");
                    CoreProfile {
                        id: *id,
                        endpoint: profile.endpoint.clone(),
                    }
                })
                .collect(),
        };
        store::write_new(
            &path.join("config.json"),
            config.bytes(),
            &manifest.owner_sid,
        )?;
        // Header last: an interrupted candidate cannot be opened as complete.
        store::write_new(
            &path.join("generation.json"),
            &store::encode(&header, RECORD_LIMIT)?,
            &manifest.owner_sid,
        )?;
        self.open_core_generation(generation)
    }

    pub fn open_core_generation(&self, generation: Uuid) -> Result<CoreGeneration> {
        if generation.is_nil() {
            return Err(Error::Invalid("INVALID_CORE_GENERATION"));
        }
        let manifest = self.load()?;
        let mut pins = self.core_directories(false)?;
        let directory = self
            .root()
            .join(format!("state/core/generations/{generation}"));
        let pin = security::directory(&directory, false)?;
        security::verify(pin.as_raw_handle(), &manifest.owner_sid, false)?;
        pins.push(pin);
        let header: Header = store::decode(&store::read_protected(
            &directory.join("generation.json"),
            &manifest.owner_sid,
            RECORD_LIMIT,
        )?)?;
        if header.schema_version != 1
            || header.store_id != manifest.store_id
            || header.generation != generation
            || header.profiles.is_empty()
            || header
                .profiles
                .iter()
                .any(|p| p.id.is_nil() || !p.endpoint.host.is_loopback() || p.endpoint.port == 0)
            || header.profiles.windows(2).any(|p| p[0].id >= p[1].id)
        {
            return Err(Error::Invalid("CORE_GENERATION_HEADER_MISMATCH"));
        }
        let path = directory.join("config.json");
        security::no_reparse(&path)?;
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)?;
        security::verify(file.as_raw_handle(), &manifest.owner_sid, false)?;
        if !file.metadata()?.is_file() {
            return Err(Error::Invalid("CORE_CONFIG_FILE_REQUIRED"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(singbox::CONFIG_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > singbox::CONFIG_LIMIT
            || <[u8; 32]>::from(Sha256::digest(&bytes)) != header.digest
        {
            return Err(Error::Invalid("CORE_CONFIG_DIGEST_MISMATCH"));
        }
        // Health evidence must use the same ID -> listener mapping as the
        // immutable config, not independently editable header metadata.
        let config: serde_json::Value = store::decode(&bytes)?;
        let inbounds = config
            .get("inbounds")
            .and_then(|v| v.as_array())
            .ok_or(Error::Invalid("CORE_PROFILE_MAPPING_MISMATCH"))?;
        if inbounds.len() != header.profiles.len()
            || header.profiles.iter().any(|profile| {
                let tag = format!("in-{}", profile.id);
                inbounds
                    .iter()
                    .filter(|inbound| {
                        inbound["tag"].as_str() == Some(&tag)
                            && inbound["type"] == "http"
                            && inbound["listen"].as_str()
                                == Some(&profile.endpoint.host.to_string())
                            && inbound["listen_port"].as_u64() == Some(profile.endpoint.port as u64)
                    })
                    .count()
                    != 1
            })
        {
            return Err(Error::Invalid("CORE_PROFILE_MAPPING_MISMATCH"));
        }
        Ok(CoreGeneration {
            header,
            path,
            _file: file,
            _directories: pins,
        })
    }

    /// Compare configuration bytes, so display-name/revision changes do not
    /// invalidate a prepared plan or force a running core to restart.
    pub fn core_generation_is_current(&self, generation: &CoreGeneration) -> Result<bool> {
        let manifest = self.load()?;
        if generation.header.store_id != manifest.store_id {
            return Err(Error::Invalid("CORE_GENERATION_STORE_MISMATCH"));
        }
        let ids: Vec<_> = generation.profiles().iter().map(|p| p.id).collect();
        let compiled = singbox::compile(&manifest, &ids, |id| {
            self.read_secret(id)
                .map_err(|_| app_proxy_core::model::ValidationError("CORE_SECRET_UNAVAILABLE"))
        })
        .map_err(|e| Error::Invalid(e.0))?;
        Ok(<[u8; 32]>::from(Sha256::digest(compiled.bytes())) == generation.header.digest)
    }

    pub fn core_state(&self) -> Result<CoreState> {
        if !self.root().join("state/core").try_exists()? {
            return Ok(CoreState::Stopped {});
        }
        let sid = self.load()?.owner_sid;
        let _pin = security::directory(&self.root().join("state/core"), false)?;
        security::verify(_pin.as_raw_handle(), &sid, false)?;
        let path = self.root().join("state/core/runtime.json");
        if !path.try_exists()? {
            return Ok(CoreState::Stopped {});
        }
        let manifest = self.load()?;
        let record: Record = store::decode(&store::read_protected(
            &path,
            &manifest.owner_sid,
            RECORD_LIMIT,
        )?)?;
        if record.schema_version != 1 || record.store_id != manifest.store_id {
            return Err(Error::Invalid("CORE_STATE_OWNER_MISMATCH"));
        }
        validate_state(&record.state, &manifest.owner_sid)?;
        Ok(record.state)
    }

    /// Caller must serialize lifecycle work and establish process identity before
    /// recording Running, or confirmed absence/termination before Stopped.
    /// A Starting record without an identity is unresolved, never auto-replayed.
    pub fn transition_core_state(&mut self, expected: &CoreState, next: CoreState) -> Result<()> {
        if self.core_state()? != *expected {
            return Err(Error::Invalid("CORE_STATE_CHANGED"));
        }
        let manifest = self.load()?;
        validate_state(&next, &manifest.owner_sid)?;
        let valid = match (expected, &next) {
            (CoreState::Stopped {} | CoreState::Down { .. }, CoreState::Starting { .. }) => true,
            (CoreState::Starting { generation: a }, CoreState::Running { generation: b, .. }) => {
                a == b
            }
            (
                CoreState::Starting { generation: a } | CoreState::Running { generation: a, .. },
                CoreState::Down { generation: b },
            ) => a == b,
            (
                CoreState::Starting { .. } | CoreState::Running { .. } | CoreState::Down { .. },
                CoreState::Stopped {},
            ) => true,
            _ => false,
        };
        if !valid {
            return Err(Error::Invalid("INVALID_CORE_STATE_TRANSITION"));
        }
        let _pins = self.core_directories(true)?;
        if let CoreState::Starting { generation }
        | CoreState::Running { generation, .. }
        | CoreState::Down { generation } = &next
        {
            self.open_core_generation(*generation)?;
        }
        self.replace_bounded(
            "state/core/runtime.json",
            &store::encode(
                &Record {
                    schema_version: 1,
                    store_id: manifest.store_id,
                    state: next,
                },
                RECORD_LIMIT,
            )?,
            RECORD_LIMIT,
        )
    }
}

fn validate_state(state: &CoreState, owner: &str) -> Result<()> {
    match state {
        CoreState::Starting { generation }
        | CoreState::Running { generation, .. }
        | CoreState::Down { generation }
            if generation.is_nil() =>
        {
            Err(Error::Invalid("INVALID_CORE_GENERATION"))
        }
        CoreState::Running { process, .. }
            if process.pid == 0
                || process.creation_time == 0
                || process.user_sid != owner
                || !process.image_path.is_absolute() =>
        {
            Err(Error::Invalid("INVALID_CORE_PROCESS_IDENTITY"))
        }
        _ => Ok(()),
    }
}
