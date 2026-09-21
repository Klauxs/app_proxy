//! User-facing local timestamps; never used for ordering or identity.
pub fn name_timestamp() -> String {
    let mut time = windows_sys::Win32::Foundation::SYSTEMTIME::default();
    // SAFETY: the system writes one initialized SYSTEMTIME to this live buffer.
    unsafe {
        windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut time);
    }
    format!(
        "{:04}{:02}{:02}-{:02}{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute
    )
}
