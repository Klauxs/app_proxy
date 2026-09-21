//! Fixed-directory, ordinary-user installation primitives. An exclusive file
//! lease requests coordinator draining; a crashed installer releases it in-kernel.
use crate::{Error, Result, identity, storage_security as security};
use std::{
    fs::{File, OpenOptions},
    io::Read,
    os::windows::{fs::OpenOptionsExt, io::OwnedHandle},
    path::{Path, PathBuf},
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const LOCK: &str = ".app-proxy-update.lock";

pub fn install_directory() -> Result<PathBuf> {
    Ok(crate::instance_data::local_app_data()?.join("Programs/AppProxy"))
}

pub fn current_directory() -> Result<PathBuf> {
    std::env::current_exe()?
        .parent()
        .map(Path::to_owned)
        .ok_or(Error::Invalid("SETUP_PROGRAM_DIRECTORY_MISSING"))
}

pub fn requested_at(root: &Path) -> Result<bool> {
    let path = root.join(LOCK);
    match OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
    {
        Ok(_) => incomplete_pair(root),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => incomplete_pair(root),
        Err(e) if e.raw_os_error() == Some(32) => Ok(true),
        Err(e) => Err(e.into()),
    }
}

fn incomplete_pair(root: &Path) -> Result<bool> {
    let path = root.join(".app-proxy-upgrade.json");
    let file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    let mut bytes = Vec::new();
    file.take(32769).read_to_end(&mut bytes)?;
    if bytes.len() > 32768 {
        return Err(Error::Invalid("SETUP_RECORD_SIZE"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    // A persisted, fully published pair may start for integration verification.
    // An interrupted half-pair remains gated even after the installer dies.
    Ok(value.get("published").and_then(serde_json::Value::as_bool) != Some(true))
}

pub fn ensure_available() -> Result<()> {
    if requested_at(&current_directory()?)? {
        Err(Error::Invalid("AppProxy 正在升级，请安装完成后再打开。"))
    } else {
        Ok(())
    }
}

pub struct Maintenance {
    file: Option<File>,
    _directories: Vec<OwnedHandle>,
}
impl Maintenance {
    pub fn acquire(root: &Path) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let mut directories = Vec::new();
        for path in root.ancestors() {
            directories.push(security::directory(path, false)?);
        }
        let path = root.join(LOCK);
        if path.try_exists()? {
            security::no_reparse(&path)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        Ok(Self {
            file: Some(file),
            _directories: directories,
        })
    }
    pub fn release_signal(&mut self) {
        self.file.take();
    }
}

/// The installer creates only its fixed user directory. Existing foreign files
/// are handled by the package ownership journal, never adopted by this helper.
pub fn prepare_directory(path: &Path) -> Result<OwnedHandle> {
    if !path.is_absolute() {
        return Err(Error::Invalid("SETUP_ABSOLUTE_PATH_REQUIRED"));
    }
    let parent = path
        .parent()
        .ok_or(Error::Invalid("SETUP_PARENT_REQUIRED"))?;
    security::no_reparse(parent)?;
    if !path.try_exists()? {
        std::fs::create_dir(path)?;
    }
    let pin = security::directory(path, true)?;
    security::protect(&pin, &identity::current()?.user_sid)?;
    Ok(pin)
}

pub fn dialog(message: &str, question: bool, error: bool) -> Result<bool> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let title = crate::wide(std::ffi::OsStr::new("AppProxy 安装程序"))?;
    let text = crate::wide(std::ffi::OsStr::new(message))?;
    let flags = if question {
        MB_YESNO | MB_ICONQUESTION
    } else if error {
        MB_OK | MB_ICONERROR
    } else {
        MB_OK | MB_ICONINFORMATION
    };
    // SAFETY: owned terminated buffers, no parent window or transferred memory.
    let result = unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            flags | MB_SETFOREGROUND,
        )
    };
    Ok(result == IDYES)
}

pub fn verify_plain_file(path: &Path) -> Result<()> {
    security::no_reparse(path)?;
    if !path.is_file() {
        return Err(Error::Invalid("SETUP_FILE_REQUIRED"));
    }
    Ok(())
}

pub fn pin_directory(path: &Path) -> Result<OwnedHandle> {
    security::directory(path, false)
}

pub fn open_frontend(path: &Path) -> Result<()> {
    identity::assert_ordinary_user()?;
    verify_plain_file(path)?;
    let file = crate::wide(path.as_os_str())?;
    let parent = crate::wide(
        path.parent()
            .ok_or(Error::Invalid("SETUP_PARENT_REQUIRED"))?
            .as_os_str(),
    )?;
    // SAFETY: an exact verified installed executable, no command string or
    // elevation verb. Shell creates the interactive frontend's own console.
    let result = unsafe {
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            file.as_ptr(),
            std::ptr::null(),
            parent.as_ptr(),
            windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL,
        )
    };
    if result as isize <= 32 {
        return Err(Error::Invalid("SETUP_OPEN_FRONTEND_FAILED"));
    }
    Ok(())
}

pub struct Progress {
    dialog: Option<windows::Win32::UI::Shell::IProgressDialog>,
}
impl Progress {
    pub fn open() -> Result<Self> {
        use windows::Win32::{System::Com::*, UI::Shell::*};
        let map = |e: windows::core::Error| Error::Windows {
            operation: "SetupProgress",
            code: e.code().0 as u32,
        };
        // SAFETY: ordinary foreground thread; COM is balanced by Drop on this
        // same thread. No interface is sent to a worker.
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED)
                .ok()
                .map_err(map)?;
            let mut progress = Self { dialog: None };
            progress.dialog = Some(
                CoCreateInstance(&CLSID_ProgressDialog, None, CLSCTX_INPROC_SERVER).map_err(map)?,
            );
            let dialog = progress.dialog.as_ref().unwrap();
            dialog
                .SetTitle(windows::core::w!("AppProxy 安装程序"))
                .map_err(map)?;
            dialog
                .StartProgressDialog(
                    None,
                    None,
                    PROGDLG_NOCANCEL | PROGDLG_MARQUEEPROGRESS | PROGDLG_NOTIME,
                    None,
                )
                .map_err(map)?;
            Ok(progress)
        }
    }
    pub fn stage(&self, message: &str) -> Result<()> {
        let value = crate::wide(std::ffi::OsStr::new(message))?;
        // SAFETY: the retained same-thread dialog and string live through call.
        unsafe {
            self.dialog.as_ref().unwrap().SetLine(
                1,
                windows::core::PCWSTR(value.as_ptr()),
                false,
                None,
            )
        }
        .map_err(|e| Error::Windows {
            operation: "SetupProgress",
            code: e.code().0 as u32,
        })
    }
}
impl Drop for Progress {
    fn drop(&mut self) {
        // SAFETY: release interface before balancing the initialization above.
        unsafe {
            if let Some(dialog) = self.dialog.take() {
                let _ = dialog.StopProgressDialog();
            }
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn maintenance_is_exclusive_and_crash_release_does_not_leave_a_stale_signal() {
        let root = tempfile::tempdir().unwrap();
        assert!(!requested_at(root.path()).unwrap());
        let lease = Maintenance::acquire(root.path()).unwrap();
        assert!(requested_at(root.path()).unwrap());
        assert!(Maintenance::acquire(root.path()).is_err());
        drop(lease);
        assert!(!requested_at(root.path()).unwrap());
        Maintenance::acquire(root.path()).unwrap();
    }

    #[test]
    fn interrupted_pair_stays_blocked_but_published_pair_can_be_verified() {
        let root = tempfile::tempdir().unwrap();
        let journal = root.path().join(".app-proxy-upgrade.json");
        std::fs::write(&journal, br#"{"published":false}"#).unwrap();
        assert!(requested_at(root.path()).unwrap());
        std::fs::write(&journal, br#"{"published":true}"#).unwrap();
        assert!(!requested_at(root.path()).unwrap());
        std::fs::write(&journal, br#"{}"#).unwrap();
        assert!(requested_at(root.path()).unwrap());
    }
}
