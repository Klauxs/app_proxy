use crate::{Error, Result, last_error, wide};
use app_proxy_core::{FileIdentity, ProcessIdentity};
use std::ffi::OsString;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStringExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows_sys::Win32::Security::*;
use windows_sys::Win32::Storage::FileSystem::*;
use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFamilyName;
use windows_sys::Win32::System::RemoteDesktop::ProcessIdToSessionId;
use windows_sys::Win32::System::Threading::*;

pub fn current() -> Result<ProcessIdentity> {
    // SAFETY: the pseudo handle refers to this process and is not closed.
    unsafe { inspect_handle(GetCurrentProcess()) }
}

pub fn inspect(pid: u32) -> Result<ProcessIdentity> {
    let process = open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
    // SAFETY: process owns a valid query handle for the duration of inspection.
    unsafe { inspect_handle(process.as_raw_handle()) }
}

pub(crate) fn open(pid: u32, access: u32) -> Result<OwnedHandle> {
    // SAFETY: no borrowed pointers; handle ownership is transferred once.
    unsafe {
        let handle = OpenProcess(access, 0, pid);
        if handle.is_null() {
            return Err(last_error("OpenProcess"));
        }
        Ok(OwnedHandle::from_raw_handle(handle))
    }
}

pub fn assert_ordinary_user() -> Result<()> {
    // SAFETY: current-process pseudo handle remains valid for this call.
    unsafe { assert_ordinary_handle(GetCurrentProcess()) }
}

pub(crate) unsafe fn assert_ordinary_handle(process: RawHandle) -> Result<()> {
    // SAFETY: caller retains the process query handle.
    unsafe { assert_elevation_handle(process, false) }
}

pub fn assert_elevated_user() -> Result<()> {
    // SAFETY: current process pseudo handle remains valid and is not closed.
    unsafe { assert_elevation_handle(GetCurrentProcess(), true) }
}

unsafe fn assert_elevation_handle(process: RawHandle, elevated: bool) -> Result<()> {
    // SAFETY: querying a token we own, with a correctly sized output buffer.
    unsafe {
        let token = token(process)?;
        let mut elevation: TOKEN_ELEVATION = zeroed();
        let mut written = 0;
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut written,
        ) == 0
        {
            return Err(last_error("GetTokenInformation(elevation)"));
        }
        if (elevation.TokenIsElevated != 0) != elevated {
            return Err(Error::Invalid(if elevated {
                "ELEVATED_USER_REQUIRED"
            } else {
                "ORDINARY_USER_REQUIRED"
            }));
        }
        // Restricted/sandbox tokens are not an ordinary launch context.
        if IsTokenRestricted(token.as_raw_handle()) != 0 {
            return Err(Error::Invalid("RESTRICTED_TOKEN_UNSUPPORTED"));
        }
    }
    Ok(())
}

pub fn package_family() -> Result<Option<String>> {
    // SAFETY: two-call API; buffer capacity matches the requested UTF-16 length.
    unsafe {
        let mut length = 0;
        let result = GetCurrentPackageFamilyName(&mut length, null_mut());
        if result == APPMODEL_ERROR_NO_PACKAGE {
            return Ok(None);
        }
        if result != ERROR_INSUFFICIENT_BUFFER {
            return Err(Error::Windows {
                operation: "GetCurrentPackageFamilyName",
                code: result,
            });
        }
        let mut buffer = vec![0u16; length as usize];
        let result = GetCurrentPackageFamilyName(&mut length, buffer.as_mut_ptr());
        if result != 0 {
            return Err(Error::Windows {
                operation: "GetCurrentPackageFamilyName",
                code: result,
            });
        }
        buffer.truncate(length.saturating_sub(1) as usize);
        Ok(Some(
            String::from_utf16(&buffer).map_err(|_| Error::Invalid("NON_UNICODE_PACKAGE"))?,
        ))
    }
}

/// Full versioned identity; family alone cannot authorize an old package helper.
pub fn package_full_name() -> Result<Option<String>> {
    // SAFETY: the current-process pseudo handle is valid and not owned here.
    unsafe { package_full_name_handle(GetCurrentProcess()) }
}

pub(crate) unsafe fn package_full_name_handle(process: RawHandle) -> Result<Option<String>> {
    use windows_sys::Win32::Storage::Packaging::Appx::GetPackageFullName;
    // SAFETY: caller retains a process query handle; buffers follow the API sizes.
    unsafe {
        let mut length = 0;
        let result = GetPackageFullName(process, &mut length, null_mut());
        if result == APPMODEL_ERROR_NO_PACKAGE {
            return Ok(None);
        }
        if result != ERROR_INSUFFICIENT_BUFFER {
            return Err(Error::Windows {
                operation: "GetPackageFullName",
                code: result,
            });
        }
        let mut buffer = vec![0u16; length as usize];
        let result = GetPackageFullName(process, &mut length, buffer.as_mut_ptr());
        if result != 0 {
            return Err(Error::Windows {
                operation: "GetPackageFullName",
                code: result,
            });
        }
        buffer.truncate(length.saturating_sub(1) as usize);
        Ok(Some(
            String::from_utf16(&buffer).map_err(|_| Error::Invalid("NON_UNICODE_PACKAGE"))?,
        ))
    }
}

pub fn file_identity(path: &Path) -> Result<FileIdentity> {
    let path = wide(path.as_os_str())?;
    // SAFETY: path is terminated, structures are sized, the returned handle is owned.
    unsafe {
        let handle = CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateFileW(image)"));
        }
        let file = OwnedHandle::from_raw_handle(handle);
        let mut info: BY_HANDLE_FILE_INFORMATION = zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(last_error("GetFileInformationByHandle"));
        }
        Ok(FileIdentity {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        })
    }
}

unsafe fn token(process: RawHandle) -> Result<OwnedHandle> {
    // SAFETY: caller supplies a valid process handle; output becomes uniquely owned.
    unsafe {
        let mut handle = null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut handle) == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        Ok(OwnedHandle::from_raw_handle(handle))
    }
}

/// Caller must retain a valid process query handle throughout this call.
pub(crate) unsafe fn inspect_handle(process: RawHandle) -> Result<ProcessIdentity> {
    // SAFETY: caller guarantees handle lifetime; all FFI outputs are correctly sized.
    unsafe {
        let mut created: FILETIME = zeroed();
        let mut exited: FILETIME = zeroed();
        let mut kernel: FILETIME = zeroed();
        let mut user: FILETIME = zeroed();
        if GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
            return Err(last_error("GetProcessTimes"));
        }
        let pid = GetProcessId(process);
        let mut session_id = 0;
        if ProcessIdToSessionId(pid, &mut session_id) == 0 {
            return Err(last_error("ProcessIdToSessionId"));
        }
        let mut path = vec![0u16; 32768];
        let mut length = path.len() as u32;
        if QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) == 0 {
            return Err(last_error("QueryFullProcessImageNameW"));
        }
        path.truncate(length as usize);
        let image_path = PathBuf::from(OsString::from_wide(&path));
        let image_file = file_identity(&image_path)?;
        let token = token(process)?;
        let mut bytes = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes);
        if bytes == 0 || bytes > 65536 {
            return Err(last_error("GetTokenInformation(size)"));
        }
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            bytes,
            &mut bytes,
        ) == 0
        {
            return Err(last_error("GetTokenInformation(user)"));
        }
        let user = &*(buffer.as_ptr().cast::<TOKEN_USER>());
        let mut sid = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid) == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        let mut length = 0;
        while *sid.add(length) != 0 {
            length += 1;
        }
        let user_sid = String::from_utf16(std::slice::from_raw_parts(sid, length));
        LocalFree(sid.cast());
        let user_sid = user_sid.map_err(|_| Error::Invalid("INVALID_SID"))?;
        Ok(ProcessIdentity {
            pid,
            creation_time: ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64,
            user_sid,
            session_id,
            image_path,
            image_file,
        })
    }
}
