use super::registry::{Key, check};
use crate::{Error, Result, last_error, wide};
use std::{ffi::OsStr, ptr};
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::{DELETE, READ_CONTROL, SYNCHRONIZE, WRITE_DAC, WRITE_OWNER},
    System::{Registry::*, SystemServices::ACCESS_ALLOWED_ACE_TYPE},
};

const SDDL: &str = "O:BAG:BAD:P(A;CI;KA;;;SY)(A;CI;KA;;;BA)(A;CI;KR;;;BU)";
const MUTEX_SDDL: &str = "O:BAG:BAD:P(A;;0x1f0001;;;SY)(A;;0x1f0001;;;BA)";
const ADMIN: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";
const INSTALLER: &str = "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464";
pub(super) struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: descriptor was allocated by Windows and is exclusively owned.
        unsafe {
            LocalFree(self.0);
        }
    }
}
impl Descriptor {
    pub fn new() -> Result<Self> {
        Self::from_sddl(SDDL)
    }
    fn from_sddl(sddl: &str) -> Result<Self> {
        let value = wide(OsStr::new(sddl))?;
        let mut descriptor = ptr::null_mut();
        // SAFETY: terminated SDDL input and writable output; ownership transfers
        // to Descriptor only after Windows reports success.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                value.as_ptr(),
                SDDL_REVISION_1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("CreateIfeoSecurity"));
        }
        Ok(Self(descriptor))
    }
    pub fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
    fn read(key: &Key) -> Result<Self> {
        Self::read_object(key.0, SE_REGISTRY_KEY)
    }
    fn read_object(handle: HANDLE, kind: SE_OBJECT_TYPE) -> Result<Self> {
        let mut descriptor = ptr::null_mut();
        check(
            // SAFETY: caller retains the handle of the specified object kind;
            // Windows allocates the descriptor returned through this output.
            unsafe {
                GetSecurityInfo(
                    handle,
                    kind,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null_mut(),
                    &mut descriptor,
                )
            },
            "ReadIfeoSecurity",
        )?;
        Ok(Self(descriptor))
    }
    fn text(&self) -> Result<String> {
        let mut text = ptr::null_mut();
        let mut length = 0;
        // SAFETY: self retains the descriptor; Windows allocates the output text.
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                self.0,
                SDDL_REVISION_1,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut text,
                &mut length,
            )
        } == 0
        {
            return Err(last_error("InspectIfeoSecurity"));
        }
        // SAFETY: successful conversion returned length UTF-16 units including NUL.
        let result = unsafe {
            String::from_utf16(std::slice::from_raw_parts(
                text,
                length.saturating_sub(1) as usize,
            ))
        };
        // SAFETY: the conversion allocated text with the LocalAlloc allocator.
        unsafe {
            LocalFree(text.cast());
        }
        result.map_err(|_| Error::Invalid("IFEO_INVALID_SECURITY"))
    }
}

pub(super) struct Mutation(HANDLE);
impl Mutation {
    pub fn acquire() -> Result<Self> {
        crate::identity::assert_elevated_user()?;
        let descriptor = Descriptor::from_sddl(MUTEX_SDDL)?;
        let attributes = descriptor.attributes();
        let name = wide(OsStr::new(r"Global\AppProxyRust-Ifeo-Registry"))?;
        // SAFETY: bounded fixed name and retained administrator-only descriptor.
        let handle = unsafe {
            CreateMutexExW(
                &attributes,
                name.as_ptr(),
                0,
                SYNCHRONIZE | MUTEX_MODIFY_STATE | READ_CONTROL,
            )
        };
        if handle.is_null() {
            return Err(last_error("OpenIfeoMutationLock"));
        }
        let accepted = (|| {
            if Descriptor::read_object(handle, SE_KERNEL_OBJECT)?.text()? != descriptor.text()? {
                return Err(Error::Invalid("IFEO_UNPROTECTED_LOCK"));
            }
            // SAFETY: handle is an open mutex with SYNCHRONIZE access.
            match unsafe { WaitForSingleObject(handle, 10_000) } {
                WAIT_OBJECT_0 | WAIT_ABANDONED => Ok(()),
                WAIT_TIMEOUT => Err(Error::Invalid("IFEO_INSTALL_BUSY")),
                _ => Err(last_error("WaitIfeoMutationLock")),
            }
        })();
        if let Err(error) = accepted {
            // SAFETY: no Mutation owns this handle on the error path.
            unsafe {
                CloseHandle(handle);
            }
            return Err(error);
        }
        Ok(Self(handle))
    }
}
impl Drop for Mutation {
    fn drop(&mut self) {
        // SAFETY: acquire obtained the mutex on this thread; Mutation is local
        // to the synchronous mutation and exclusively owns its handle.
        unsafe {
            ReleaseMutex(self.0);
            CloseHandle(self.0);
        }
    }
}
pub(super) fn owned(key: &Key) -> Result<()> {
    if Descriptor::read(key)?.text()? != Descriptor::new()?.text()? {
        return Err(Error::Invalid("IFEO_UNPROTECTED_RECORD"));
    }
    Ok(())
}
pub(super) fn system_key(key: &Key) -> Result<()> {
    let descriptor = Descriptor::read(key)?;
    // SAFETY: every pointer below belongs to a live Windows-owned descriptor.
    unsafe {
        let mut owner = ptr::null_mut();
        let mut acl = ptr::null_mut();
        let mut present = 0;
        let mut defaulted = 0;
        if GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) == 0
        {
            return Err(last_error("InspectIfeoSecurity"));
        }
        if owner.is_null() || !trusted(&sid(owner)?) || present == 0 || acl.is_null() {
            return Err(Error::Invalid("IFEO_WRITABLE_SYSTEM_KEY"));
        }
        for index in 0..(*acl).AceCount {
            let mut entry = ptr::null_mut();
            if GetAce(acl, index as u32, &mut entry) == 0 {
                return Err(last_error("InspectIfeoAce"));
            }
            let header = &*entry.cast::<ACE_HEADER>();
            if u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE {
                let ace = &*entry.cast::<ACCESS_ALLOWED_ACE>();
                let writes = KEY_SET_VALUE
                    | KEY_CREATE_SUB_KEY
                    | KEY_CREATE_LINK
                    | DELETE
                    | WRITE_DAC
                    | WRITE_OWNER
                    | GENERIC_WRITE
                    | GENERIC_ALL;
                // CREATOR_OWNER is an inheritance placeholder, not a grant to
                // every user. Windows SOFTWARE uses it; the actual owner was
                // already restricted to administrators/SYSTEM/TrustedInstaller.
                if ace.Mask & writes != 0
                    && u32::from(header.AceFlags) & INHERIT_ONLY_ACE == 0
                    && !matches!(
                        sid((&ace.SidStart as *const u32).cast_mut().cast())?.as_str(),
                        ADMIN | SYSTEM | INSTALLER | "S-1-3-0"
                    )
                {
                    return Err(Error::Invalid("IFEO_WRITABLE_SYSTEM_KEY"));
                }
            } else if header.AceType != 1 {
                return Err(Error::Invalid("IFEO_UNSUPPORTED_SECURITY"));
            }
        }
    }
    Ok(())
}
fn trusted(sid: &str) -> bool {
    matches!(sid, ADMIN | SYSTEM | INSTALLER)
}
unsafe fn sid(value: PSID) -> Result<String> {
    // SAFETY: caller provides a SID within a live Windows descriptor/ACE.
    // Successful conversion returns allocated NUL-terminated text, freed once.
    unsafe {
        let mut text = ptr::null_mut();
        if ConvertSidToStringSidW(value, &mut text) == 0 {
            return Err(last_error("InspectIfeoSid"));
        }
        let mut len = 0;
        while *text.add(len) != 0 {
            len += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(text, len));
        LocalFree(text.cast());
        result.map_err(|_| Error::Invalid("IFEO_INVALID_SID"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owned_descriptor_distinguishes_owner_write_and_inheritance_changes() {
        let expected = Descriptor::new().unwrap().text().unwrap();
        assert!(expected.contains("O:BA"));
        for value in [
            SDDL.replace("O:BA", "O:BU"),
            SDDL.replace("D:P", "D:"),
            SDDL.replace("KR;;;BU", "KA;;;BU"),
            SDDL.replace("CI;", ";"),
        ] {
            assert_ne!(
                Descriptor::from_sddl(&value).unwrap().text().unwrap(),
                expected
            );
        }
    }
}
