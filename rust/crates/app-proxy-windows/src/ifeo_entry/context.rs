use crate::{Error, Result, identity, last_error};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
};
use windows_sys::Win32::System::SystemServices::SECURITY_MANDATORY_MEDIUM_RID;
use windows_sys::Win32::{Foundation::*, Security::*, System::Threading::*};

pub(super) fn ordinary_entry() -> Result<()> {
    identity::assert_ordinary_user()?;
    let current = identity::current()?;
    // SAFETY: queries only the current process token. The owned token and aligned
    // output storage remain live through every SID/label inspection below.
    unsafe {
        let mut raw = ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw) == 0 {
            return Err(last_error("OpenIfeoEntryToken"));
        }
        let token = OwnedHandle::from_raw_handle(raw);
        let mut label = [0usize; 64];
        let mut written = 0;
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenIntegrityLevel,
            label.as_mut_ptr().cast(),
            size_of::<[usize; 64]>() as u32,
            &mut written,
        ) == 0
        {
            return Err(last_error("ReadIfeoIntegrity"));
        }
        if written < size_of::<TOKEN_MANDATORY_LABEL>() as u32
            || written as usize > size_of_val(&label)
        {
            return Err(Error::Invalid("IFEO_CONTEXT_UNSUPPORTED"));
        }
        let label = &*label.as_ptr().cast::<TOKEN_MANDATORY_LABEL>();
        let sid = label.Label.Sid;
        if IsValidSid(sid) == 0 {
            return Err(Error::Invalid("IFEO_CONTEXT_UNSUPPORTED"));
        }
        let count = *GetSidSubAuthorityCount(sid);
        if count == 0 {
            return Err(Error::Invalid("IFEO_CONTEXT_UNSUPPORTED"));
        }
        let integrity = *GetSidSubAuthority(sid, u32::from(count - 1));
        let ui_access = flag(token.as_raw_handle(), TokenUIAccess)?;
        let app_container = flag(token.as_raw_handle(), TokenIsAppContainer)?;
        if !supported(current.session_id, integrity, ui_access, app_container) {
            return Err(Error::Invalid("IFEO_CONTEXT_UNSUPPORTED"));
        }
    }
    Ok(())
}
unsafe fn flag(token: HANDLE, kind: TOKEN_INFORMATION_CLASS) -> Result<u32> {
    let mut value = 0u32;
    let mut written = 0;
    // SAFETY: caller holds a token query handle and supplies a DWORD token class;
    // the writable output and its size match that class.
    if unsafe {
        GetTokenInformation(
            token,
            kind,
            (&mut value as *mut u32).cast(),
            size_of::<u32>() as u32,
            &mut written,
        )
    } == 0
    {
        return Err(last_error("ReadIfeoTokenContext"));
    }
    if written != size_of::<u32>() as u32 {
        return Err(Error::Invalid("IFEO_CONTEXT_UNSUPPORTED"));
    }
    Ok(value)
}
fn supported(session: u32, integrity: u32, ui_access: u32, app_container: u32) -> bool {
    session != 0
        && integrity == SECURITY_MANDATORY_MEDIUM_RID as u32
        && ui_access == 0
        && app_container == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::SystemServices::{
        SECURITY_MANDATORY_HIGH_RID, SECURITY_MANDATORY_LOW_RID,
    };
    #[test]
    fn only_interactive_medium_non_sandbox_context_is_supported() {
        assert!(supported(1, SECURITY_MANDATORY_MEDIUM_RID as u32, 0, 0));
        for (session, integrity, ui, container) in [
            (0, SECURITY_MANDATORY_MEDIUM_RID, 0, 0),
            (1, SECURITY_MANDATORY_LOW_RID, 0, 0),
            (1, SECURITY_MANDATORY_HIGH_RID, 0, 0),
            (1, SECURITY_MANDATORY_MEDIUM_RID, 1, 0),
            (1, SECURITY_MANDATORY_MEDIUM_RID, 0, 1),
        ] {
            assert!(!supported(session, integrity as u32, ui, container));
        }
        ordinary_entry().unwrap();
    }
}
