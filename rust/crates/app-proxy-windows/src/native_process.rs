//! One exact process object, held from argument observation through Guard stop.
//! Command lines never leave the ordinary process or become serialized evidence.
use crate::{Error, Result, identity, last_error, process_query};
use app_proxy_core::ProcessIdentity;
use std::{
    ffi::{OsString, c_void},
    mem::size_of,
    os::windows::io::{AsRawHandle, OwnedHandle},
};
use windows_sys::Win32::{Foundation::*, System::Threading::*};

#[repr(C)]
struct UnicodeString {
    length: u16,
    maximum_length: u16,
    buffer: *const u16,
}

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryInformationProcess(
        process: HANDLE,
        class: u32,
        information: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
    fn RtlNtStatusToDosError(status: i32) -> u32;
}

pub struct PinnedProcess {
    handle: OwnedHandle,
    identity: ProcessIdentity,
    arguments: Option<Vec<OsString>>,
}
impl PinnedProcess {
    pub fn open(pid: u32) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let caller = identity::current()?;
        if pid == 0 || pid == caller.pid {
            return Err(Error::IdentityMismatch);
        }
        let handle = identity::open(
            pid,
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
        )?;
        // SAFETY: the owned query handle remains retained by this object.
        let observed = unsafe { identity::inspect_handle(handle.as_raw_handle())? };
        if observed.user_sid != caller.user_sid || observed.session_id != caller.session_id {
            return Err(Error::IdentityMismatch);
        }
        let value = Self {
            handle,
            identity: observed,
            arguments: None,
        };
        value.verify()?;
        Ok(value)
    }
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }
    pub fn arguments(&self) -> Option<&[OsString]> {
        self.arguments.as_deref()
    }
    pub fn verify(&self) -> Result<()> {
        // SAFETY: this object retains the exact query/synchronize handle.
        unsafe {
            match WaitForSingleObject(self.handle.as_raw_handle(), 0) {
                WAIT_OBJECT_0 => return Err(Error::Invalid("GUARD_TARGET_EXITED_BEFORE_STOP")),
                WAIT_TIMEOUT => {}
                _ => return Err(last_error("ObserveGuardTarget")),
            }
            if identity::inspect_handle(self.handle.as_raw_handle())? != self.identity {
                return Err(Error::IdentityMismatch);
            }
        }
        Ok(())
    }
    pub fn read_arguments(&mut self) -> Result<()> {
        let _timing = crate::diagnostic_timing::Span::new("native.command_line", || {
            self.identity.pid.to_string()
        });
        self.verify()?;
        let mut bytes = 1024usize;
        for _ in 0..3 {
            if bytes > 128 * 1024 || bytes < size_of::<UnicodeString>() {
                return Err(Error::Invalid("PROCESS_QUERY_TEXT_LIMIT"));
            }
            // Word alignment is sufficient for UNICODE_STRING on both architectures.
            let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            let capacity = storage.len() * size_of::<usize>();
            let mut returned = 0;
            // SAFETY: exact retained process; aligned writable allocation with its
            // real size, and valid return-length storage. Class 60 is command line.
            let status = unsafe {
                NtQueryInformationProcess(
                    self.handle.as_raw_handle(),
                    60,
                    storage.as_mut_ptr().cast(),
                    capacity as u32,
                    &mut returned,
                )
            };
            if matches!(status as u32, 0xC0000004 | 0xC0000023 | 0x80000005) {
                if returned as usize <= capacity {
                    return Err(Error::Invalid("INVALID_NATIVE_COMMAND_LINE"));
                }
                bytes = returned as usize;
                continue;
            }
            if status < 0 {
                // SAFETY: pure NTSTATUS conversion, no pointer parameters.
                let code = unsafe { RtlNtStatusToDosError(status) };
                return Err(Error::Windows {
                    operation: "QueryProcessCommandLine",
                    code,
                });
            }
            let words = decode(&storage, returned as usize)?;
            let arguments = process_query::parse_arguments(&words)?;
            if arguments.is_none() {
                return Err(Error::Invalid("PROCESS_COMMAND_LINE_NOT_READY"));
            }
            self.verify()?;
            self.arguments = arguments;
            return Ok(());
        }
        Err(Error::Invalid("PROCESS_COMMAND_LINE_CHANGED"))
    }
    pub(crate) fn terminate(
        &self,
        expected: &ProcessIdentity,
    ) -> Result<crate::process_stop::StopOutcome> {
        if expected != &self.identity {
            return Err(Error::IdentityMismatch);
        }
        let timing =
            crate::diagnostic_timing::Span::new("stop.native_verify", || expected.pid.to_string());
        self.verify()?;
        drop(timing);
        let timing =
            crate::diagnostic_timing::Span::new("stop.terminate", || expected.pid.to_string());
        // SAFETY: same retained, verified process object; termination access was
        // acquired at open. No PID lookup or descendant termination is performed.
        if unsafe { TerminateProcess(self.handle.as_raw_handle(), 1) } == 0 {
            return Err(last_error("TerminateGuardTarget"));
        }
        drop(timing);
        let _timing =
            crate::diagnostic_timing::Span::new("stop.wait_exit", || expected.pid.to_string());
        // SAFETY: retained synchronize handle, bounded wait.
        if unsafe { WaitForSingleObject(self.handle.as_raw_handle(), 3000) } != WAIT_OBJECT_0 {
            return Err(Error::Invalid("PROCESS_STOP_UNCONFIRMED"));
        }
        Ok(crate::process_stop::StopOutcome::Forced)
    }
}

fn decode(storage: &[usize], returned: usize) -> Result<Vec<u16>> {
    let capacity = std::mem::size_of_val(storage);
    if returned < size_of::<UnicodeString>() || returned > capacity {
        return Err(Error::Invalid("INVALID_NATIVE_COMMAND_LINE"));
    }
    // SAFETY: aligned allocation contains at least the structure size. Pointer
    // content is validated against this allocation before ever dereferencing it.
    let text = unsafe { &*storage.as_ptr().cast::<UnicodeString>() };
    let start = storage.as_ptr() as usize;
    let pointer = text.buffer as usize;
    let length = text.length as usize;
    if length > 65534
        || !length.is_multiple_of(2)
        || text.length > text.maximum_length
        || !pointer.is_multiple_of(2)
        || pointer < start + size_of::<UnicodeString>()
        || pointer
            .checked_add(length)
            .is_none_or(|end| end > start + returned)
    {
        return Err(Error::Invalid("INVALID_NATIVE_COMMAND_LINE"));
    }
    // SAFETY: the preceding checks prove alignment and all bytes in bounds.
    Ok(unsafe { std::slice::from_raw_parts(text.buffer, length / 2) }.to_vec())
}

#[cfg(test)]
mod tests;
