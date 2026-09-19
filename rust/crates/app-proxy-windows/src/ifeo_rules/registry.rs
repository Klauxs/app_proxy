use crate::{Error, Result, wide};
use serde::{Deserialize, Serialize};
use std::{ffi::OsStr, ptr};
use windows_sys::Win32::{
    Foundation::*, Security::SECURITY_ATTRIBUTES, Storage::FileSystem::DELETE, System::Registry::*,
};

pub(super) struct Key(pub HKEY);
impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: Key exclusively owns this successfully opened registry handle.
        unsafe {
            RegCloseKey(self.0);
        }
    }
}
pub(super) fn check(code: u32, operation: &'static str) -> Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Windows { operation, code })
    }
}
fn name(value: &str) -> Result<Vec<u16>> {
    if value.is_empty() || value.contains(['\\', '/']) {
        return Err(Error::Invalid("IFEO_KEY_NAME"));
    }
    wide(OsStr::new(value))
}
impl Key {
    pub fn open(parent: HKEY, child: &str, write: bool) -> Result<Option<Self>> {
        let child = name(child)?;
        let access = KEY_READ | KEY_WOW64_64KEY | if write { KEY_WRITE | DELETE } else { 0 };
        let mut key = ptr::null_mut();
        // SAFETY: one terminated component, OPEN_LINK prevents following a link.
        let code = unsafe {
            RegOpenKeyExW(
                parent,
                child.as_ptr(),
                REG_OPTION_OPEN_LINK,
                access,
                &mut key,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code, "OpenIfeoKey")?;
        let key = Self(key);
        if key.value("SymbolicLinkValue")?.is_some() {
            return Err(Error::Invalid("IFEO_REGISTRY_LINK"));
        }
        Ok(Some(key))
    }
    pub fn path(parent: HKEY, path: &str, write: bool) -> Result<Option<Self>> {
        let mut key = None;
        for part in path.split('\\') {
            let next = Self::open(key.as_ref().map_or(parent, |k: &Key| k.0), part, write)?;
            if next.is_none() {
                return Ok(None);
            }
            key = next;
        }
        Ok(key)
    }
    pub fn create(
        parent: HKEY,
        child: &str,
        attributes: Option<&SECURITY_ATTRIBUTES>,
    ) -> Result<(Self, bool)> {
        if let Some(key) = Self::open(parent, child, true)? {
            return Ok((key, false));
        }
        let child_name = name(child)?;
        let mut key = ptr::null_mut();
        let mut disposition = 0;
        // SAFETY: only a single new child. Never adopt an existing link through
        // RegCreateKeyEx: close the returned handle and reopen with OPEN_LINK.
        check(
            // SAFETY: terminated single component, valid parent and live optional
            // security descriptor; both output pointers refer to local storage.
            unsafe {
                RegCreateKeyExW(
                    parent,
                    child_name.as_ptr(),
                    0,
                    ptr::null(),
                    0,
                    KEY_READ | KEY_WRITE | DELETE | KEY_WOW64_64KEY,
                    attributes.map_or(ptr::null(), |a| a),
                    &mut key,
                    &mut disposition,
                )
            },
            "CreateIfeoKey",
        )?;
        drop(Self(key));
        let key =
            Self::open(parent, child, true)?.ok_or(Error::Invalid("IFEO_REGISTRATION_CHANGED"))?;
        Ok((key, disposition == REG_CREATED_NEW_KEY))
    }
    pub fn flush(&self) -> Result<()> {
        // SAFETY: self retains the registry handle throughout this call.
        check(unsafe { RegFlushKey(self.0) }, "FlushIfeoKey")
    }
    pub fn value(&self, name: &str) -> Result<Option<Value>> {
        let name = wide(OsStr::new(name))?;
        let mut kind = 0;
        let mut size = 0;
        // SAFETY: size query followed by bounded, separately checked data query.
        let code = unsafe {
            RegQueryValueExW(
                self.0,
                name.as_ptr(),
                ptr::null(),
                &mut kind,
                ptr::null_mut(),
                &mut size,
            )
        };
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        check(code, "ReadIfeoValue")?;
        if size > 65536 {
            return Err(Error::Invalid("IFEO_VALUE_SIZE"));
        }
        let mut bytes = vec![0; size as usize];
        check(
            // SAFETY: the writable buffer has the capacity specified by size;
            // Windows reports a changed/larger value without overrunning it.
            unsafe {
                RegQueryValueExW(
                    self.0,
                    name.as_ptr(),
                    ptr::null(),
                    &mut kind,
                    bytes.as_mut_ptr(),
                    &mut size,
                )
            },
            "ReadIfeoValue",
        )?;
        if size as usize > bytes.len() {
            return Err(Error::Invalid("IFEO_VALUE_CHANGED"));
        }
        bytes.truncate(size as usize);
        Ok(Some(Value { kind, bytes }))
    }
    pub fn set(&self, name: &str, value: &Value) -> Result<()> {
        if value.bytes.len() > 65536 {
            return Err(Error::Invalid("IFEO_VALUE_SIZE"));
        }
        let name = wide(OsStr::new(name))?;
        check(
            // SAFETY: live key, terminated name and readable bounded value buffer.
            unsafe {
                RegSetValueExW(
                    self.0,
                    name.as_ptr(),
                    0,
                    value.kind,
                    value.bytes.as_ptr(),
                    value.bytes.len() as u32,
                )
            },
            "WriteIfeoValue",
        )
    }
    pub fn remove_value(&self, name: &str) -> Result<()> {
        let name = wide(OsStr::new(name))?;
        check(
            // SAFETY: live key and terminated value name; no pointer is retained.
            unsafe { RegDeleteValueW(self.0, name.as_ptr()) },
            "RemoveIfeoValue",
        )
    }
    pub fn children(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for index in 0..=512 {
            let mut name = [0u16; 256];
            let mut size = name.len() as u32;
            // SAFETY: size describes the writable array; optional outputs are null.
            let code = unsafe {
                RegEnumKeyExW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut size,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(names);
            }
            check(code, "ListIfeoKeys")?;
            if index == 512 {
                return Err(Error::Invalid("IFEO_KEY_LIMIT"));
            }
            names.push(
                String::from_utf16(&name[..size as usize])
                    .map_err(|_| Error::Invalid("IFEO_KEY_NAME"))?,
            );
        }
        Err(Error::Invalid("IFEO_KEY_LIMIT"))
    }
    pub fn value_count(&self) -> Result<u32> {
        let mut count = 0;
        check(
            // SAFETY: live key and writable count; all unrequested outputs are null.
            unsafe {
                RegQueryInfoKeyW(
                    self.0,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut count,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            },
            "InspectIfeoKey",
        )?;
        Ok(count)
    }
    pub fn value_names(&self) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for index in 0..=512 {
            let mut name = [0u16; 256];
            let mut size = name.len() as u32;
            // SAFETY: name is a writable buffer of size UTF-16 units; unused
            // type/data outputs are null and the key remains open.
            let code = unsafe {
                RegEnumValueW(
                    self.0,
                    index,
                    name.as_mut_ptr(),
                    &mut size,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                )
            };
            if code == ERROR_NO_MORE_ITEMS {
                return Ok(names);
            }
            check(code, "ListIfeoRecords")?;
            if index == 512 {
                return Err(Error::Invalid("IFEO_RECORD_LIMIT"));
            }
            names.push(
                String::from_utf16(&name[..size as usize])
                    .map_err(|_| Error::Invalid("IFEO_INVALID_INDEX"))?,
            );
        }
        Err(Error::Invalid("IFEO_RECORD_LIMIT"))
    }
    pub fn delete_child(&self, child: &str) -> Result<()> {
        let child = name(child)?;
        check(
            // SAFETY: live parent and terminated single child; this is nonrecursive.
            unsafe { RegDeleteKeyExW(self.0, child.as_ptr(), KEY_WOW64_64KEY, 0) },
            "RemoveIfeoKey",
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Value {
    pub kind: u32,
    pub bytes: Vec<u8>,
}
impl Value {
    pub fn dword(value: u32) -> Self {
        Self {
            kind: REG_DWORD,
            bytes: value.to_le_bytes().to_vec(),
        }
    }
    pub fn string(value: &str) -> Result<Self> {
        Ok(Self {
            kind: REG_SZ,
            bytes: wide(OsStr::new(value))?
                .into_iter()
                .flat_map(u16::to_le_bytes)
                .collect(),
        })
    }
    pub fn text(&self) -> Result<String> {
        if self.kind != REG_SZ || !self.bytes.len().is_multiple_of(2) || self.bytes.len() < 2 {
            return Err(Error::Invalid("IFEO_INVALID_STRING"));
        }
        let words: Vec<_> = self
            .bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        if words.last() != Some(&0) || words[..words.len() - 1].contains(&0) {
            return Err(Error::Invalid("IFEO_INVALID_STRING"));
        }
        String::from_utf16(&words[..words.len() - 1])
            .map_err(|_| Error::Invalid("IFEO_INVALID_STRING"))
    }
}
