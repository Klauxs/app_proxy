//! Product files live outside AppData so packaged and unpackaged processes see
//! the same physical files. Resolve the profile through Windows, not env vars.
use crate::{Error, Result, storage_security};
use std::path::{Path, PathBuf};

pub fn root() -> Result<PathBuf> {
    use windows_sys::Win32::{
        System::Com::CoTaskMemFree,
        UI::Shell::{FOLDERID_Profile, SHGetKnownFolderPath},
    };
    // SAFETY: Windows allocates the known-folder string; it is copied and freed
    // before returning. No caller-controlled path or token is supplied.
    unsafe {
        let mut value = std::ptr::null_mut();
        let status = SHGetKnownFolderPath(&FOLDERID_Profile, 0, std::ptr::null_mut(), &mut value);
        if status < 0 {
            return Err(Error::Windows {
                operation: "GetUserProfile",
                code: status as u32,
            });
        }
        let mut length = 0;
        while *value.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(value, length));
        CoTaskMemFree(value.cast());
        let profile = PathBuf::from(result.map_err(|_| Error::Invalid("INVALID_PROFILE_PATH"))?);
        if !profile.is_absolute() {
            return Err(Error::Invalid("INVALID_PROFILE_PATH"));
        }
        Ok(profile.join("AppProxy"))
    }
}

pub fn ensure_root() -> Result<PathBuf> {
    let root = root()?;
    storage_security::no_reparse(
        root.parent()
            .ok_or(Error::Invalid("PROFILE_PARENT_REQUIRED"))?,
    )?;
    match std::fs::create_dir(&root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let _pin = storage_security::directory(&root, false)?;
    Ok(root)
}

pub fn data_directory() -> Result<PathBuf> {
    Ok(root()?.join("data"))
}

/// Only the verified profile layout bypasses the developer --home fallback for
/// virtualized packages. A custom AppData store still uses package LocalState.
pub fn shared_package_files(path: &Path) -> Result<bool> {
    Ok(within(path, &root()?))
}

fn within(path: &Path, root: &Path) -> bool {
    fn normalize(path: &Path) -> String {
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .replace('/', "\\")
            .trim_end_matches('\\')
            .to_lowercase()
    }
    if path
        .components()
        .any(|part| matches!(part, std::path::Component::ParentDir))
    {
        return false;
    }
    let path = normalize(path);
    let root = normalize(root);
    path == root || path.starts_with(&(root + "\\"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_layout_accepts_canonical_spelling_without_accepting_siblings_or_escape() {
        let root = Path::new(r"C:\Users\Alice\AppProxy");
        assert!(within(Path::new(r"\\?\C:\Users\ALICE\AppProxy\data"), root));
        assert!(!within(
            Path::new(r"C:\Users\Alice\AppProxy-other\data"),
            root
        ));
        assert!(!within(
            Path::new(r"C:\Users\Alice\AppProxy\..\AppData"),
            root
        ));
        assert!(!within(
            Path::new(r"C:\Users\Alice\AppData\Local\AppProxy"),
            root
        ));
    }
}
