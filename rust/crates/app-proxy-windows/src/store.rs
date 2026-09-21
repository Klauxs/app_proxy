//! Single-owner configuration store. Opening never repairs or replaces broken data.
use crate::{Error, Result, identity, storage_security as security, wide};
use app_proxy_core::model::{FORMAT, MANIFEST_LIMIT, Manifest, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::null;
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::*;

const MARKER: &str = ".app-proxy-rust-owned.json";
const SECRET_LIMIT: usize = 256 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Owner {
    format: String,
    schema_version: u32,
    store_id: Uuid,
    owner_sid: String,
}

/// Read-only discovery lets clients find the owner without acquiring its write lock.
pub struct StoreDescriptor {
    pub store_id: Uuid,
    pub owner_sid: String,
}

pub fn describe(root: &Path) -> Result<StoreDescriptor> {
    identity::assert_ordinary_user()?;
    absolute(root)?;
    let sid = identity::current()?.user_sid;
    let handle = security::directory(root, false)?;
    security::verify(handle.as_raw_handle(), &sid, true)?;
    if root.join(".initializing").try_exists()? {
        return Err(Error::Invalid("STORE_INITIALIZATION_INCOMPLETE"));
    }
    let owner: Owner = decode(&read_protected(&root.join(MARKER), &sid, 4096)?)?;
    if owner.format != FORMAT
        || owner.schema_version != SCHEMA_VERSION
        || owner.store_id.is_nil()
        || owner.owner_sid != sid
    {
        return Err(Error::Invalid("STORE_HEADER_MISMATCH"));
    }
    Ok(StoreDescriptor {
        store_id: owner.store_id,
        owner_sid: owner.owner_sid,
    })
}

pub struct StartupLock {
    _file: File,
    _directory: OwnedHandle,
}

/// Short client-side lock; held only while finding/starting the coordinator.
pub fn try_startup_lock(root: &Path) -> Result<Option<StartupLock>> {
    let descriptor = describe(root)?;
    let directory = security::directory(&root.join("state"), false)?;
    security::verify(directory.as_raw_handle(), &descriptor.owner_sid, false)?;
    let path = root.join("state/startup.lock");
    if path.try_exists()? {
        security::no_reparse(&path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    security::verify(file.as_raw_handle(), &descriptor.owner_sid, false)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(StartupLock {
            _file: file,
            _directory: directory,
        })),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

pub struct Store {
    root: PathBuf,
    owner: Owner,
    _directories: Vec<OwnedHandle>,
    _lock: std::sync::Arc<File>,
    validated_secrets: RefCell<Option<ValidatedSecrets>>,
}

// Secret IDs are immutable. Retained read-only handles deny writes/deletion,
// so a successfully validated snapshot need not reopen every subscription node
// on each Guard/configuration read. No secret values are kept in this cache.
struct ValidatedSecrets {
    manifest_digest: [u8; 32],
    _pins: Vec<File>,
}

impl Store {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }
    pub(crate) fn owner_lease(&self) -> std::sync::Arc<File> {
        self._lock.clone()
    }
    pub fn create(root: &Path) -> Result<Self> {
        identity::assert_ordinary_user()?;
        absolute(root)?;
        if !root.exists() {
            let parent = root
                .parent()
                .ok_or(Error::Invalid("STORE_PARENT_REQUIRED"))?;
            security::no_reparse(parent)?;
            fs::create_dir(root)?;
        }
        let handle = security::directory(root, true)?;
        let sid = identity::current()?.user_sid;
        security::verify_owner(handle.as_raw_handle(), &sid)?;
        if fs::read_dir(root)?.next().is_some() {
            return Err(Error::Invalid("STORE_NOT_EMPTY"));
        }
        // create_new claims initialization. A crash leaves a diagnosable partial store.
        let mut initializing = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(root.join(".initializing"))?;
        initializing.write_all(b"app-proxy-rust\n")?;
        security::protect(&handle, &sid)?;
        let manifest = Manifest::empty(sid.clone());
        let owner = Owner {
            format: FORMAT.into(),
            schema_version: SCHEMA_VERSION,
            store_id: manifest.store_id,
            owner_sid: sid,
        };
        for name in ["state", "secrets", "backups"] {
            fs::create_dir(root.join(name))?;
        }
        write_new(&root.join(MARKER), &encode(&owner, 4096)?, &owner.owner_sid)?;
        write_new(
            &root.join("manifest.json"),
            &encode(&manifest, MANIFEST_LIMIT)?,
            &owner.owner_sid,
        )?;
        drop(initializing);
        fs::remove_file(root.join(".initializing"))?;
        drop(handle);
        Self::open(root)
    }

    pub fn open(root: &Path) -> Result<Self> {
        Self::open_expected(root, None)
    }

    /// Bind a saved entry to its store before replaying any accepted edits.
    pub fn open_expected(root: &Path, expected_store: Option<Uuid>) -> Result<Self> {
        identity::assert_ordinary_user()?;
        absolute(root)?;
        let sid = identity::current()?.user_sid;
        let mut directories = Vec::new();
        for name in ["", "state", "secrets", "backups"] {
            let handle = security::directory(&root.join(name), false)?;
            security::verify(handle.as_raw_handle(), &sid, name.is_empty())?;
            directories.push(handle);
        }
        if root.join(".initializing").try_exists()? {
            return Err(Error::Invalid("STORE_INITIALIZATION_INCOMPLETE"));
        }
        let owner: Owner = decode(&read_protected(&root.join(MARKER), &sid, 4096)?)?;
        if owner.format != FORMAT
            || owner.schema_version != SCHEMA_VERSION
            || owner.store_id.is_nil()
            || owner.owner_sid != sid
        {
            return Err(Error::Invalid("STORE_HEADER_MISMATCH"));
        }
        let lock_path = root.join("state/owner.lock");
        if lock_path.try_exists()? {
            security::no_reparse(&lock_path)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&lock_path)?;
        security::verify(lock.as_raw_handle(), &sid, false)?;
        lock.try_lock()
            .map_err(|_| Error::Invalid("STORE_ALREADY_OWNED"))?;
        if expected_store.is_some_and(|expected| expected != owner.store_id) {
            return Err(Error::Invalid("STORE_ID_MISMATCH"));
        }
        let mut store = Self {
            root: root.to_owned(),
            owner,
            _directories: directories,
            _lock: std::sync::Arc::new(lock),
            validated_secrets: RefCell::new(None),
        };
        store.load()?;
        store.recover_config_requests()?;
        Ok(store)
    }

    pub fn load(&self) -> Result<Manifest> {
        let _timing = crate::diagnostic_timing::Span::new("store.load", String::new);
        let manifest: Manifest = decode(&read_protected(
            &self.root.join("manifest.json"),
            &self.owner.owner_sid,
            MANIFEST_LIMIT,
        )?)?;
        self.validate(&manifest)?;
        Ok(manifest)
    }

    /// Consumes caller's snapshot. The saved revision is always assigned here.
    pub fn commit(&mut self, expected_revision: u64, manifest: Manifest) -> Result<u64> {
        self.ensure_core_update_idle()?;
        self.recover_config_requests()?;
        self.commit_snapshot(expected_revision, manifest)
    }

    pub(crate) fn commit_snapshot(
        &mut self,
        expected_revision: u64,
        mut manifest: Manifest,
    ) -> Result<u64> {
        let previous = self.load()?;
        if previous.revision != expected_revision || manifest.revision != expected_revision {
            return Err(Error::Invalid("STALE_MANIFEST_REVISION"));
        }
        manifest.revision = expected_revision
            .checked_add(1)
            .ok_or(Error::Invalid("REVISION_EXHAUSTED"))?;
        self.validate(&manifest)?;
        self.ensure_shortcut_instances(&manifest)?;
        let bytes = encode(&manifest, MANIFEST_LIMIT)?;
        // Backup replacement completes before touching the authoritative snapshot.
        self.replace(
            "backups/manifest.previous.json",
            &encode(&previous, MANIFEST_LIMIT)?,
        )?;
        self.replace("manifest.json", &bytes)?;
        Ok(manifest.revision)
    }

    pub fn put_secret(&self, value: &str) -> Result<Uuid> {
        let id = Uuid::new_v4();
        self.put_secret_once(id, value)?;
        Ok(id)
    }

    /// Stable request identity allows retry after staging but before intent.
    /// Publish a complete file without ever replacing an existing secret.
    pub(crate) fn put_secret_once(&self, id: Uuid, value: &str) -> Result<()> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_SECRET_ID"));
        }
        if value.contains('\0') {
            return Err(Error::Invalid("INVALID_SECRET_VALUE"));
        }
        let destination = self.root.join(format!("secrets/{id}.json"));
        if destination.try_exists()? {
            return if self.read_secret(id)? == value {
                Ok(())
            } else {
                Err(Error::Invalid("SECRET_ID_CONFLICT"))
            };
        }
        let bytes = encode(
            &Secret {
                value: value.to_owned(),
            },
            SECRET_LIMIT,
        )?;
        let mut temp = tempfile::NamedTempFile::new_in(self.root.join("secrets"))?;
        security::verify(temp.as_file().as_raw_handle(), &self.owner.owner_sid, false)?;
        temp.write_all(&bytes)?;
        temp.as_file().sync_all()?;
        temp.into_temp_path()
            .persist_noclobber(destination)
            .map_err(|e| Error::Io(e.error))?;
        Ok(())
    }

    pub fn read_secret(&self, id: Uuid) -> Result<String> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_SECRET_ID"));
        }
        let secret: Secret = decode(&read_protected(
            &self.root.join(format!("secrets/{id}.json")),
            &self.owner.owner_sid,
            SECRET_LIMIT,
        )?)?;
        if secret.value.contains('\0') {
            return Err(Error::Invalid("INVALID_SECRET_VALUE"));
        }
        Ok(secret.value)
    }

    pub(crate) fn validate(&self, manifest: &Manifest) -> Result<()> {
        manifest.validate().map_err(|e| Error::Invalid(e.0))?;
        if manifest.store_id != self.owner.store_id || manifest.owner_sid != self.owner.owner_sid {
            return Err(Error::Invalid("MANIFEST_OWNER_MISMATCH"));
        }
        // Hash the complete contents, not just revision: even an out-of-band
        // same-revision edit must undergo full credential validation again.
        let manifest_digest: [u8; 32] = Sha256::digest(encode(manifest, MANIFEST_LIMIT)?).into();
        if self
            .validated_secrets
            .borrow()
            .as_ref()
            .is_some_and(|cached| cached.manifest_digest == manifest_digest)
        {
            return Ok(());
        }
        let _timing = crate::diagnostic_timing::Span::new("store.validate_secrets", || {
            manifest.secret_ids().len().to_string()
        });
        let mut pins = Vec::new();
        let mut read_secret = |id: Uuid| -> Result<String> {
            let (bytes, file) = read_protected_pinned(
                &self.root.join(format!("secrets/{id}.json")),
                &self.owner.owner_sid,
                SECRET_LIMIT,
            )?;
            let secret: Secret = decode(&bytes)?;
            if secret.value.contains('\0') {
                return Err(Error::Invalid("INVALID_SECRET_VALUE"));
            }
            pins.push(file);
            Ok(secret.value)
        };
        let mut checked = std::collections::HashSet::new();
        for profile in &manifest.profiles {
            if let app_proxy_core::model::ProxySource::Subscription {
                url_secret_id,
                nodes,
                ..
            } = &profile.source
            {
                app_proxy_core::subscription::source_url(&read_secret(*url_secret_id)?)
                    .map_err(|_| Error::Invalid("SUBSCRIPTION_URL_INVALID"))?;
                checked.insert(*url_secret_id);
                for node in nodes {
                    node.resolve(&read_secret(node.secret_id)?)
                        .map_err(|_| Error::Invalid("INVALID_SUBSCRIPTION_SECRET"))?;
                    checked.insert(node.secret_id);
                }
            }
        }
        for id in manifest.secret_ids().difference(&checked) {
            read_secret(*id)?;
        }
        *self.validated_secrets.borrow_mut() = Some(ValidatedSecrets {
            manifest_digest,
            _pins: pins,
        });
        Ok(())
    }

    fn replace(&self, relative: &str, bytes: &[u8]) -> Result<()> {
        self.replace_bounded(relative, bytes, MANIFEST_LIMIT)
    }

    pub(crate) fn replace_bounded(&self, relative: &str, bytes: &[u8], limit: usize) -> Result<()> {
        replace_protected(&self.root, &self.owner.owner_sid, relative, bytes, limit)
    }
}

/// Caller retains an owned, ACL-verified root directory handle throughout this
/// replacement. Shared by the store and the per-user instance resource registry.
pub(crate) fn replace_protected(
    root: &Path,
    owner_sid: &str,
    relative: &str,
    bytes: &[u8],
    limit: usize,
) -> Result<()> {
    app_proxy_core::model::safe_relative(Path::new(relative)).map_err(|e| Error::Invalid(e.0))?;
    if bytes.len() > limit {
        return Err(Error::Invalid("STORE_FILE_TOO_LARGE"));
    }
    let destination = root.join(relative);
    if destination.try_exists()? {
        // Refuse an untrusted/reparse destination instead of replacing it silently.
        read_protected(&destination, owner_sid, limit)?;
    }
    let parent = destination
        .parent()
        .ok_or(Error::Invalid("STORE_PARENT_REQUIRED"))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    security::verify(temp.as_file().as_raw_handle(), owner_sid, false)?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    // Close the write handle before Windows rename; TempPath removes only our temp on failure.
    let temp = temp.into_temp_path();
    let source = wide(temp.as_os_str())?;
    let target = wide(destination.as_os_str())?;
    for attempt in 0..4 {
        // SAFETY: both paths are live terminated buffers on the same protected volume.
        let success = unsafe {
            if destination.try_exists()? {
                ReplaceFileW(target.as_ptr(), source.as_ptr(), null(), 0, null(), null())
            } else {
                MoveFileExW(source.as_ptr(), target.as_ptr(), MOVEFILE_WRITE_THROUGH)
            }
        };
        if success != 0 {
            return Ok(());
        }
        let error = std::io::Error::last_os_error();
        if attempt == 3 || !matches!(error.raw_os_error(), Some(5 | 32 | 33)) {
            return Err(error.into());
        }
        std::thread::sleep(std::time::Duration::from_millis(20 * (attempt + 1)));
    }
    unreachable!()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Secret {
    value: String,
}

fn absolute(root: &Path) -> Result<()> {
    if !root.is_absolute()
        || root.to_str().is_none_or(|s| s.contains('\0'))
        || root
            .components()
            .any(|p| matches!(p, std::path::Component::ParentDir))
    {
        return Err(Error::Invalid("ABSOLUTE_STORE_PATH_REQUIRED"));
    }
    Ok(())
}
pub(crate) fn encode<T: Serialize>(value: &T, limit: usize) -> Result<Vec<u8>> {
    let bytes =
        serde_json::to_vec_pretty(value).map_err(|_| Error::Invalid("STORE_SERIALIZE_FAILED"))?;
    if bytes.len() > limit {
        return Err(Error::Invalid("STORE_FILE_TOO_LARGE"));
    }
    Ok(bytes)
}
pub(crate) fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    // serde errors can contain the offending value, so only expose a stable code.
    serde_json::from_slice(bytes).map_err(|error| {
        // Only this fixed model error is safe to surface; never echo JSON values.
        if error
            .to_string()
            .starts_with("LEGACY_IFEO_CLEANUP_REQUIRED at line ")
        {
            Error::Invalid("LEGACY_IFEO_CLEANUP_REQUIRED")
        } else {
            Error::Invalid("INVALID_STORE_JSON")
        }
    })
}
pub(crate) fn write_new(path: &Path, bytes: &[u8], sid: &str) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    security::verify(file.as_raw_handle(), sid, false)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
pub(crate) fn read_protected(path: &Path, sid: &str, limit: usize) -> Result<Vec<u8>> {
    read_protected_pinned(path, sid, limit).map(|(bytes, _)| bytes)
}

fn read_protected_pinned(path: &Path, sid: &str, limit: usize) -> Result<(Vec<u8>, File)> {
    security::no_reparse(path)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid("STORE_FILE_REQUIRED"));
    }
    security::verify(file.as_raw_handle(), sid, false)?;
    let mut bytes = Vec::new();
    (&file).take(limit as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(Error::Invalid("STORE_FILE_TOO_LARGE"));
    }
    Ok((bytes, file))
}

/// User-selected advanced input may contain credentials. Read a bounded file
/// owned by this ordinary user with the same private ACL as configuration.
pub fn read_private_input(path: &Path, limit: usize) -> Result<Vec<u8>> {
    identity::assert_ordinary_user()?;
    absolute(path)?;
    read_protected(path, &identity::current()?.user_sid, limit)
}
