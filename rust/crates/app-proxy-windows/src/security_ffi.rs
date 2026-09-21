//! Ownership wrappers for the two `LocalAlloc` results every security check uses.
use crate::{Error, Result, last_error};
use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, PSID};

/// A security descriptor allocated by Windows and released with `LocalFree`.
pub(crate) struct Descriptor(pub(crate) PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: the conversion and query APIs that produce this pointer
        // allocate it with LocalAlloc, and this wrapper is its only owner.
        unsafe {
            LocalFree(self.0);
        }
    }
}

/// Textual form of a SID. `operation` labels a Win32 failure and `invalid` is
/// the caller's code for text that is not valid UTF-16.
///
/// # Safety
/// `sid` must point to a valid SID that stays alive for the call.
pub(crate) unsafe fn sid_string(
    sid: PSID,
    operation: &'static str,
    invalid: &'static str,
) -> Result<String> {
    let mut text = std::ptr::null_mut();
    // SAFETY: the caller guarantees `sid`; `text` is a valid output slot.
    if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
        return Err(last_error(operation));
    }
    // SAFETY: on success `text` is a NUL-terminated UTF-16 string allocated by
    // Windows. It is read up to its terminator and then released exactly once.
    let result = unsafe {
        let mut length = 0;
        while *text.add(length) != 0 {
            length += 1;
        }
        let result = String::from_utf16(std::slice::from_raw_parts(text, length));
        LocalFree(text.cast());
        result
    };
    result.map_err(|_| Error::Invalid(invalid))
}
