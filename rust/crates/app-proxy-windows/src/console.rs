//! Temporarily suppress console echo while a foreground client reads a secret.
use crate::{Result, last_error};
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::Console::{
        ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    },
};

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
