//! Native Shell Links. Callers must journal the intended path/spec/bytes before
//! publication and retain the receipt before reporting an integration complete.
//! This layer never resolves or executes a link and never replaces an existing file.
use crate::{Error, Result, identity, last_error, process, storage_security as security, wide};
use app_proxy_core::FileIdentity;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    path::{Component, Path, PathBuf, Prefix},
};
use uuid::Uuid;
use windows::{
    Win32::{
        System::Com::*,
        UI::{Shell::*, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::{Interface, PCWSTR},
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_READONLY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
};

const LIMIT: usize = 1024 * 1024;
pub mod icons;
pub mod journal;
pub mod staging;
const TRACKING_DISABLED: u32 = (SLDF_FORCE_NO_LINKTRACK.0
    | SLDF_DISABLE_LINK_PATH_TRACKING.0
    | SLDF_DISABLE_KNOWNFOLDER_RELATIVE_TRACKING.0
    | SLDF_NO_KF_ALIAS.0
    | SLDF_NO_PIDL_ALIAS.0) as u32;
const ALLOWED_FLAGS: u32 = TRACKING_DISABLED
    | (SLDF_HAS_ID_LIST.0
        | SLDF_HAS_LINK_INFO.0
        | SLDF_HAS_NAME.0
        | SLDF_HAS_WORKINGDIR.0
        | SLDF_HAS_ARGS.0
        | SLDF_HAS_ICONLOCATION.0
        | SLDF_UNICODE.0) as u32;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Spec {
    pub store_id: Uuid,
    pub instance_id: Uuid,
    pub home: PathBuf,
    pub host: PathBuf,
    pub icon: PathBuf,
}
impl Spec {
    fn validate(&self) -> Result<()> {
        if self.store_id.is_nil()
            || self.instance_id.is_nil()
            || !self
                .host
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("app-proxy-host.exe"))
            || !self
                .icon
                .extension()
                .is_some_and(|n| n.eq_ignore_ascii_case("ico"))
        {
            return Err(Error::Invalid("SHORTCUT_SPEC_INVALID"));
        }
        for path in [&self.home, &self.host, &self.icon] {
            absolute(path)?;
        }
        if [&self.host, &self.icon]
            .iter()
            .any(|p| p.as_os_str().to_string_lossy().contains('%'))
        {
            return Err(Error::Invalid("SHORTCUT_ENVIRONMENT_PATH"));
        }
        Ok(())
    }
    fn description(&self) -> String {
        format!("AppProxy:v1:{}:{}", self.store_id, self.instance_id)
    }
    pub fn arguments(&self) -> Result<String> {
        self.validate()?;
        let home = process::quote_windows_word(self.home.as_os_str())?;
        let arguments = format!(
            "launch {} --home {} --notify",
            self.instance_id,
            String::from_utf16(&home).map_err(|_| Error::Invalid("SHORTCUT_PATH_INVALID"))?
        );
        let target = process::quote_windows_word(self.host.as_os_str())?;
        if target.len() + 1 + arguments.encode_utf16().count() + 1 > 32767 {
            return Err(Error::Invalid("SHORTCUT_COMMAND_TOO_LONG"));
        }
        Ok(arguments)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub file: FileIdentity,
    pub sha256: [u8; 32],
}

pub fn filename(name: &str, instance: Uuid) -> Result<String> {
    if instance.is_nil() {
        return Err(Error::Invalid("SHORTCUT_INSTANCE_REQUIRED"));
    }
    Ok(format!(
        "{} - {}.lnk",
        filename_stem(name),
        &instance.to_string()[..8]
    ))
}

pub fn original_filename(application_name: &str) -> Result<String> {
    let name = filename_stem(application_name);
    let device = name.split('.').next().unwrap().to_ascii_uppercase();
    if matches!(device.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            device.strip_prefix(prefix).is_some_and(|n| {
                matches!(
                    n,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
    {
        return Err(Error::Invalid("SHORTCUT_PATH_INVALID"));
    }
    Ok(format!("{name}.lnk"))
}

fn filename_stem(name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| !c.is_control() && !"<>:\"/\\|?*".contains(*c))
        .take(64)
        .collect();
    let name = name.trim_matches([' ', '.']);
    if name.is_empty() { "实例" } else { name }.into()
}

/// Read only; creating links on the real desktop is a separate foreground action.
pub fn desktop() -> Result<PathBuf> {
    identity::assert_ordinary_user()?;
    // SAFETY: a fixed known-folder ID and no alternate token; copy/free Shell allocation.
    unsafe {
        let value =
            SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).map_err(com_error)?;
        let path = value
            .to_string()
            .map(PathBuf::from)
            .map_err(|_| Error::Invalid("SHORTCUT_PATH_INVALID"));
        CoTaskMemFree(Some(value.0.cast()));
        path
    }
}

fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute() || !matches!(path.components().next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
        || path.to_str().is_none_or(|s| s.contains('\0') || s.len() > 32760)
        || path.components().any(|c| matches!(c, Component::Normal(n) if n.to_string_lossy().contains([':', '"', '*', '?']) || n.to_string_lossy().ends_with([' ', '.']))) {
        return Err(Error::Invalid("SHORTCUT_PATH_INVALID"));
    }
    Ok(())
}

// Rust's current_exe/canonicalize can return a verbatim drive path. Shell Link
// setters reject that spelling with E_INVALIDARG. Normalize only at the Shell
// boundary; keep the journal's original spec for ownership and crash recovery.
fn shell_path(path: &Path) -> Result<PathBuf> {
    absolute(path)?;
    let text = path
        .to_str()
        .ok_or(Error::Invalid("SHORTCUT_PATH_INVALID"))?;
    Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text)))
}

/// Encode before external publication, so the caller can persist recovery evidence.
pub fn encode(spec: &Spec) -> Result<Vec<u8>> {
    identity::assert_ordinary_user()?;
    spec.validate()?;
    let session = Session::new()?;
    let host = shell_path(&spec.host)?;
    let icon_path = shell_path(&spec.icon)?;
    let target = wide(host.as_os_str())?;
    let cwd = wide(
        host.parent()
            .ok_or(Error::Invalid("SHORTCUT_PATH_INVALID"))?
            .as_os_str(),
    )?;
    let args = wide(std::ffi::OsStr::new(&spec.arguments()?))?;
    let description = wide(std::ffi::OsStr::new(&spec.description()))?;
    let icon = wide(icon_path.as_os_str())?;
    // SAFETY: all buffers are terminated; COM interfaces remain on this thread
    // and are released before Session's apartment. No Resolve or execution call.
    let bytes = unsafe {
        session
            .link
            .SetPath(PCWSTR(target.as_ptr()))
            .map_err(com_error)?;
        session
            .link
            .SetArguments(PCWSTR(args.as_ptr()))
            .map_err(com_error)?;
        session
            .link
            .SetWorkingDirectory(PCWSTR(cwd.as_ptr()))
            .map_err(com_error)?;
        session
            .link
            .SetDescription(PCWSTR(description.as_ptr()))
            .map_err(com_error)?;
        session
            .link
            .SetIconLocation(PCWSTR(icon.as_ptr()), 0)
            .map_err(com_error)?;
        session.link.SetShowCmd(SW_SHOWNORMAL).map_err(com_error)?;
        session.link.SetHotkey(0).map_err(com_error)?;
        let data: IShellLinkDataList = session.link.cast().map_err(com_error)?;
        data.SetFlags((data.GetFlags().map_err(com_error)? & ALLOWED_FLAGS) | TRACKING_DISABLED)
            .map_err(com_error)?;
        let persist: IPersistStream = session.link.cast().map_err(com_error)?;
        let stream = SHCreateMemStream(None).ok_or(Error::Invalid("SHORTCUT_STREAM_FAILED"))?;
        persist.Save(&stream, true).map_err(com_error)?;
        let mut stat = STATSTG::default();
        stream.Stat(&mut stat, STATFLAG_NONAME).map_err(com_error)?;
        if stat.cbSize == 0 || stat.cbSize > LIMIT as u64 {
            return Err(Error::Invalid("SHORTCUT_SIZE_INVALID"));
        }
        stream.Seek(0, STREAM_SEEK_SET, None).map_err(com_error)?;
        let mut bytes = vec![0; stat.cbSize as usize];
        let mut read = 0;
        stream
            .Read(
                bytes.as_mut_ptr().cast(),
                bytes.len() as u32,
                Some(&mut read),
            )
            .ok()
            .map_err(com_error)?;
        if read as usize != bytes.len() {
            return Err(Error::Invalid("SHORTCUT_STREAM_TRUNCATED"));
        }
        bytes
    };
    inspect_bytes(spec, &bytes)?;
    Ok(bytes)
}

pub fn publish(path: &Path, spec: &Spec, bytes: &[u8]) -> Result<Receipt> {
    identity::assert_ordinary_user()?;
    let _directory = parent(path)?;
    if std::fs::symlink_metadata(path).is_ok() {
        return Err(Error::Invalid("SHORTCUT_PATH_OCCUPIED"));
    }
    inspect_bytes(spec, bytes)?;
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    let expected = file_id(&information(temporary.as_file())?);
    // No overwrite even if another creator wins after the check above.
    let published = temporary
        .persist_noclobber(path)
        .map_err(|_| Error::Invalid("SHORTCUT_PUBLISH_UNCONFIRMED"))?;
    drop(published);
    let (file, actual, data) = read_pinned(path, false)?;
    if actual != expected || data != bytes {
        return Err(Error::Invalid("SHORTCUT_PUBLISH_UNCONFIRMED"));
    }
    drop(file);
    let receipt = Receipt {
        file: actual,
        sha256: Sha256::digest(bytes).into(),
    };
    notify(path);
    Ok(receipt)
}

pub fn verify(path: &Path, spec: &Spec, receipt: &Receipt) -> Result<()> {
    identity::assert_ordinary_user()?;
    let _directory = parent(path)?;
    let (_file, actual, bytes) = read_pinned(path, false)?;
    verify_bytes(spec, receipt, &actual, &bytes)
}

/// Missing is meaningful only after the entire parent chain was verified and
/// pinned. An inaccessible/moved parent is uncertainty, not a deleted shortcut.
fn present(path: &Path, spec: &Spec, receipt: &Receipt) -> Result<bool> {
    let _parents = parent(path)?;
    match read_pinned(path, false) {
        Ok((_file, actual, bytes)) => {
            verify_bytes(spec, receipt, &actual, &bytes)?;
            Ok(true)
        }
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Mark precisely the verified file for deletion; never delete through a new
/// path lookup. Caller keeps its journal until this returns confirmed success.
pub fn remove(path: &Path, spec: &Spec, receipt: &Receipt) -> Result<()> {
    identity::assert_ordinary_user()?;
    let _directory = parent(path)?;
    remove_verified(path, spec, receipt)
}
fn remove_verified(path: &Path, spec: &Spec, receipt: &Receipt) -> Result<()> {
    let (file, actual, bytes) = read_pinned(path, true)?;
    verify_bytes(spec, receipt, &actual, &bytes)?;
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    loop {
        // SAFETY: same pinned READ|DELETE handle used for identity, bytes and COM
        // verification. No write/delete sharing allows replacement while waiting.
        if unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileDispositionInfo,
                (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                std::mem::size_of_val(&disposition) as u32,
            )
        } != 0
        {
            break;
        }
        let error = last_error("ShortcutDelete");
        // A mapped reader can deny disposition after DELETE access was granted.
        // Retry only on this already-authorized handle; never clear attributes,
        // reopen a replacement, or repeat a successful disposition.
        if !matches!(error, Error::Windows { code: 5 | 32, .. })
            || std::time::Instant::now() >= deadline
            || information(&file)?.dwFileAttributes & FILE_ATTRIBUTE_READONLY != 0
        {
            return Err(error);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    drop(file);
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            notify(path);
            Ok(())
        }
        _ => Err(Error::Invalid("SHORTCUT_REMOVE_UNCONFIRMED")),
    }
}

fn parent(path: &Path) -> Result<Vec<File>> {
    absolute(path)?;
    if !path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        return Err(Error::Invalid("SHORTCUT_EXTENSION_REQUIRED"));
    }
    pin_parents(path)
}
fn pin_parents(path: &Path) -> Result<Vec<File>> {
    absolute(path)?;
    let parent = path
        .parent()
        .ok_or(Error::Invalid("SHORTCUT_PARENT_REQUIRED"))?;
    let mut directories = Vec::new();
    for directory in parent.ancestors().collect::<Vec<_>>().into_iter().rev() {
        directories.push(pin_directory(directory, || {})?);
    }
    Ok(directories)
}
fn pin_directory(path: &Path, after_check: impl FnOnce()) -> Result<File> {
    security::no_reparse(path)?;
    after_check();
    let directory = OpenOptions::new()
        .read(true)
        .access_mode(FILE_GENERIC_READ)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    let info = information(&directory)?;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT)
        != FILE_ATTRIBUTE_DIRECTORY
    {
        return Err(Error::Invalid("SHORTCUT_PARENT_INVALID"));
    }
    Ok(directory)
}
fn read_pinned(path: &Path, delete: bool) -> Result<(File, FileIdentity, Vec<u8>)> {
    // Shell/AV readers may briefly hold a freshly published or edited link.
    // Retry only handle acquisition, before any read/delete side effect; each
    // attempt still checks reparse points and uses the same restrictive sharing.
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    let mut file = loop {
        security::no_reparse(path)?;
        match OpenOptions::new()
            .read(true)
            .access_mode(FILE_GENERIC_READ | if delete { DELETE } else { 0 })
            .share_mode(FILE_SHARE_READ)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(file) => break file,
            Err(error)
                if error.raw_os_error() == Some(32) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(error) if error.raw_os_error() == Some(32) => {
                return Err(Error::Invalid("SHORTCUT_FILE_BUSY"));
            }
            Err(error) => return Err(error.into()),
        }
    };
    let info = information(&file)?;
    let length = ((info.nFileSizeHigh as u64) << 32) | info.nFileSizeLow as u64;
    if info.dwFileAttributes & (FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT) != 0
        || info.nNumberOfLinks != 1
        || length == 0
        || length > LIMIT as u64
    {
        return Err(Error::Invalid("SHORTCUT_FILE_INVALID"));
    }
    let id = file_id(&info);
    let mut bytes = Vec::with_capacity(length as usize);
    (&mut file).take(LIMIT as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 != length {
        return Err(Error::Invalid("SHORTCUT_FILE_CHANGED"));
    }
    Ok((file, id, bytes))
}
fn information(file: &File) -> Result<BY_HANDLE_FILE_INFORMATION> {
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: live File handle, correctly sized writable structure.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(last_error("ShortcutIdentity"));
    }
    Ok(info)
}
fn file_id(info: &BY_HANDLE_FILE_INFORMATION) -> FileIdentity {
    FileIdentity {
        volume_serial: info.dwVolumeSerialNumber,
        file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    }
}
fn verify_bytes(spec: &Spec, receipt: &Receipt, actual: &FileIdentity, bytes: &[u8]) -> Result<()> {
    if *actual != receipt.file || <[u8; 32]>::from(Sha256::digest(bytes)) != receipt.sha256 {
        return Err(Error::Invalid("SHORTCUT_CHANGED"));
    }
    inspect_bytes(spec, bytes)
}

fn inspect_bytes(spec: &Spec, bytes: &[u8]) -> Result<()> {
    spec.validate()?;
    if bytes.is_empty() || bytes.len() > LIMIT {
        return Err(Error::Invalid("SHORTCUT_SIZE_INVALID"));
    }
    let session = Session::new()?;
    // SAFETY: bounded immutable input copied into a COM memory stream. Reading
    // Shell Link fields never calls Resolve, ShellExecute, or loads its target.
    unsafe {
        let stream =
            SHCreateMemStream(Some(bytes)).ok_or(Error::Invalid("SHORTCUT_STREAM_FAILED"))?;
        let persist: IPersistStream = session.link.cast().map_err(com_error)?;
        persist.Load(&stream).map_err(com_error)?;
        let path = get_text(|buf| {
            session
                .link
                .GetPath(buf, std::ptr::null_mut(), SLGP_RAWPATH.0 as u32)
        })?;
        let args = get_text(|buf| session.link.GetArguments(buf))?;
        let cwd = get_text(|buf| session.link.GetWorkingDirectory(buf))?;
        let description = get_text(|buf| session.link.GetDescription(buf))?;
        let mut index = 0;
        let icon = get_text(|buf| session.link.GetIconLocation(buf, &mut index))?;
        let data: IShellLinkDataList = session.link.cast().map_err(com_error)?;
        let flags = data.GetFlags().map_err(com_error)?;
        let host = shell_path(&spec.host)?;
        if Path::new(&path) != host
            || args != spec.arguments()?
            || Path::new(&cwd) != host.parent().unwrap()
            || description != spec.description()
            || Path::new(&icon) != shell_path(&spec.icon)?
            || index != 0
            || flags & !ALLOWED_FLAGS != 0
            || flags & TRACKING_DISABLED != TRACKING_DISABLED
            || session.link.GetHotkey().map_err(com_error)? != 0
            || session.link.GetShowCmd().map_err(com_error)? != SW_SHOWNORMAL
        {
            return Err(Error::Invalid("SHORTCUT_CONTENT_CONFLICT"));
        }
    }
    Ok(())
}
fn get_text(read: impl FnOnce(&mut [u16]) -> windows::core::Result<()>) -> Result<String> {
    let mut buffer = vec![0xffff; 32768];
    read(&mut buffer).map_err(com_error)?;
    let end = buffer
        .iter()
        .position(|c| *c == 0)
        .filter(|n| *n < buffer.len() - 1)
        .ok_or(Error::Invalid("SHORTCUT_TEXT_TRUNCATED"))?;
    String::from_utf16(&buffer[..end]).map_err(|_| Error::Invalid("SHORTCUT_TEXT_INVALID"))
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        /* SAFETY: paired on this synchronous thread. */
        unsafe { CoUninitialize() }
    }
}
struct Session {
    link: IShellLinkW,
    _apartment: Apartment,
}
impl Session {
    fn new() -> Result<Self> {
        // SAFETY: synchronous caller, no interfaces cross threads or await points.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(com_error)?;
            let apartment = Apartment;
            let link =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(com_error)?;
            Ok(Self {
                link,
                _apartment: apartment,
            })
        }
    }
}
fn com_error(error: windows::core::Error) -> Error {
    Error::Windows {
        operation: "ShortcutCom",
        code: error.code().0 as u32,
    }
}
fn notify(path: &Path) {
    if let Ok(path) = wide(path.as_os_str()) {
        // SAFETY: terminated absolute path remains live for this synchronous hint.
        unsafe {
            SHChangeNotify(
                SHCNE_UPDATEITEM,
                SHCNF_PATHW,
                Some(path.as_ptr().cast()),
                None,
            )
        }
    }
}

#[cfg(test)]
mod tests;
