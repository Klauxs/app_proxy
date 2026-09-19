//! Bounded, exact-process stop primitive. Callers must establish instance ownership
//! separately; this module never discovers descendants or terminates by name.
use crate::{Error, Result, identity, last_error};
use app_proxy_core::ProcessIdentity;
use std::{
    os::windows::io::{AsRawHandle, OwnedHandle},
    time::{Duration, Instant},
};
use windows_sys::Win32::{Foundation::*, System::Threading::*, UI::WindowsAndMessaging::*};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopOutcome {
    AlreadyExited,
    Exited,
    Forced,
    StillRunning,
}

/// Blocking platform operation: run off the coordinator's async executor and
/// configuration lock. Close messages have a shared one-second budget, followed
/// by 1.5 seconds for exit. Force, when explicitly selected by the caller,
/// obtains termination rights only after that grace period and rechecks identity.
/// Success means this one process exited, not that its descendants have exited.
pub fn stop_exact(expected: &ProcessIdentity, force: bool) -> Result<StopOutcome> {
    identity::assert_ordinary_user()?;
    let caller = identity::current()?;
    if expected.pid == 0
        || expected.pid == caller.pid
        || expected.creation_time == 0
        || expected.user_sid != caller.user_sid
        || expected.session_id != caller.session_id
    {
        return Err(Error::IdentityMismatch);
    }
    let handle = match identity::open(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
    ) {
        Ok(handle) => handle,
        Err(Error::Windows {
            code: ERROR_INVALID_PARAMETER,
            ..
        }) => return Ok(StopOutcome::AlreadyExited),
        Err(error) => return Err(error),
    };
    if exited(&handle, 0)? {
        return Ok(StopOutcome::AlreadyExited);
    }
    // A different creation time proves PID reuse, never permission to affect it.
    // SAFETY: this owned query handle remains live throughout inspection.
    let actual = unsafe { identity::inspect_handle(handle.as_raw_handle())? };
    if actual.creation_time != expected.creation_time {
        return Ok(StopOutcome::AlreadyExited);
    }
    if actual != *expected {
        return Err(Error::IdentityMismatch);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    let windows = windows_for(expected.pid, deadline)?;
    for window in windows {
        if exited(&handle, 0)? {
            return Ok(StopOutcome::Exited);
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            break;
        };
        // The process handle remains pinned; window ownership is checked again
        // immediately before the best-effort close request. A send result is
        // never interpreted as proof of process exit.
        verify(&handle, expected)?;
        let mut pid = 0;
        // SAFETY: output storage is valid; HWND is only a transient hint.
        if unsafe { GetWindowThreadProcessId(window, &mut pid) } == 0 || pid != expected.pid {
            continue;
        }
        // SAFETY: WM_CLOSE has no borrowed payload. Never broadcast, never ignore
        // the timeout for a responsive thread, and never pump incoming messages.
        unsafe {
            SendMessageTimeoutW(
                window,
                WM_CLOSE,
                0,
                0,
                SMTO_ABORTIFHUNG | SMTO_BLOCK | SMTO_ERRORONEXIT,
                remaining.as_millis().clamp(1, 100) as u32,
                std::ptr::null_mut(),
            );
        }
    }
    if exited(&handle, 1500)? {
        return Ok(StopOutcome::Exited);
    }
    if !force {
        return Ok(StopOutcome::StillRunning);
    }
    verify(&handle, expected)?;
    let terminator = match identity::open(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
    ) {
        Ok(handle) => handle,
        Err(_) if exited(&handle, 0)? => return Ok(StopOutcome::Exited),
        Err(error) => return Err(error),
    };
    if exited(&handle, 0)? {
        return Ok(StopOutcome::Exited);
    }
    verify(&terminator, expected)?;
    // SAFETY: complete identity was just verified on this retained exact handle.
    if unsafe { TerminateProcess(terminator.as_raw_handle(), 1) } == 0 {
        let error = last_error("TerminateApplication");
        if exited(&handle, 0)? {
            return Ok(StopOutcome::Exited);
        }
        return Err(error);
    }
    if !exited(&terminator, 3000)? {
        return Err(Error::Invalid("PROCESS_STOP_UNCONFIRMED"));
    }
    Ok(StopOutcome::Forced)
}

fn exited(handle: &OwnedHandle, timeout: u32) -> Result<bool> {
    // SAFETY: a retained process handle with SYNCHRONIZE and a bounded wait.
    match unsafe { WaitForSingleObject(handle.as_raw_handle(), timeout) } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(last_error("WaitForApplicationExit")),
    }
}

fn verify(handle: &OwnedHandle, expected: &ProcessIdentity) -> Result<()> {
    // SAFETY: the borrowed handle stays live throughout the identity query.
    if unsafe { identity::inspect_handle(handle.as_raw_handle())? } != *expected {
        return Err(Error::IdentityMismatch);
    }
    Ok(())
}

fn windows_for(pid: u32, deadline: Instant) -> Result<Vec<HWND>> {
    struct Enumeration {
        pid: u32,
        deadline: Instant,
        windows: Vec<HWND>,
        limited: bool,
    }
    unsafe extern "system" fn collect(window: HWND, parameter: LPARAM) -> i32 {
        // SAFETY: EnumWindows is synchronous; parameter points to the live stack
        // record below. No references or window handles escape this call's owner.
        let state = unsafe { &mut *(parameter as *mut Enumeration) };
        if Instant::now() >= state.deadline || state.windows.len() >= 128 {
            state.limited = true;
            return 0;
        }
        let mut pid = 0;
        // SAFETY: valid output storage; no dereference of the transient HWND.
        if unsafe { GetWindowThreadProcessId(window, &mut pid) } != 0 && pid == state.pid {
            state.windows.push(window);
        }
        1
    }
    let mut state = Enumeration {
        pid,
        deadline,
        windows: Vec::new(),
        limited: false,
    };
    // SAFETY: stack record outlives the synchronous callback enumeration.
    let result = unsafe { EnumWindows(Some(collect), (&mut state as *mut Enumeration) as LPARAM) };
    if state.limited {
        return Err(Error::Invalid("PROCESS_WINDOW_LIMIT"));
    }
    if result == 0 {
        return Err(last_error("EnumerateApplicationWindows"));
    }
    Ok(state.windows)
}

#[cfg(test)]
mod tests;
