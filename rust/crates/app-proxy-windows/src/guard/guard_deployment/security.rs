use crate::{Error, Result, last_error, storage_security, wide};
use std::{
    fs::File,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    ptr,
};
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
};

const ADMIN: &str = "S-1-5-32-544";
const SYSTEM: &str = "S-1-5-18";
const USERS: &str = "S-1-5-32-545";
const READ_EXECUTE: u32 = FILE_GENERIC_READ | FILE_GENERIC_EXECUTE;
const SDDL: &str = "O:BAG:BAD:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;0x1200a9;;;BU)";

use crate::security_ffi::Descriptor;
impl Descriptor {
    fn new(sddl: &str) -> Result<Self> {
        let text = wide(std::ffi::OsStr::new(sddl))?;
        let mut value = ptr::null_mut();
        // SAFETY: terminated SDDL and valid output, Windows owns allocation.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                text.as_ptr(),
                SDDL_REVISION_1,
                &mut value,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(last_error("CreateGuardDescriptor"));
        }
        Ok(Self(value))
    }
    fn attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

pub(super) fn directory(path: &Path, create: bool) -> Result<OwnedHandle> {
    if create {
        match create_directory(path) {
            Ok(()) => {}
            Err(Error::Windows {
                code: ERROR_ALREADY_EXISTS,
                ..
            }) => {}
            Err(error) => return Err(error),
        }
    }
    let handle = storage_security::directory(path, false)?;
    verify(handle.as_raw_handle(), true)?;
    Ok(handle)
}
pub(super) fn create_generation(path: &Path) -> Result<OwnedHandle> {
    // A UUID collision does not authorize adoption, even with the correct ACL.
    create_directory(path)?;
    directory(path, false)
}
fn create_directory(path: &Path) -> Result<()> {
    storage_security::no_reparse(
        path.parent()
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?,
    )?;
    let descriptor = Descriptor::new(SDDL)?;
    let attributes = descriptor.attributes();
    let path = wide(path.as_os_str())?;
    // SAFETY: descriptor/path retained until directory creation finishes.
    if unsafe { CreateDirectoryW(path.as_ptr(), &attributes) } == 0 {
        return Err(last_error("CreateGuardDirectory"));
    }
    Ok(())
}
pub(super) fn new_file(path: &Path) -> Result<File> {
    let parent = path
        .parent()
        .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?;
    let _parent = directory(parent, false)?;
    let descriptor = Descriptor::new(SDDL)?;
    let attributes = descriptor.attributes();
    open_file(path, GENERIC_READ | GENERIC_WRITE, CREATE_NEW, &attributes)
}
pub(super) fn read_file(path: &Path) -> Result<File> {
    storage_security::no_reparse(path)?;
    open_file(path, GENERIC_READ, OPEN_EXISTING, ptr::null())
}
pub(super) fn event_journal(path: &Path) -> Result<File> {
    let _parent = directory(
        path.parent()
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?,
        false,
    )?;
    match new_file(path) {
        Ok(file) => Ok(file),
        Err(Error::Windows {
            code: ERROR_FILE_EXISTS | ERROR_ALREADY_EXISTS,
            ..
        }) => {
            // No truncation or ACL repair. FILE_SHARE_READ excludes every other
            // writer and deletion; verify the existing object before reading it.
            open_file(
                path,
                GENERIC_READ | GENERIC_WRITE,
                OPEN_EXISTING,
                ptr::null(),
            )
        }
        Err(error) => Err(error),
    }
}
fn open_file(
    path: &Path,
    access: u32,
    disposition: u32,
    attributes: *const SECURITY_ATTRIBUTES,
) -> Result<File> {
    let name = wide(path.as_os_str())?;
    // SAFETY: terminated path and optional descriptor remain live, handle owned once.
    unsafe {
        let handle = CreateFileW(
            name.as_ptr(),
            access,
            FILE_SHARE_READ,
            attributes,
            disposition,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("OpenGuardFile"));
        }
        let file = File::from_raw_handle(handle);
        verify(file.as_raw_handle(), false)?;
        Ok(file)
    }
}

fn verify(handle: HANDLE, directory: bool) -> Result<()> {
    // SAFETY: caller holds object handle with READ_CONTROL; descriptor/ACE/SID
    // pointers are retained until all validation completes.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(handle, &mut info) == 0 {
            return Err(last_error("InspectGuardObject"));
        }
        if (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
            || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
            || (!directory && info.nNumberOfLinks != 1)
        {
            return Err(Error::Invalid("GUARD_OBJECT_TYPE_MISMATCH"));
        }
        let mut descriptor = ptr::null_mut();
        let code = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            ptr::null_mut(),
            &mut descriptor,
        );
        if code != ERROR_SUCCESS {
            return Err(Error::Windows {
                operation: "ReadGuardSecurity",
                code,
            });
        }
        let descriptor = Descriptor(descriptor);
        validate_descriptor(descriptor.0, directory)
    }
}

unsafe fn validate_descriptor(descriptor: PSECURITY_DESCRIPTOR, directory: bool) -> Result<()> {
    // SAFETY: caller retains a valid Windows descriptor; ACE/SID fields remain
    // within that allocation and no untrusted serialized ACL is cast here.
    unsafe {
        let mut owner = ptr::null_mut();
        let mut defaulted = 0;
        let mut present = 0;
        let mut acl = ptr::null_mut();
        let mut control = 0;
        let mut revision = 0;
        if GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor, &mut present, &mut acl, &mut defaulted) == 0
            || GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0
        {
            return Err(last_error("InspectGuardSecurity"));
        }
        if owner.is_null()
            || sid_string(owner)? != ADMIN
            || present == 0
            || acl.is_null()
            || control & SE_DACL_PROTECTED == 0
            || (*acl).AceCount != 3
        {
            return Err(Error::Invalid("GUARD_UNPROTECTED_OBJECT"));
        }
        let mut trustees = std::collections::HashSet::new();
        for index in 0..(*acl).AceCount {
            let mut entry = ptr::null_mut();
            if GetAce(acl, index as u32, &mut entry) == 0 {
                return Err(last_error("InspectGuardAce"));
            }
            let header = &*entry.cast::<ACE_HEADER>();
            let flags = u32::from(header.AceFlags);
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
                || flags & (INHERIT_ONLY_ACE | NO_PROPAGATE_INHERIT_ACE) != 0
                || (directory
                    && flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)
                        != OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)
            {
                return Err(Error::Invalid("GUARD_UNSAFE_ACE"));
            }
            let ace = &*entry.cast::<ACCESS_ALLOWED_ACE>();
            let sid = sid_string((&ace.SidStart as *const u32).cast_mut().cast())?;
            let rights = match sid.as_str() {
                ADMIN | SYSTEM => FILE_ALL_ACCESS,
                USERS => READ_EXECUTE,
                _ => return Err(Error::Invalid("GUARD_UNSAFE_TRUSTEE")),
            };
            if ace.Mask != rights || !trustees.insert(sid) {
                return Err(Error::Invalid("GUARD_UNSAFE_RIGHTS"));
            }
        }
        Ok(())
    }
}
unsafe fn sid_string(sid: PSID) -> Result<String> {
    // SAFETY: the caller supplies a SID from a live Windows security descriptor.
    unsafe { crate::security_ffi::sid_string(sid, "ConvertGuardSid", "INVALID_GUARD_SID") }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admin_descriptor_requires_exact_read_only_user_rights_and_inheritance() {
        let descriptor = Descriptor::new(SDDL).unwrap();
        // SAFETY: descriptor is produced by Windows and retained through validation.
        unsafe {
            validate_descriptor(descriptor.0, true).unwrap();
            validate_descriptor(descriptor.0, false).unwrap();
        }
        for text in [
            SDDL.replace("O:BA", "O:BU"),
            SDDL.replace("D:P", "D:"),
            SDDL.replace("0x1200a9", "FA"),
            SDDL.replace(";;;BU", ";;;WD"),
            SDDL.replace("OICI;", "OI;"),
            SDDL.replace("OICI;", "OICIIO;"),
            SDDL.replace("OICI;", "OICINP;"),
            SDDL.replace("(A;OICI;FA;;;BA)", ""),
            format!("{SDDL}(A;OICI;FA;;;BA)"),
        ] {
            let descriptor = Descriptor::new(&text).unwrap();
            // SAFETY: descriptor is produced by Windows, not from raw external bytes.
            let rejected = unsafe { validate_descriptor(descriptor.0, true) }.is_err();
            assert!(rejected, "{text}");
        }
    }
    #[test]
    fn ordinary_owned_files_and_directories_are_never_adopted() {
        let temp = tempfile::tempdir().unwrap();
        assert!(directory(temp.path(), false).is_err());
        let path = temp.path().join("host.exe");
        std::fs::write(&path, b"fixture").unwrap();
        assert!(read_file(&path).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"fixture");
    }
}
