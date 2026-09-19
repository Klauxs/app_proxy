//! Read-only installation resolution. Keep the result only for preparation/spawn;
//! retaining it for an application's entire lifetime would obstruct updates.
use crate::{Error, Result, last_error, package};
use app_proxy_core::{FileIdentity, model::ApplicationLocator};
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::windows::{ffi::OsStringExt, fs::OpenOptionsExt, io::AsRawHandle};
use std::path::{Component, Path, PathBuf};
use windows_sys::Win32::Storage::FileSystem::*;

pub struct ResolvedApplication {
    executable: PathBuf,
    image: FileIdentity,
    package: Option<package::Package>,
    locator: ApplicationLocator,
    // Deny write/delete sharing while the plan is being prepared. Final identity
    // checks remain necessary: parent directories/package registration can change.
    _file: File,
}

impl ResolvedApplication {
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn image(&self) -> &FileIdentity {
        &self.image
    }
    pub fn package(&self) -> Option<&package::Package> {
        self.package.as_ref()
    }

    /// MSIX identity survives version upgrades. Plain EXE aliases and hard links
    /// compare their actual file identity, never their display name or template.
    pub fn same_installation(&self, other: &Self) -> bool {
        match (&self.package, &other.package) {
            (Some(a), Some(b)) => {
                a.family_name.eq_ignore_ascii_case(&b.family_name)
                    && a.app_id.eq_ignore_ascii_case(&b.app_id)
            }
            _ => self.image == other.image,
        }
    }

    pub fn verify_current(&self) -> Result<()> {
        self.verify_with(package::resolve)
    }

    fn verify_with(
        &self,
        query: impl FnOnce(&str, &str) -> Result<package::Package>,
    ) -> Result<()> {
        let current = resolve_with(&self.locator, query)?;
        if self.package.as_ref().map(|p| &p.full_name)
            != current.package.as_ref().map(|p| &p.full_name)
        {
            return Err(Error::Invalid("PACKAGE_CHANGED"));
        }
        if self.image != current.image || self.executable != current.executable {
            return Err(Error::Invalid("INSTALLATION_CHANGED"));
        }
        Ok(())
    }
}

pub fn resolve(locator: &ApplicationLocator) -> Result<ResolvedApplication> {
    resolve_with(locator, package::resolve)
}

fn resolve_with(
    locator: &ApplicationLocator,
    query: impl FnOnce(&str, &str) -> Result<package::Package>,
) -> Result<ResolvedApplication> {
    let package = match locator {
        ApplicationLocator::Exe { .. } => None,
        ApplicationLocator::Msix {
            family_name,
            app_id,
        } => {
            let package = query(family_name, app_id)?;
            if !package.family_name.eq_ignore_ascii_case(family_name)
                || !package.app_id.eq_ignore_ascii_case(app_id)
                || package.full_name.is_empty()
            {
                return Err(Error::Invalid("PACKAGE_IDENTITY_MISMATCH"));
            }
            Some(package)
        }
    };
    let path = match (&package, locator) {
        (Some(package), _) => &package.exe,
        (None, ApplicationLocator::Exe { path }) => path,
        _ => unreachable!(),
    };
    executable_path(path)?;
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Invalid("EXECUTABLE_FILE_REQUIRED"));
    }
    let (executable, image) = inspect_file(&file)?;
    executable_path(&executable)?;
    Ok(ResolvedApplication {
        executable,
        image,
        package,
        locator: locator.clone(),
        _file: file,
    })
}

fn executable_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path.components().any(|p| matches!(p, Component::ParentDir))
        || path.to_str().is_none_or(|s| s.contains('\0'))
        || !path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    {
        return Err(Error::Invalid("ABSOLUTE_EXE_REQUIRED"));
    }
    Ok(())
}

fn inspect_file(file: &File) -> Result<(PathBuf, FileIdentity)> {
    // SAFETY: file owns a live handle; both output buffers have their declared size.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(last_error("GetFileInformationByHandle(installation)"));
        }
        let mut buffer = vec![0u16; 32768];
        let length = GetFinalPathNameByHandleW(
            file.as_raw_handle(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
        );
        if length == 0 {
            return Err(last_error("GetFinalPathNameByHandleW(installation)"));
        }
        if length as usize >= buffer.len() {
            return Err(Error::Invalid("INSTALLATION_PATH_TOO_LONG"));
        }
        buffer.truncate(length as usize);
        Ok((
            PathBuf::from(OsString::from_wide(&buffer)),
            FileIdentity {
                volume_serial: info.dwVolumeSerialNumber,
                file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
            },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn exe(path: &Path) -> ApplicationLocator {
        ApplicationLocator::Exe { path: path.into() }
    }
    fn package(path: &Path, version: &str) -> package::Package {
        package::Package {
            family_name: "Fixture_publisher".into(),
            full_name: format!("Fixture_{version}_x64__publisher"),
            app_id: "App".into(),
            exe: path.into(),
            isolated_storage: true,
        }
    }
    fn locator() -> ApplicationLocator {
        ApplicationLocator::Msix {
            family_name: "Fixture_publisher".into(),
            app_id: "App".into(),
        }
    }

    #[test]
    fn hard_links_and_case_aliases_share_identity_but_copies_do_not() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("fixture.exe");
        let alias = temp.path().join("alias.exe");
        let copy = temp.path().join("copy.exe");
        fs::write(&first, b"file identity fixture, never executed").unwrap();
        fs::hard_link(&first, &alias).unwrap();
        fs::copy(&first, &copy).unwrap();
        let a = resolve(&exe(&first)).unwrap();
        let b = resolve(&exe(&alias)).unwrap();
        let c = resolve(&exe(&copy)).unwrap();
        let case = resolve(&exe(&temp.path().join("FIXTURE.EXE"))).unwrap();
        assert!(a.same_installation(&b));
        assert!(a.same_installation(&case));
        assert!(!a.same_installation(&c));
        assert_eq!(a.executable(), fs::canonicalize(&first).unwrap());
        a.verify_current().unwrap();
    }

    #[test]
    fn plan_handle_pins_image_until_dropped_and_bad_inputs_are_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.exe");
        fs::write(&path, b"fixture").unwrap();
        let plan = resolve(&exe(&path)).unwrap();
        assert!(fs::write(&path, b"replacement").is_err());
        assert!(fs::rename(&path, temp.path().join("renamed.exe")).is_err());
        drop(plan);
        fs::write(&path, b"replacement").unwrap();
        for invalid in [
            PathBuf::from("relative.exe"),
            temp.path().join("run.cmd"),
            temp.path().join("run.bat"),
            temp.path().join("missing.exe"),
            temp.path().join("..\\fixture.exe"),
        ] {
            assert!(resolve(&exe(&invalid)).is_err());
        }
        let directory = temp.path().join("directory.exe");
        fs::create_dir(&directory).unwrap();
        assert!(resolve(&exe(&directory)).is_err());
    }

    #[test]
    fn package_locator_is_stable_but_old_plan_is_rejected_after_update() {
        let temp = tempfile::tempdir().unwrap();
        let old = temp.path().join("old.exe");
        let new = temp.path().join("new.exe");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"new").unwrap();
        let first = resolve_with(&locator(), |family, app| {
            assert_eq!(family, "Fixture_publisher");
            assert_eq!(app, "App");
            Ok(package(&old, "1"))
        })
        .unwrap();
        let updated = resolve_with(&locator(), |_, _| Ok(package(&new, "2"))).unwrap();
        assert!(first.same_installation(&updated));
        assert_ne!(first.image(), updated.image());
        assert!(matches!(
            first.verify_with(|_, _| Ok(package(&new, "2"))),
            Err(Error::Invalid("PACKAGE_CHANGED"))
        ));
        first.verify_with(|_, _| Ok(package(&old, "1"))).unwrap();
        assert!(matches!(
            first.verify_with(|_, _| Err(Error::Invalid("APP_NOT_INSTALLED"))),
            Err(Error::Invalid("APP_NOT_INSTALLED"))
        ));
        assert!(
            resolve_with(&locator(), |_, _| {
                let mut wrong = package(&old, "1");
                wrong.family_name = "Other_family".into();
                Ok(wrong)
            })
            .is_err()
        );
    }
}
