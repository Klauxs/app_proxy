use crate::{Error, Result, last_error, wide};
use std::ptr;
use windows_sys::Win32::{
    Foundation::*,
    Security::{Authorization::*, *},
    Storage::FileSystem::*,
    System::SystemServices::ACCESS_ALLOWED_ACE_TYPE,
};

pub(super) fn sddl(sid: &str) -> String {
    format!("O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;GRGX;;;{sid})")
}

// Task Scheduler's UserId getter returns an account name even when SetUserId
// received a SID. Compare the resolved Windows SID, never the display string.
pub(super) fn user_matches(account: &str, expected: &str) -> Result<bool> {
    if account == expected {
        return Ok(true);
    }
    let account = wide(std::ffi::OsStr::new(account))?;
    // SAFETY: bounded two-call Windows lookup with aligned SID and UTF-16 domain
    // buffers; outputs stay live through SID conversion.
    unsafe {
        let (mut sid_size, mut domain_size) = (0, 0);
        let mut kind = SidTypeUnknown;
        LookupAccountNameW(
            ptr::null(),
            account.as_ptr(),
            ptr::null_mut(),
            &mut sid_size,
            ptr::null_mut(),
            &mut domain_size,
            &mut kind,
        );
        if sid_size == 0 || sid_size > 1024 || domain_size > 32768 {
            return Err(last_error("ResolveGuardTaskUser"));
        }
        let mut sid = vec![0u64; (sid_size as usize).div_ceil(8)];
        let mut domain = vec![0u16; domain_size as usize];
        if LookupAccountNameW(
            ptr::null(),
            account.as_ptr(),
            sid.as_mut_ptr().cast(),
            &mut sid_size,
            domain.as_mut_ptr(),
            &mut domain_size,
            &mut kind,
        ) == 0
        {
            return Err(last_error("ResolveGuardTaskUser"));
        }
        Ok(kind == SidTypeUser && sid_string(sid.as_mut_ptr().cast())? == expected)
    }
}
struct Descriptor(PSECURITY_DESCRIPTOR);
impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: conversion API allocates with LocalAlloc.
        unsafe {
            LocalFree(self.0);
        }
    }
}
pub(super) fn verify(sddl: &str, sid: &str) -> Result<()> {
    if sddl.len() > 65536 {
        return Err(Error::Invalid("GUARD_TASK_ACL_SIZE"));
    }
    let input = wide(std::ffi::OsStr::new(sddl))?;
    // SAFETY: Windows validates and allocates the descriptor. All ACE/SID
    // pointers remain within that live allocation until the final comparison.
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            input.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(last_error("ParseGuardTaskSecurity"));
        }
        let descriptor = Descriptor(descriptor);
        let mut owner = ptr::null_mut();
        let mut acl = ptr::null_mut();
        let (mut defaulted, mut present, mut control, mut revision) = (0, 0, 0, 0);
        if GetSecurityDescriptorOwner(descriptor.0, &mut owner, &mut defaulted) == 0
            || GetSecurityDescriptorDacl(descriptor.0, &mut present, &mut acl, &mut defaulted) == 0
            || GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) == 0
        {
            return Err(last_error("ReadGuardTaskSecurity"));
        }
        if owner.is_null()
            || sid_string(owner)? != "S-1-5-32-544"
            || present == 0
            || acl.is_null()
            || control & SE_DACL_PROTECTED == 0
            || (*acl).AceCount != 3
        {
            return Err(Error::Invalid("GUARD_TASK_UNPROTECTED"));
        }
        let mut seen = std::collections::HashSet::new();
        for index in 0..(*acl).AceCount {
            let mut entry = ptr::null_mut();
            if GetAce(acl, index as u32, &mut entry) == 0 {
                return Err(last_error("ReadGuardTaskAce"));
            }
            let header = &*entry.cast::<ACE_HEADER>();
            if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE || header.AceFlags != 0 {
                return Err(Error::Invalid("GUARD_TASK_UNSAFE_ACE"));
            }
            let ace = &*entry.cast::<ACCESS_ALLOWED_ACE>();
            let trustee = sid_string((&ace.SidStart as *const u32).cast_mut().cast())?;
            let allowed = if trustee == "S-1-5-18" || trustee == "S-1-5-32-544" {
                ace.Mask == FILE_ALL_ACCESS
            } else if trustee == sid {
                ace.Mask == GENERIC_READ | GENERIC_EXECUTE
                    || ace.Mask == FILE_GENERIC_READ | FILE_GENERIC_EXECUTE
            } else {
                false
            };
            if !allowed || !seen.insert(trustee) {
                return Err(Error::Invalid("GUARD_TASK_UNSAFE_RIGHTS"));
            }
        }
    }
    Ok(())
}
unsafe fn sid_string(sid: PSID) -> Result<String> {
    // SAFETY: caller supplies SID from a retained Windows security descriptor.
    unsafe {
        let mut value = ptr::null_mut();
        if ConvertSidToStringSidW(sid, &mut value) == 0 {
            return Err(last_error("ConvertGuardTaskSid"));
        }
        let mut length = 0;
        while *value.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(value, length));
        LocalFree(value.cast());
        result.map_err(|_| Error::Invalid("GUARD_TASK_SID"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn task_descriptor_allows_only_admin_write_and_bound_user_read_run() {
        let sid = crate::identity::current().unwrap().user_sid;
        let good = sddl(&sid);
        verify(&good, &sid).unwrap();
        verify(&good.replace("GRGX", "FRFX"), &sid).unwrap();
        for descriptor in [
            good.replace("GRGX", "GA"),
            good.replace("GRGX", "GRGWGX"),
            good.replace("O:BA", &format!("O:{sid}")),
            good.replace("D:P", "D:"),
            good.replace(&sid, "WD"),
            good.replace("(A;;FA;;;BA)", ""),
            format!("{good}(A;;FA;;;BA)"),
            good.replace("(A;;", "(A;OI;"),
        ] {
            assert!(verify(&descriptor, &sid).is_err(), "{descriptor}");
        }
    }
}
