use crate::{Error, Result, identity, last_error};
use app_proxy_core::{EnvPatch, ProcessIdentity};
use std::ffi::OsString;
use std::mem::zeroed;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
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

pub struct StartedHost {
    process: OwnedHandle,
    pub identity: ProcessIdentity,
}
impl StartedHost {
    pub fn has_exited(&self) -> Result<bool> {
        // SAFETY: the exact process handle remains owned throughout this zero-time wait.
        match unsafe { WaitForSingleObject(self.process.as_raw_handle(), 0) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            _ => Err(last_error("WaitForCoordinator")),
        }
    }
}

/// Fixed coordinator entry. Inherit no handles: std::Command on stable Windows
/// otherwise inherits the CLI's captured stdout, keeping its caller waiting for EOF.
pub fn start_host(exe: &std::path::Path, home: &std::path::Path) -> Result<StartedHost> {
    identity::assert_ordinary_user()?;
    if !exe.is_absolute() || !home.is_absolute() {
        return Err(Error::Invalid("ABSOLUTE_HOST_PATH_REQUIRED"));
    }
    let expected_image = identity::file_identity(exe)?;
    let caller = identity::current()?;
    let application = crate::wide(exe.as_os_str())?;
    let cwd = crate::wide(
        exe.parent()
            .ok_or(Error::Invalid("HOST_DIRECTORY_REQUIRED"))?
            .as_os_str(),
    )?;
    let mut command = Vec::new();
    for word in [
        exe.as_os_str(),
        std::ffi::OsStr::new("serve"),
        std::ffi::OsStr::new("--home"),
        home.as_os_str(),
    ] {
        if !command.is_empty() {
            command.push(b' ' as u16);
        }
        command.extend(quote_windows_word(word)?);
    }
    command.push(0);
    if command.len() > 32767 {
        return Err(Error::Invalid("HOST_COMMAND_TOO_LONG"));
    }
    // SAFETY: all buffers terminated and live; no inherited handles or security pointers.
    unsafe {
        let mut startup: STARTUPINFOW = zeroed();
        startup.cb = std::mem::size_of::<STARTUPINFOW>() as u32;
        let mut info: PROCESS_INFORMATION = zeroed();
        if CreateProcessW(
            application.as_ptr(),
            command.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW,
            std::ptr::null(),
            cwd.as_ptr(),
            &startup,
            &mut info,
        ) == 0
        {
            return Err(last_error("StartCoordinator"));
        }
        let process = OwnedHandle::from_raw_handle(info.hProcess);
        let thread = OwnedHandle::from_raw_handle(info.hThread);
        drop(thread);
        let inspected = identity::inspect_handle(process.as_raw_handle());
        match inspected {
            Ok(actual)
                if actual.image_file == expected_image
                    && actual.user_sid == caller.user_sid
                    && actual.session_id == caller.session_id =>
            {
                Ok(StartedHost {
                    process,
                    identity: actual,
                })
            }
            _ => {
                // Exact newly-created handle only; never terminate an unrelated owner.
                TerminateProcess(process.as_raw_handle(), 1);
                WaitForSingleObject(process.as_raw_handle(), 3000);
                Err(Error::Invalid("COORDINATOR_IDENTITY_UNCONFIRMED"))
            }
        }
    }
}

fn quote_windows_word(word: &std::ffi::OsStr) -> Result<Vec<u16>> {
    let units = crate::wide(word)?;
    let mut output = vec![b'"' as u16];
    let mut slashes = 0;
    for &unit in &units[..units.len() - 1] {
        if unit == b'\\' as u16 {
            slashes += 1;
            continue;
        }
        output.extend(std::iter::repeat_n(
            b'\\' as u16,
            if unit == b'"' as u16 {
                slashes * 2 + 1
            } else {
                slashes
            },
        ));
        slashes = 0;
        output.push(unit);
    }
    output.extend(std::iter::repeat_n(b'\\' as u16, slashes * 2));
    output.push(b'"' as u16);
    Ok(output)
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
