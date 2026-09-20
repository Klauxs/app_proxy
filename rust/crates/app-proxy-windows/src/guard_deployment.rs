//! Immutable, administrator-owned helper generations. Staging never activates
//! a scheduled task. Callers must verify those integrations separately before
//! reporting Guard active; failed stages are retained for explicit maintenance.
use crate::{Error, Result, identity, installation, storage_security};
use app_proxy_core::{FileIdentity, ProcessIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::*;

mod event_journal;
mod listener_install;
mod security;

const FORMAT: &str = "app-proxy-rust-guard-deployment";
const PROTOCOL: u32 = 1;
const HOST: &str = "app-proxy-host.exe";
const RECORD: &str = "deployment.json";
const IMAGE_LIMIT: u64 = 256 * 1024 * 1024;
const RECORD_LIMIT: u64 = 32 * 1024;

/// Captured before UAC while the ordinary caller pins the selected host. Pass
/// this value as immutable elevation arguments, not a reread writable request.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceExpectation {
    image: FileIdentity,
    size: u64,
    sha256: String,
}

/// The foreground caller retains this until elevated installation terminates,
/// including while the consent UI is open. Parent pins prevent path replacement.
pub struct InstallerSource {
    source: Source,
}
impl InstallerSource {
    pub fn capture() -> Result<Self> {
        identity::assert_ordinary_user()?;
        let current = identity::current()?;
        let path = current
            .image_path
            .parent()
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?
            .join(HOST);
        let expected = identity::file_identity(&path)?;
        Ok(Self {
            source: Source::open(&path, &expected)?,
        })
    }
    pub fn path(&self) -> &Path {
        &self.source.path
    }
    pub fn expectation(&self) -> SourceExpectation {
        SourceExpectation {
            image: self.source.image.clone(),
            size: self.source.size,
            sha256: self.source.hash.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: String,
    protocol: u32,
    store_id: Uuid,
    generation: Uuid,
    owner_sid: String,
    coordinator_path: PathBuf,
    coordinator_image: FileIdentity,
    host_image: FileIdentity,
    image_size: u64,
    image_sha256: String,
}

/// Holds the protected files/directories against replacement. Image identities
/// passed to event_pipe remain backed by these handles for the connection life.
pub struct Deployment {
    record: Record,
    host_path: PathBuf,
    _host: File,
    _record: File,
    _listener_record: Option<File>,
    _directories: Vec<OwnedHandle>,
}
impl Deployment {
    pub(crate) fn event_trace(&self) -> Result<event_journal::EventTrace<'_>> {
        identity::assert_elevated_user()?;
        let current = identity::current()?;
        if current.image_file != *self.host_image() {
            return Err(Error::Invalid("GUARD_LISTENER_IMAGE_MISMATCH"));
        }
        event_journal::EventTrace::start(self, &current)
    }
    pub fn generation(&self) -> Uuid {
        self.record.generation
    }
    pub fn store_id(&self) -> Uuid {
        self.record.store_id
    }
    pub fn host_path(&self) -> &Path {
        &self.host_path
    }
    pub fn host_image(&self) -> &FileIdentity {
        &self.record.host_image
    }

    /// Ordinary portable coordinator is also pinned and hashed, but is never
    /// copied from this stored path or executed with elevation.
    pub fn coordinator(&self) -> Result<CoordinatorImage> {
        let source = Source::open(
            &self.record.coordinator_path,
            &self.record.coordinator_image,
        )?;
        if source.size != self.record.image_size || source.hash != self.record.image_sha256 {
            return Err(Error::Invalid("GUARD_COORDINATOR_CHANGED"));
        }
        Ok(CoordinatorImage { source })
    }

    /// Read only. IDs are names, not authorization; every protected object and
    /// record binding is revalidated. Does not accept caller-selected directories.
    pub fn open(store_id: Uuid, generation: Uuid) -> Result<Self> {
        ids(store_id, generation)?;
        let sid = identity::current()?.user_sid;
        let (root, directories) = location(&sid, store_id, false)?;
        Self::open_at(&root, directories, &sid, store_id, generation)
    }

    fn open_at(
        root: &Path,
        mut directories: Vec<OwnedHandle>,
        sid: &str,
        store: Uuid,
        generation: Uuid,
    ) -> Result<Self> {
        let root = root.join(generation.to_string());
        directories.push(security::directory(&root, false)?);
        let mut record_file = security::read_file(&root.join(RECORD))?;
        if record_file.metadata()?.len() > RECORD_LIMIT {
            return Err(Error::Invalid("GUARD_RECORD_SIZE"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut record_file)
            .take(RECORD_LIMIT + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > RECORD_LIMIT {
            return Err(Error::Invalid("GUARD_RECORD_SIZE"));
        }
        let record: Record =
            serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("INVALID_GUARD_RECORD"))?;
        validate_record(&record, sid, store, generation)?;
        let host_path = root.join(HOST);
        let mut host = security::read_file(&host_path)?;
        let (_, image) = installation::inspect_file(&host)?;
        if image != record.host_image
            || host.metadata()?.len() != record.image_size
            || hash(&mut host)? != record.image_sha256
        {
            return Err(Error::Invalid("GUARD_HOST_CHANGED"));
        }
        Ok(Self {
            record,
            host_path,
            _host: host,
            _record: record_file,
            _listener_record: None,
            _directories: directories,
        })
    }
}

pub struct CoordinatorImage {
    source: Source,
}
impl CoordinatorImage {
    pub fn path(&self) -> &Path {
        &self.source.path
    }
    pub fn image(&self) -> &FileIdentity {
        &self.source.image
    }
}

/// Called only by the elevated copy of this release's host. Copies its own
/// retained executable, never a path from a request/manifest. The ordinary
/// issuer must still be the exact same user/session process that requested UAC.
pub fn stage(
    store_id: Uuid,
    issuer: &ProcessIdentity,
    expected: &SourceExpectation,
) -> Result<Deployment> {
    identity::assert_elevated_user()?;
    let current = identity::current()?;
    if store_id.is_nil() {
        return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
    }
    let issuer_handle = verify_issuer(&current, issuer)?;
    if !current
        .image_path
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case(HOST))
    {
        return Err(Error::Invalid("GUARD_HOST_EXECUTABLE_REQUIRED"));
    }
    let source = Source::expected(&current.image_path, expected)?;
    issuer_alive(&issuer_handle)?;
    let (root, directories) = location(&current.user_sid, store_id, true)?;
    let result = stage_source(store_id, source, &current.user_sid, root, directories)?;
    issuer_alive(&issuer_handle)?;
    Ok(result)
}

fn stage_source(
    store_id: Uuid,
    source: Source,
    sid: &str,
    root: PathBuf,
    directories: Vec<OwnedHandle>,
) -> Result<Deployment> {
    let generation = Uuid::new_v4();
    let destination = root.join(generation.to_string());
    let generation_pin = security::create_generation(&destination)?;
    let record = copy_generation(source, &destination, sid, store_id, generation)?;
    let mut output = security::new_file(&destination.join(RECORD))?;
    let bytes = serde_json::to_vec(&record).map_err(|_| Error::Invalid("INVALID_GUARD_RECORD"))?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(Error::Invalid("GUARD_RECORD_SIZE"));
    }
    output.write_all(&bytes)?;
    output.sync_all()?;
    drop(output);
    // No current-generation pointer or task is switched here. Only a complete,
    // re-read protected generation can be supplied to the integration transaction.
    let result = Deployment::open_at(&root, directories, sid, store_id, generation)?;
    drop(generation_pin);
    Ok(result)
}

fn verify_issuer(current: &ProcessIdentity, issuer: &ProcessIdentity) -> Result<OwnedHandle> {
    if current.user_sid != issuer.user_sid || current.session_id != issuer.session_id {
        return Err(Error::Invalid("GUARD_INSTALL_CONTEXT_MISMATCH"));
    }
    let handle = identity::open(
        issuer.pid,
        windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
    )?;
    // SAFETY: owned query handle persists throughout both token and identity checks.
    unsafe {
        identity::assert_ordinary_handle(handle.as_raw_handle())?;
        if identity::inspect_handle(handle.as_raw_handle())? != *issuer {
            return Err(Error::IdentityMismatch);
        }
    }
    issuer_alive(&handle)?;
    Ok(handle)
}
fn issuer_alive(handle: &OwnedHandle) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT},
        System::Threading::WaitForSingleObject,
    };
    // SAFETY: retained process handle includes SYNCHRONIZE; zero timeout never waits.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } {
        WAIT_TIMEOUT => Ok(()),
        WAIT_OBJECT_0 => Err(Error::Invalid("GUARD_INSTALL_ISSUER_EXITED")),
        _ => Err(crate::last_error("CheckGuardIssuer")),
    }
}

fn copy_generation(
    mut source: Source,
    destination: &Path,
    sid: &str,
    store: Uuid,
    generation: Uuid,
) -> Result<Record> {
    let mut host = security::new_file(&destination.join(HOST))?;
    source.file.seek(SeekFrom::Start(0))?;
    if std::io::copy(&mut source.file, &mut host)? != source.size {
        return Err(Error::Invalid("GUARD_SOURCE_CHANGED"));
    }
    host.sync_all()?;
    if hash(&mut host)? != source.hash {
        return Err(Error::Invalid("GUARD_COPY_MISMATCH"));
    }
    let (_, host_image) = installation::inspect_file(&host)?;
    Ok(Record {
        format: FORMAT.into(),
        protocol: PROTOCOL,
        store_id: store,
        generation,
        owner_sid: sid.into(),
        coordinator_path: source.path,
        coordinator_image: source.image,
        host_image,
        image_size: source.size,
        image_sha256: source.hash,
    })
}

struct Source {
    file: File,
    path: PathBuf,
    image: FileIdentity,
    hash: String,
    size: u64,
    _directories: Vec<OwnedHandle>,
}
impl Source {
    fn expected(path: &Path, expected: &SourceExpectation) -> Result<Self> {
        let source = Self::open(path, &expected.image)?;
        if source.size != expected.size || source.hash != expected.sha256 {
            return Err(Error::Invalid("GUARD_SOURCE_CHANGED"));
        }
        Ok(source)
    }
    fn open(path: &Path, expected: &FileIdentity) -> Result<Self> {
        local_exe(path)?;
        let parents: Vec<_> = path
            .parent()
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?
            .ancestors()
            .collect();
        let mut directories = Vec::new();
        for parent in parents.into_iter().rev() {
            directories.push(storage_security::directory(parent, false)?);
        }
        let mut file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let metadata = file.metadata()?;
        use std::os::windows::fs::MetadataExt;
        if !metadata.is_file() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Invalid("GUARD_SOURCE_FILE_REQUIRED"));
        }
        let (path, image) = installation::inspect_file(&file)?;
        local_exe(&path)?;
        if &image != expected {
            return Err(Error::Invalid("GUARD_SOURCE_CHANGED"));
        }
        let hash = hash(&mut file)?;
        Ok(Self {
            file,
            path,
            image,
            hash,
            size: metadata.len(),
            _directories: directories,
        })
    }
}

fn local_exe(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || !matches!(path.components().next(), Some(Component::Prefix(p))
        if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || path.components().any(|c| matches!(c, Component::ParentDir))
        || !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
        || path.to_str().is_none_or(|p| p.contains('\0'))
    {
        return Err(Error::Invalid("GUARD_LOCAL_EXE_REQUIRED"));
    }
    Ok(())
}
fn hash(file: &mut File) -> Result<String> {
    let size = file.metadata()?.len();
    if size == 0 || size > IMAGE_LIMIT {
        return Err(Error::Invalid("GUARD_IMAGE_SIZE"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > IMAGE_LIMIT {
            return Err(Error::Invalid("GUARD_IMAGE_SIZE"));
        }
        hash.update(&buffer[..count]);
    }
    if total != size {
        return Err(Error::Invalid("GUARD_SOURCE_CHANGED"));
    }
    Ok(format!("{:x}", hash.finalize()))
}
fn ids(store: Uuid, generation: Uuid) -> Result<()> {
    if store.is_nil() || generation.is_nil() {
        return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
    }
    Ok(())
}
fn validate_record(record: &Record, sid: &str, store: Uuid, generation: Uuid) -> Result<()> {
    ids(store, generation)?;
    if record.format != FORMAT
        || record.protocol != PROTOCOL
        || record.store_id != store
        || record.generation != generation
        || record.owner_sid != sid
        || record.image_size == 0
        || record.image_size > IMAGE_LIMIT
        || record.image_sha256.len() != 64
        || !record
            .image_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid("GUARD_DEPLOYMENT_MISMATCH"));
    }
    local_exe(&record.coordinator_path)
}

fn location(sid: &str, store: Uuid, create: bool) -> Result<(PathBuf, Vec<OwnedHandle>)> {
    if store.is_nil() {
        return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
    }
    let mut path = program_files()?;
    // Program Files itself uses Windows' existing ACL/owner; do not alter it.
    let mut directories = vec![storage_security::directory(&path, false)?];
    let user = format!("{:x}", Sha256::digest(sid.as_bytes()));
    for name in ["AppProxyRust", "Guard", &user[..16], &store.to_string()] {
        path.push(name);
        directories.push(security::directory(&path, create)?);
    }
    Ok((path, directories))
}

fn program_files() -> Result<PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_ProgramFiles, SHGetKnownFolderPath},
    };
    // SAFETY: fixed machine known folder, not an environment override. Windows
    // allocates its UTF-16 result; released on success and conversion failure.
    unsafe {
        let mut value = std::ptr::null_mut();
        let status =
            SHGetKnownFolderPath(&FOLDERID_ProgramFiles, 0, std::ptr::null_mut(), &mut value);
        if status < 0 {
            return Err(Error::Windows {
                operation: "GetGuardProgramFiles",
                code: status as u32,
            });
        }
        let mut length = 0;
        while *value.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(value, length));
        CoTaskMemFree(value.cast());
        let path = PathBuf::from(result.map_err(|_| Error::Invalid("GUARD_PROGRAM_FILES_PATH"))?);
        if !path.is_absolute() {
            return Err(Error::Invalid("GUARD_PROGRAM_FILES_PATH"));
        }
        Ok(path)
    }
}

#[cfg(test)]
mod tests;
