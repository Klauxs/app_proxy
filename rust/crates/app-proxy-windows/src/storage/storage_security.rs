use crate::{Error, Result, last_error, wide};
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

use crate::security_ffi::Descriptor;

pub(crate) fn no_reparse(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        let metadata = std::fs::symlink_metadata(part)?;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Invalid("STORE_REPARSE_POINT"));
        }
    }
    Ok(())
}

/// Pins a directory against rename/deletion while its store is open.
pub(crate) fn directory(path: &Path, writable_acl: bool) -> Result<OwnedHandle> {
    no_reparse(path)?;
    if !path.is_dir() {
        return Err(Error::Invalid("STORE_DIRECTORY_REQUIRED"));
    }
    let name = wide(path.as_os_str())?;
    // SAFETY: terminated name, no security/template pointers; handle owned once.
    unsafe {
        let handle = CreateFileW(
            name.as_ptr(),
            READ_CONTROL | if writable_acl { WRITE_DAC } else { 0 },
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("OpenStoreDirectory"));
        }
        Ok(OwnedHandle::from_raw_handle(handle))
    }
}

pub(crate) fn protect(handle: &OwnedHandle, sid: &str) -> Result<()> {
    verify_owner(handle.as_raw_handle(), sid)?;
    let sddl = wide(std::ffi::OsStr::new(&format!(
        "D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)"
    )))?;
    // SAFETY: valid SDDL buffer, output allocated by Windows and kept alive until SetSecurityInfo.
    unsafe {
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(last_error("CreateStoreAcl"));
        }
        let descriptor = Descriptor(descriptor);
        let mut present = 0;
        let mut defaulted = 0;
        let mut acl = null_mut();
        if GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) == 0 {
            return Err(last_error("GetStoreAcl"));
        }
        let code = SetSecurityInfo(
            handle.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        );
        if code != 0 {
            return Err(Error::Windows {
                operation: "ProtectStore",
                code,
            });
        }
    }
    verify(handle.as_raw_handle(), sid, true)
}

pub(crate) fn verify_owner(handle: HANDLE, sid: &str) -> Result<()> {
    // SAFETY: live handle, owner SID remains valid until its allocated descriptor is dropped.
    unsafe {
        let mut owner = null_mut();
        let mut descriptor = null_mut();
        let code = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            null_mut(),
            null_mut(),
            &mut descriptor,
        );
        if code != 0 {
            return Err(Error::Windows {
                operation: "ReadStoreOwner",
                code,
            });
        }
        let _descriptor = Descriptor(descriptor);
        if sid_string(owner)? != sid {
            return Err(Error::Invalid("STORE_OWNER_MISMATCH"));
        }
    }
    Ok(())
}

pub(crate) fn verify(handle: HANDLE, sid: &str, protected: bool) -> Result<()> {
    // SAFETY: security API owns the returned descriptor; ACE pointers remain within it.
    unsafe {
        let mut owner = null_mut();
        let mut acl = null_mut();
        let mut descriptor = null_mut();
        let code = GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if code != 0 {
            return Err(Error::Windows {
                operation: "ReadStoreAcl",
                code,
            });
        }
        let descriptor = Descriptor(descriptor);
        if sid_string(owner)? != sid || acl.is_null() {
            return Err(Error::Invalid("STORE_OWNER_OR_ACL_MISMATCH"));
        }
        let mut control = 0;
        let mut revision = 0;
        if GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) == 0 {
            return Err(last_error("GetStoreAclControl"));
        }
        if protected && control & SE_DACL_PROTECTED == 0 {
            return Err(Error::Invalid("STORE_ACL_NOT_PROTECTED"));
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(handle, &mut info) == 0 {
            return Err(last_error("InspectStoreAclTarget"));
        }
        let directory = info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
        let mut allowed = std::collections::HashSet::new();
        for index in 0..(*acl).AceCount {
            let mut entry = null_mut();
            if GetAce(acl, index as u32, &mut entry) == 0 {
                return Err(last_error("GetStoreAce"));
            }
            let header = &*entry.cast::<ACE_HEADER>();
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE
                || u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0
            {
                return Err(Error::Invalid("STORE_UNEXPECTED_ACE"));
            }
            let flags = u32::from(header.AceFlags);
            if directory
                && (flags & (OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)
                    != OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE
                    || flags & NO_PROPAGATE_INHERIT_ACE != 0)
            {
                return Err(Error::Invalid("STORE_ACL_NOT_INHERITABLE"));
            }
            let ace = &*entry.cast::<ACCESS_ALLOWED_ACE>();
            let trustee = sid_string((&ace.SidStart as *const u32).cast_mut().cast())?;
            if ![sid, "S-1-5-18", "S-1-5-32-544"].contains(&trustee.as_str())
                || ace.Mask != FILE_ALL_ACCESS
            {
                return Err(Error::Invalid("STORE_UNSAFE_ACE"));
            }
            allowed.insert(trustee);
        }
        if allowed.len() != 3 {
            return Err(Error::Invalid("STORE_INCOMPLETE_ACL"));
        }
    }
    Ok(())
}

unsafe fn sid_string(sid: PSID) -> Result<String> {
    // SAFETY: the caller supplies a SID from a live Windows security descriptor.
    unsafe { crate::security_ffi::sid_string(sid, "ConvertStoreSid", "INVALID_STORE_SID") }
}
