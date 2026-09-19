use crate::{Error, Result, identity, last_error};
use app_proxy_core::{EnvPatch, ProcessIdentity};
use std::ffi::OsString;
use std::mem::zeroed;
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::System::Diagnostics::Debug::*;
use windows_sys::Win32::System::Threading::*;

/// Experimental until actual IFEO registration and target application tests pass.
#[derive(Clone, Copy)]
pub enum CreationMode {
    Normal,
    DebugDetach,
}

// No Debug: arguments and environment may contain secrets.
pub struct SpawnSpec {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: EnvPatch,
    pub mode: CreationMode,
}

pub struct StartedProcess {
    child: Child,
    pub identity: ProcessIdentity,
}

impl StartedProcess {
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.child.try_wait()?)
    }

    pub fn wait_timeout(&mut self, timeout: Duration) -> Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.try_wait()? {
                return Ok(Some(status));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Fixture/failed-start cleanup only. Production stop will first close windows.
    pub fn terminate(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }
        // SAFETY: child retains the exact original process handle, not a re-opened PID.
        let actual = unsafe { identity::inspect_handle(self.child.as_raw_handle())? };
        if actual != self.identity {
            return Err(Error::IdentityMismatch);
        }
        self.child.kill()?;
        self.child.wait()?;
        Ok(())
    }
}

/// Used by probes to verify that a recycled or forged identity cannot stop a process.
/// This is not the eventual user-facing graceful stop operation.
pub fn terminate_exact(expected: &ProcessIdentity) -> Result<()> {
    let caller = identity::current()?;
    if expected.user_sid != caller.user_sid || expected.session_id != caller.session_id {
        return Err(Error::IdentityMismatch);
    }
    let handle = identity::open(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
    )?;
    // SAFETY: handle is held across the identity check, termination, and bounded wait.
    unsafe {
        if identity::inspect_handle(handle.as_raw_handle())? != *expected {
            return Err(Error::IdentityMismatch);
        }
        if TerminateProcess(handle.as_raw_handle(), 1) == 0 {
            return Err(last_error("TerminateProcess"));
        }
        if WaitForSingleObject(handle.as_raw_handle(), 3000) != WAIT_OBJECT_0 {
            return Err(Error::Invalid("PROCESS_STOP_UNCONFIRMED"));
        }
    }
    Ok(())
}

pub fn spawn(spec: SpawnSpec) -> Result<StartedProcess> {
    identity::assert_ordinary_user()?;
    spec.environment.validate()?;
    if !spec.exe.is_absolute()
        || !spec.cwd.is_absolute()
        || !spec
            .exe
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(Error::Invalid("ABSOLUTE_EXE_AND_CWD_REQUIRED"));
    }
    // Windows debug events and detach belong to the thread that created the child.
    // A short-lived thread also tests that successful detach lets the target survive.
    std::thread::spawn(move || spawn_on_thread(spec))
        .join()
        .map_err(|_| Error::Invalid("SPAWN_THREAD_PANICKED_RESULT_UNKNOWN"))?
}

fn spawn_on_thread(spec: SpawnSpec) -> Result<StartedProcess> {
    let expected_image = identity::file_identity(&spec.exe)?;
    let caller = identity::current()?;
    let debug = matches!(spec.mode, CreationMode::DebugDetach);
    let mut command = Command::new(&spec.exe);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for key in &spec.environment.unset {
        command.env_remove(key);
    }
    for (key, value) in &spec.environment.set {
        command.env(key, value);
    }
    if debug {
        command.creation_flags(DEBUG_ONLY_THIS_PROCESS);
    }
    let mut child = command.spawn()?;
    if debug {
        // SAFETY: this thread just established a debug relationship via CreateProcess.
        if unsafe { DebugSetProcessKillOnExit(0) } == 0 {
            let error = last_error("DebugSetProcessKillOnExit");
            cleanup_failed_debug(&mut child);
            return Err(error);
        }
    }
    // SAFETY: the Child owns and retains the created process query handle.
    let result = unsafe { identity::inspect_handle(child.as_raw_handle()) };
    let actual = match result {
        Ok(identity)
            if identity.image_file == expected_image
                && identity.user_sid == caller.user_sid
                && identity.session_id == caller.session_id =>
        {
            identity
        }
        Ok(_) => {
            if debug {
                cleanup_failed_debug(&mut child);
            } else {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(Error::IdentityMismatch);
        }
        Err(error) => {
            if debug {
                cleanup_failed_debug(&mut child);
            } else {
                let _ = child.kill();
                let _ = child.wait();
            }
            return Err(error);
        }
    };
    if debug && let Err(error) = detach_after_start(child.id(), Duration::from_secs(5)) {
        cleanup_failed_debug(&mut child);
        return Err(error);
    }
    Ok(StartedProcess {
        child,
        identity: actual,
    })
}

fn cleanup_failed_debug(child: &mut Child) {
    let _ = child.kill();
    // SAFETY: only the debuggee created by this thread is detached.
    unsafe {
        DebugActiveProcessStop(child.id());
    }
    // Bounded: an outstanding debug event can delay process exit.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if child.try_wait().ok().flatten().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn detach_after_start(pid: u32, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    let mut created = false;
    loop {
        if Instant::now() >= deadline {
            return Err(Error::Invalid("DEBUG_START_TIMEOUT"));
        }
        // SAFETY: correctly sized event; same creating thread consumes all events.
        unsafe {
            let mut event: DEBUG_EVENT = zeroed();
            if WaitForDebugEventEx(&mut event, 50) == 0 {
                let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
                if code == ERROR_SEM_TIMEOUT {
                    continue;
                }
                return Err(Error::Windows {
                    operation: "WaitForDebugEventEx",
                    code,
                });
            }
            if event.dwProcessId != pid {
                return Err(Error::Invalid("UNEXPECTED_DEBUGGEE"));
            }
            let mut status = DBG_CONTINUE;
            let mut ready = false;
            match event.dwDebugEventCode {
                CREATE_PROCESS_DEBUG_EVENT => {
                    created = true;
                    let file = event.u.CreateProcessInfo.hFile;
                    if !file.is_null() {
                        CloseHandle(file);
                    }
                    // Debug process/thread handles are released by Windows on detach.
                }
                LOAD_DLL_DEBUG_EVENT => {
                    let file = event.u.LoadDll.hFile;
                    if !file.is_null() {
                        CloseHandle(file);
                    }
                }
                EXCEPTION_DEBUG_EVENT => {
                    if created
                        && event.u.Exception.ExceptionRecord.ExceptionCode == EXCEPTION_BREAKPOINT
                    {
                        ready = true;
                    } else {
                        status = DBG_EXCEPTION_NOT_HANDLED;
                    }
                }
                EXIT_PROCESS_DEBUG_EVENT => {
                    ContinueDebugEvent(event.dwProcessId, event.dwThreadId, DBG_CONTINUE);
                    return Err(Error::Invalid("TARGET_EXITED_BEFORE_DEBUG_DETACH"));
                }
                _ => {}
            }
            if ContinueDebugEvent(event.dwProcessId, event.dwThreadId, status) == 0 {
                return Err(last_error("ContinueDebugEvent"));
            }
            if ready {
                if DebugActiveProcessStop(pid) == 0 {
                    return Err(last_error("DebugActiveProcessStop"));
                }
                return Ok(());
            }
        }
    }
}
