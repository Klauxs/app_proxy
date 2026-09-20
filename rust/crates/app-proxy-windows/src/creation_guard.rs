//! Read-only guard against externally configured Windows debugger redirection.
//! No registrations are created, changed or removed by this module.
use crate::{Error, Result};
use std::{path::Path, ptr};
use windows_sys::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS},
    System::Registry::*,
};

struct Key(HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: this is a successfully opened key, not a predefined root.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}

/// Do not let ordinary CreateProcess silently execute a third-party debugger.
/// Filtered Debugger entries are conservatively rejected because this launcher does not support debugger redirection; unrelated mitigation-only keys are allowed.
pub fn ensure_plain_creation(executable: &Path) -> Result<()> {
    let name = executable
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(Error::Invalid("EXE_REQUIRED"))?;
    let path = format!(
        "SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Image File Execution Options\\{name}"
    );
    for view in [KEY_WOW64_64KEY, KEY_WOW64_32KEY] {
        if let Some(key) = open(HKEY_LOCAL_MACHINE, &path, KEY_READ | view)? {
            check(&key, view)?;
        }
    }
    Ok(())
}

fn open(parent: HKEY, path: &str, access: u32) -> Result<Option<Key>> {
    let path = crate::wide(std::ffi::OsStr::new(path))?;
    let mut key = ptr::null_mut();
    // SAFETY: terminated path and live output; successful handle is owned once.
    let status = unsafe { RegOpenKeyExW(parent, path.as_ptr(), 0, access, &mut key) };
    match status {
        0 => Ok(Some(Key(key))),
        ERROR_FILE_NOT_FOUND => Ok(None),
        code => Err(Error::Windows {
            operation: "ReadIfeoRegistration",
            code,
        }),
    }
}
fn debugger(key: &Key) -> Result<bool> {
    let name = crate::wide(std::ffi::OsStr::new("Debugger"))?;
    let mut bytes = 0;
    // SAFETY: size-only query; a present value is enough to reject uncertain routing.
    let status = unsafe {
        RegQueryValueExW(
            key.0,
            name.as_ptr(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut bytes,
        )
    };
    match status {
        0 => Ok(true),
        ERROR_FILE_NOT_FOUND => Ok(false),
        code => Err(Error::Windows {
            operation: "ReadIfeoDebugger",
            code,
        }),
    }
}
fn check(key: &Key, view: u32) -> Result<()> {
    if debugger(key)? {
        return Err(Error::Invalid("EXTERNAL_DEBUGGER_UNSUPPORTED"));
    }
    for index in 0..512 {
        let mut name = [0u16; 256];
        let mut length = name.len() as u32;
        // SAFETY: declared capacity matches output storage; optional outputs null.
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                name.as_mut_ptr(),
                &mut length,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if status == ERROR_NO_MORE_ITEMS {
            return Ok(());
        }
        if status != 0 {
            return Err(Error::Windows {
                operation: "EnumerateIfeoFilters",
                code: status,
            });
        }
        let name = String::from_utf16(&name[..length as usize])
            .map_err(|_| Error::Invalid("INVALID_IFEO_FILTER"))?;
        let child = open(key.0, &name, KEY_READ | view)?
            .ok_or(Error::Invalid("IFEO_REGISTRATION_CHANGED"))?;
        if debugger(&child)? {
            return Err(Error::Invalid("EXTERNAL_DEBUGGER_UNSUPPORTED"));
        }
    }
    Err(Error::Invalid("IFEO_FILTER_LIMIT"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn read_only_guard_allows_mitigation_keys_and_rejects_root_or_filtered_debuggers() {
        let path = format!("Software\\AppProxyRustIfeoFixture-{}", uuid::Uuid::new_v4());
        let wide = crate::wide(std::ffi::OsStr::new(&path)).unwrap();
        struct Fixture(Vec<u16>);
        impl Drop for Fixture {
            fn drop(&mut self) {
                // SAFETY: deletes only this test's uniquely named HKCU subtree.
                unsafe {
                    RegDeleteTreeW(HKEY_CURRENT_USER, self.0.as_ptr());
                }
            }
        }
        // SAFETY: unique per-test HKCU path, bounded outputs, owned key lifetime.
        unsafe {
            let mut key = ptr::null_mut();
            assert_eq!(
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    wide.as_ptr(),
                    0,
                    ptr::null(),
                    0,
                    KEY_ALL_ACCESS,
                    ptr::null(),
                    &mut key,
                    ptr::null_mut()
                ),
                0
            );
            let _cleanup = Fixture(wide);
            let key = Key(key);
            let name = crate::wide(std::ffi::OsStr::new("GlobalFlag")).unwrap();
            assert_eq!(
                RegSetValueExW(
                    key.0,
                    name.as_ptr(),
                    0,
                    REG_DWORD,
                    0u32.to_le_bytes().as_ptr(),
                    4
                ),
                0
            );
            check(&key, 0).unwrap();
            let debugger = crate::wide(std::ffi::OsStr::new("Debugger")).unwrap();
            let command = crate::wide(std::ffi::OsStr::new("fixture-only.exe")).unwrap();
            assert_eq!(
                RegSetValueExW(
                    key.0,
                    debugger.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32
                ),
                0
            );
            assert!(matches!(
                check(&key, 0),
                Err(Error::Invalid("EXTERNAL_DEBUGGER_UNSUPPORTED"))
            ));
            assert_eq!(RegDeleteValueW(key.0, debugger.as_ptr()), 0);
            let filter = crate::wide(std::ffi::OsStr::new("filter")).unwrap();
            let mut child = ptr::null_mut();
            assert_eq!(
                RegCreateKeyExW(
                    key.0,
                    filter.as_ptr(),
                    0,
                    ptr::null(),
                    0,
                    KEY_ALL_ACCESS,
                    ptr::null(),
                    &mut child,
                    ptr::null_mut()
                ),
                0
            );
            let child = Key(child);
            assert_eq!(
                RegSetValueExW(
                    child.0,
                    debugger.as_ptr(),
                    0,
                    REG_SZ,
                    command.as_ptr().cast(),
                    (command.len() * 2) as u32
                ),
                0
            );
            assert!(matches!(
                check(&key, 0),
                Err(Error::Invalid("EXTERNAL_DEBUGGER_UNSUPPORTED"))
            ));
        }
        assert!(open(HKEY_CURRENT_USER, &path, KEY_READ).unwrap().is_none());
    }
}
