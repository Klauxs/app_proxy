//! Foreground console attachment, failure notification and secret input echo.
use crate::{Result, last_error};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::Console::{
        ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    },
};

/// Used only by the GUI launcher when an accepted launch needs user input.
/// Call before installing Ctrl+C handlers: AllocConsole resets their table.
/// Do not use in a coordinator or concurrently with other foreground flows.
pub struct ForegroundConsole {
    previous: [usize; 3],
    files: Option<[std::fs::File; 2]>,
    allocated: bool,
}
impl ForegroundConsole {
    pub fn open() -> Result<Self> {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::System::Console::*;
        // SAFETY: read this process's borrowed console/standard handles only.
        let mut console = unsafe {
            Self {
                previous: [
                    GetStdHandle(STD_INPUT_HANDLE) as usize,
                    GetStdHandle(STD_OUTPUT_HANDLE) as usize,
                    GetStdHandle(STD_ERROR_HANDLE) as usize,
                ],
                files: None,
                allocated: false,
            }
        };
        // SAFETY: allocation affects only this GUI foreground process. Never
        // detach an existing console, which might belong to the user's terminal.
        unsafe {
            if GetConsoleWindow().is_null() {
                if AllocConsole() == 0 {
                    return Err(last_error("OPEN_FOREGROUND_CONSOLE"));
                }
                console.allocated = true;
            }
        }
        let open = |name| {
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .share_mode(3)
                .open(name)
        };
        console.files = Some([open("CONIN$")?, open("CONOUT$")?]);
        let files = console.files.as_ref().unwrap();
        // SAFETY: owned handles remain open until Drop. Explicitly bind them:
        // redirected STARTF_USESTDHANDLES may survive AllocConsole.
        unsafe {
            for (slot, handle) in [
                (STD_INPUT_HANDLE, files[0].as_raw_handle()),
                (STD_OUTPUT_HANDLE, files[1].as_raw_handle()),
                (STD_ERROR_HANDLE, files[1].as_raw_handle()),
            ] {
                if SetStdHandle(slot, handle) == 0 {
                    return Err(last_error("BIND_FOREGROUND_CONSOLE"));
                }
            }
        }
        Ok(console)
    }
}
impl Drop for ForegroundConsole {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Console::*;
        // SAFETY: restore borrowed handles; only close files/allocation we own.
        unsafe {
            for (slot, handle) in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE]
                .into_iter()
                .zip(self.previous)
            {
                SetStdHandle(slot, handle as HANDLE);
            }
            drop(self.files.take());
            if self.allocated {
                FreeConsole();
            }
        }
    }
}

/// Keep the failure readable until acknowledged, even without a terminal.
pub fn notify_launch_failure(message: &str) -> Result<()> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let message = crate::wide(std::ffi::OsStr::new(message))?;
    let title = crate::wide(std::ffi::OsStr::new("App Proxy：启动未完成"))?;
    // SAFETY: terminated buffers live throughout the synchronous system dialog.
    if unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONWARNING | MB_SETFOREGROUND | MB_TASKMODAL,
        )
    } == 0
    {
        return Err(last_error("LAUNCH_NOTIFICATION_FAILED"));
    }
    Ok(())
}

#[cfg(feature = "test-support")]
pub mod test_support {
    use super::*;
    use windows_sys::Win32::System::Console::*;

    fn exclusive_console() -> Result<()> {
        let mut processes = [0; 2];
        // SAFETY: writable bounded PID array. Test input is permitted only in a
        // console containing exactly this fixture, never another user process.
        if unsafe { GetConsoleProcessList(processes.as_mut_ptr(), 2) } != 1
            || processes[0] != std::process::id()
        {
            return Err(crate::Error::Invalid("TEST_CONSOLE_NOT_EXCLUSIVE"));
        }
        Ok(())
    }
    pub fn line(input: &str) -> Result<()> {
        exclusive_console()?;
        let events: Vec<_> = input
            .encode_utf16()
            .map(|value| INPUT_RECORD {
                EventType: KEY_EVENT as u16,
                Event: INPUT_RECORD_0 {
                    KeyEvent: KEY_EVENT_RECORD {
                        bKeyDown: 1,
                        wRepeatCount: 1,
                        wVirtualKeyCode: if value == 13 { 13 } else { 0 },
                        wVirtualScanCode: 0,
                        uChar: KEY_EVENT_RECORD_0 { UnicodeChar: value },
                        dwControlKeyState: 0,
                    },
                },
            })
            .collect();
        let mut written = 0;
        // SAFETY: owned exclusive test console and live bounded event array.
        if unsafe {
            WriteConsoleInputW(
                GetStdHandle(STD_INPUT_HANDLE),
                events.as_ptr(),
                events.len() as u32,
                &mut written,
            )
        } == 0
            || written as usize != events.len()
        {
            return Err(last_error("TEST_CONSOLE_INPUT"));
        }
        Ok(())
    }
    pub fn ctrl_c() -> Result<()> {
        exclusive_console()?;
        // SAFETY: sole attached process is this test fixture.
        if unsafe { GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) } == 0 {
            return Err(last_error("TEST_CONSOLE_CTRL_C"));
        }
        Ok(())
    }
}

pub struct HiddenInput {
    handle: HANDLE,
    mode: u32,
}
impl HiddenInput {
    pub fn begin() -> Result<Self> {
        // SAFETY: STD_INPUT_HANDLE is a defined selector; this borrows the
        // process standard handle without transferring ownership.
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        // SAFETY: mode is writable and live for the call. The API validates
        // whether the borrowed standard handle refers to a console.
        if unsafe { GetConsoleMode(handle, &mut mode) } == 0 {
            return Err(last_error("READ_CONSOLE_MODE"));
        }
        // SAFETY: GetConsoleMode confirmed a console handle; preserve every
        // existing input flag except echo for this foreground read.
        if unsafe { SetConsoleMode(handle, mode & !ENABLE_ECHO_INPUT) } == 0 {
            return Err(last_error("HIDE_CONSOLE_INPUT"));
        }
        Ok(Self { handle, mode })
    }
}
impl Drop for HiddenInput {
    fn drop(&mut self) {
        // SAFETY: the guard borrows, never closes, the standard handle. Restore
        // its captured valid mode even when Ctrl+C abandons the reader thread.
        unsafe {
            SetConsoleMode(self.handle, self.mode);
        }
    }
}

#[cfg(test)]
mod tests;
