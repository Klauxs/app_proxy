//! Foreground-only authorization of a fixed listener installer. No arbitrary
//! elevated executable, directory, command, or writable request file is accepted.
use crate::{
    Error, Result, creation_guard,
    guard_deployment::{Deployment, InstallerSource, SourceExpectation},
    guard_task, identity, last_error, wide,
};
use app_proxy_core::ProcessIdentity;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    time::Duration,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::*,
    System::{Com::*, Threading::*},
    UI::{Shell::*, WindowsAndMessaging::SW_HIDE},
};

const TICKET_LIMIT: usize = 8192;
const INSTALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Ticket {
    version: u32,
    store: Uuid,
    issuer: ProcessIdentity,
    source: SourceExpectation,
}

/// Called only by the fixed host guard-install entry point after UAC. The
/// expected source was captured before consent and cannot be reselected here.
pub fn elevated(encoded: &str) -> Result<()> {
    identity::assert_elevated_user()?;
    let ticket = decode(encoded)?;
    Deployment::install_listener(ticket.store, &ticket.issuer, &ticket.source)?;
    Ok(())
}

/// Synchronous foreground worker. UAC cancellation is final for this attempt;
/// after dispatch, errors/timeouts mean unconfirmed, never automatic reexecution.
pub fn authorize_listener(store: Uuid) -> Result<Uuid> {
    identity::assert_ordinary_user()?;
    if store.is_nil() {
        return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
    }
    let source = InstallerSource::capture()?;
    match Deployment::listener(store) {
        Ok(deployment) => {
            if !source.matches_listener(&deployment) {
                return Err(Error::Invalid("GUARD_LISTENER_RELEASE_CONFLICT"));
            }
            match guard_task::verify_registered(&deployment) {
                Ok(()) => return Ok(deployment.generation()),
                Err(Error::Invalid("GUARD_TASK_MISSING")) => {}
                Err(error) => return Err(error),
            }
        }
        Err(Error::Invalid("GUARD_LISTENER_MISSING")) => {}
        Err(error) => return Err(error),
    }
    let ticket = Ticket {
        version: 1,
        store,
        issuer: identity::current()?,
        source: source.expectation(),
    };
    let encoded = encode(&ticket)?;
    creation_guard::ensure_plain_creation(source.path())?;
    let process = launch(&source, &encoded)?;
    let completed = wait(&process, INSTALL_TIMEOUT)?;
    if completed.is_none() {
        // Keep the exact source and parent pins while a slow COM installation
        // finishes, even after this worker returns an unknown result. If the
        // foreground process itself exits, the issuer-liveness checks still
        // prohibit a delayed installer from beginning a new stage/register.
        let _ = std::thread::Builder::new()
            .name("guard-install-wait".into())
            .spawn(move || {
                let _source = source;
                while let Ok(None) = wait(&process, Duration::from_secs(1)) {}
            });
        return Err(Error::Invalid("GUARD_INSTALL_RESULT_UNKNOWN"));
    }
    // A successful process exit is not installation evidence; a nonzero exit
    // after registration is not proof that the task was never created either.
    let deployment =
        Deployment::listener(store).map_err(|_| Error::Invalid("GUARD_INSTALL_NOT_CONFIRMED"))?;
    if !source.matches_listener(&deployment) {
        return Err(Error::Invalid("GUARD_LISTENER_RELEASE_CONFLICT"));
    }
    guard_task::verify_registered(&deployment)?;
    Ok(deployment.generation())
}

fn encode(ticket: &Ticket) -> Result<String> {
    let bytes = serde_json::to_vec(ticket)?;
    if bytes.len() * 2 > TICKET_LIMIT {
        return Err(Error::Invalid("GUARD_INSTALL_TICKET_SIZE"));
    }
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        write!(output, "{byte:02x}").map_err(|_| Error::Invalid("GUARD_INSTALL_ENCODING"))?;
    }
    Ok(output)
}
fn decode(encoded: &str) -> Result<Ticket> {
    if encoded.is_empty() || encoded.len() > TICKET_LIMIT || !encoded.len().is_multiple_of(2) {
        return Err(Error::Invalid("GUARD_INSTALL_TICKET_SIZE"));
    }
    fn digit(byte: u8) -> Result<u8> {
        match byte {
            b'0'..=b'9' => Ok(byte - b'0'),
            b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(Error::Invalid("GUARD_INSTALL_ENCODING")),
        }
    }
    let bytes: Vec<u8> = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok(digit(pair[0])? * 16 + digit(pair[1])?))
        .collect::<Result<_>>()?;
    let ticket: Ticket = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Invalid("INVALID_GUARD_INSTALL_TICKET"))?;
    if ticket.version != 1
        || ticket.store.is_nil()
        || ticket.issuer.pid == 0
        || ticket.issuer.creation_time == 0
        || ticket.issuer.session_id == 0
    {
        return Err(Error::Invalid("INVALID_GUARD_INSTALL_TICKET"));
    }
    Ok(ticket)
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balanced successful initialization on this same thread.
        unsafe { CoUninitialize() };
    }
}
fn launch(source: &InstallerSource, encoded: &str) -> Result<OwnedHandle> {
    // All parameter characters are fixed ASCII or strict lowercase hex; no
    // shell, escaping of user strings, env substitution or request path.
    decode(encoded)?;
    let verb = wide(OsStr::new("runas"))?;
    let file = wide(source.path().as_os_str())?;
    let directory = wide(
        source
            .path()
            .parent()
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?
            .as_os_str(),
    )?;
    let parameters = wide(OsStr::new(&format!("guard-install --ticket {encoded}")))?;
    // SAFETY: no existing COM objects escape this worker; ShellExecuteEx needs
    // initialization and NOASYNC because this thread has no shell message loop.
    let code = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
    if code < 0 {
        return Err(Error::Windows {
            operation: "InitializeGuardElevation",
            code: code as u32,
        });
    }
    let _apartment = Apartment;
    let mut info = SHELLEXECUTEINFOW {
        cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: parameters.as_ptr(),
        lpDirectory: directory.as_ptr(),
        nShow: SW_HIDE,
        ..Default::default()
    };
    // SAFETY: fixed executable and parameter buffers, retained source pins; the
    // OS consent UI is explicitly requested by the foreground user's choice.
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        let error = last_error("AuthorizeGuardListener");
        return match error {
            Error::Windows {
                code: ERROR_CANCELLED,
                ..
            } => Err(Error::Invalid("GUARD_INSTALL_CANCELLED")),
            other => Err(other),
        };
    }
    if info.hProcess.is_null() {
        return Err(Error::Invalid("GUARD_INSTALL_RESULT_UNKNOWN"));
    }
    // SAFETY: NOCLOSEPROCESS transfers the returned process handle to this caller.
    Ok(unsafe { OwnedHandle::from_raw_handle(info.hProcess) })
}

fn wait(process: &OwnedHandle, timeout: Duration) -> Result<Option<u32>> {
    // SAFETY: retained ShellExecute process handle includes synchronization and
    // query access. No termination or PID lookup is performed.
    match unsafe {
        WaitForSingleObject(
            process.as_raw_handle(),
            timeout.as_millis().min(u32::MAX as u128) as u32,
        )
    } {
        WAIT_TIMEOUT => Ok(None),
        WAIT_OBJECT_0 => {
            let mut code = 0;
            // SAFETY: the exact process handle remains retained after exit.
            if unsafe { GetExitCodeProcess(process.as_raw_handle(), &mut code) } == 0 {
                return Err(last_error("GuardInstallerExitCode"));
            }
            Ok(Some(code))
        }
        _ => Err(last_error("WaitGuardInstaller")),
    }
}

#[cfg(test)]
mod tests;
