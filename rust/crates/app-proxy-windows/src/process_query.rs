//! Read-only process observations. Snapshot names/PIDs are hints, never ownership
//! or permission to stop. WMI arguments are bound to a retained exact native handle.
use crate::{Error, Result, identity, last_error};
use app_proxy_core::ProcessIdentity;
use std::{
    ffi::OsString,
    os::windows::{
        ffi::OsStringExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows::{
    Win32::System::{
        Com::*,
        Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE},
        Variant::*,
        Wmi::*,
    },
    core::{BSTR, PCWSTR, w},
};
use windows_sys::Win32::{
    Foundation::{
        ERROR_NO_MORE_FILES, FILETIME, INVALID_HANDLE_VALUE, LocalFree, SYSTEMTIME, WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::ToolHelp::*,
        Threading::{PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject},
        Time::SystemTimeToFileTime,
    },
    UI::Shell::CommandLineToArgvW,
};

const QUERY_BUDGET: Duration = Duration::from_secs(5);
const MAX_PROCESSES: usize = 32768;
static QUERY_BUSY: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(crate) static QUERY_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct ProcessHint {
    pub pid: u32,
    /// Hint only: the parent may have exited and its PID may have been reused.
    pub parent_pid: u32,
    pub executable_name: OsString,
}

/// No Debug or Serialize: command lines can contain credentials.
pub struct ProcessObservation {
    pub identity: ProcessIdentity,
    pub parent_pid: u32,
    /// None means unavailable, not an empty list of application switches.
    pub arguments: Option<Vec<OsString>>,
}

pub fn snapshot() -> Result<Vec<ProcessHint>> {
    identity::assert_ordinary_user()?;
    // SAFETY: no pointer inputs, and the returned handle is adopted exactly once.
    let raw = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if raw == INVALID_HANDLE_VALUE {
        return Err(last_error("CreateProcessSnapshot"));
    }
    // SAFETY: raw is the successful newly-owned snapshot handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: PROCESSENTRY32W is an integer/array C output structure.
    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of_val(&entry) as u32;
    let mut result = Vec::new();
    // SAFETY: the handle and correctly sized output live through enumeration.
    let mut ok = unsafe { Process32FirstW(handle.as_raw_handle(), &mut entry) };
    while ok != 0 {
        if result.len() >= MAX_PROCESSES {
            return Err(Error::Invalid("PROCESS_SNAPSHOT_LIMIT"));
        }
        let end = entry
            .szExeFile
            .iter()
            .position(|&v| v == 0)
            .ok_or(Error::Invalid("INVALID_PROCESS_NAME"))?;
        result.push(ProcessHint {
            pid: entry.th32ProcessID,
            parent_pid: entry.th32ParentProcessID,
            executable_name: OsString::from_wide(&entry.szExeFile[..end]),
        });
        // SAFETY: same live snapshot/output as above.
        ok = unsafe { Process32NextW(handle.as_raw_handle(), &mut entry) };
    }
    let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32;
    if error != ERROR_NO_MORE_FILES {
        return Err(Error::Windows {
            operation: "EnumerateProcesses",
            code: error,
        });
    }
    Ok(result)
}

/// No access to another user's/session's command line, and no termination rights.
/// A timeout never proves absence. At most one native query can remain outstanding
/// in this process even when COM outlives the caller's deadline.
pub async fn inspect(expected: &ProcessIdentity) -> Result<ProcessObservation> {
    inspect_with(expected, Ok).await
}

/// Read-only candidate discovery for the current physical installation. Snapshot
/// names only decide which unreadable processes require an unknown result; all
/// readable processes are checked for physical aliases, regardless of basename.
pub async fn application_candidates(
    application: &crate::installation::ResolvedApplication,
) -> Result<Vec<ProcessIdentity>> {
    identity::assert_ordinary_user()?;
    let caller = identity::current()?;
    let image = application.image().clone();
    let name = application
        .executable()
        .file_name()
        .ok_or(Error::Invalid("EXE_REQUIRED"))?
        .to_owned();
    query_with(&QUERY_BUSY, QUERY_BUDGET, move |deadline| {
        let mut candidates = Vec::new();
        for hint in snapshot()? {
            if Instant::now() >= deadline {
                return Err(Error::Invalid("PROCESS_QUERY_TIMEOUT"));
            }
            match identity::inspect(hint.pid) {
                Ok(process)
                    if process.user_sid == caller.user_sid
                        && process.session_id == caller.session_id
                        && process.image_file == image =>
                {
                    candidates.push(process)
                }
                Ok(_) => {}
                Err(Error::Windows { code: 87, .. }) => {}
                Err(_) if hint.executable_name.eq_ignore_ascii_case(&name) => {
                    return Err(Error::Invalid("INSTANCE_PROCESS_UNKNOWN"));
                }
                Err(_) => {}
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Invalid("PROCESS_QUERY_TIMEOUT"));
        }
        Ok(candidates)
    })
    .await
}

/// Instance attribution's read-only filesystem checks share the same deadline,
/// retained process handle and outstanding-work bound as the WMI query itself.
pub(crate) async fn inspect_with<T: Send + 'static>(
    expected: &ProcessIdentity,
    finish: impl FnOnce(ProcessObservation) -> Result<T> + Send + 'static,
) -> Result<T> {
    identity::assert_ordinary_user()?;
    let caller = identity::current()?;
    if expected.pid == 0
        || expected.creation_time == 0
        || expected.user_sid != caller.user_sid
        || expected.session_id != caller.session_id
    {
        return Err(Error::IdentityMismatch);
    }
    let expected = expected.clone();
    query_with(&QUERY_BUSY, QUERY_BUDGET, move |deadline| {
        inspect_on_thread(expected, deadline, finish)
    })
    .await
}

async fn query_with<T: Send + 'static>(
    busy: &'static AtomicBool,
    budget: Duration,
    work: impl FnOnce(Instant) -> Result<T> + Send + 'static,
) -> Result<T> {
    if busy
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return Err(Error::Invalid("PROCESS_QUERY_BUSY"));
    }
    struct Slot(&'static AtomicBool);
    impl Drop for Slot {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Release);
        }
    }
    let slot = Slot(busy);
    let deadline = Instant::now() + budget;
    let (sender, receiver) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("process-wmi".into())
        .spawn(move || {
            let result = work(deadline);
            drop(slot);
            let _ = sender.send(result);
        })?;
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), receiver)
        .await
        .map_err(|_| Error::Invalid("PROCESS_QUERY_TIMEOUT"))?
        .map_err(|_| Error::Invalid("PROCESS_QUERY_INTERRUPTED"))?
}

fn inspect_on_thread<T>(
    expected: ProcessIdentity,
    deadline: Instant,
    finish: impl FnOnce(ProcessObservation) -> Result<T>,
) -> Result<T> {
    let handle = identity::open(
        expected.pid,
        PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
    )?;
    verify_handle(&handle, &expected)?;
    let row = wmi_row(expected.pid, deadline)?;
    if row.pid != expected.pid
        || row.session_id != expected.session_id
        || dmtf_creation_time(&row.created)? / 10 != expected.creation_time / 10
    {
        return Err(Error::IdentityMismatch);
    }
    let arguments = row
        .command_line
        .as_deref()
        .map(parse_arguments)
        .transpose()?
        .flatten();
    let result = finish(ProcessObservation {
        identity: expected.clone(),
        parent_pid: row.parent_pid,
        arguments,
    })?;
    verify_handle(&handle, &expected)?;
    if Instant::now() >= deadline {
        return Err(Error::Invalid("PROCESS_QUERY_TIMEOUT"));
    }
    Ok(result)
}

fn verify_handle(handle: &OwnedHandle, expected: &ProcessIdentity) -> Result<()> {
    // SAFETY: the exact process handle remains open across both checks and query.
    unsafe {
        match WaitForSingleObject(handle.as_raw_handle(), 0) {
            WAIT_TIMEOUT => {}
            windows_sys::Win32::Foundation::WAIT_OBJECT_0 => {
                return Err(Error::Invalid("PROCESS_EXITED_DURING_INSPECTION"));
            }
            _ => return Err(last_error("WaitForProcessInspection")),
        }
        if identity::inspect_handle(handle.as_raw_handle())? != *expected {
            return Err(Error::IdentityMismatch);
        }
    }
    Ok(())
}

struct Row {
    pid: u32,
    parent_pid: u32,
    session_id: u32,
    created: Vec<u16>,
    command_line: Option<Vec<u16>>,
}

fn com_error(error: windows::core::Error) -> Error {
    // Never forward COM descriptions: provider errors can include query contents.
    Error::Windows {
        operation: "ProcessWmiQuery",
        code: error.code().0 as u32,
    }
}

fn wmi_row(pid: u32, deadline: Instant) -> Result<Row> {
    // All COM objects stay on this dedicated thread and drop before apartment teardown.
    struct Apartment;
    impl Drop for Apartment {
        fn drop(&mut self) {
            // SAFETY: balances the successful initialization on this same thread.
            unsafe { CoUninitialize() }
        }
    }
    // SAFETY: fresh dedicated thread; COM objects never leave it. Strings and output
    // storage remain live for each synchronous ABI call and wrappers own all outputs.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED)
            .ok()
            .map_err(com_error)?;
        let _apartment = Apartment;
        let locator: IWbemLocator =
            CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER).map_err(com_error)?;
        let empty = BSTR::new();
        let service = locator
            .ConnectServer(
                &BSTR::from("ROOT\\CIMV2"),
                &empty,
                &empty,
                &empty,
                WBEM_FLAG_CONNECT_USE_MAX_WAIT.0,
                &empty,
                None,
            )
            .map_err(com_error)?;
        CoSetProxyBlanket(
            &service,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )
        .map_err(com_error)?;
        let query = BSTR::from(format!(
            "SELECT ProcessId,ParentProcessId,SessionId,CreationDate,CommandLine FROM Win32_Process WHERE ProcessId = {pid}"
        ));
        let enumeration = service
            .ExecQuery(
                &BSTR::from("WQL"),
                &query,
                WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                None,
            )
            .map_err(com_error)?;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .ok_or(Error::Invalid("PROCESS_QUERY_TIMEOUT"))?;
            let mut objects = [None];
            let mut returned = 0;
            let status = enumeration.Next(
                remaining.as_millis().clamp(1, 250) as i32,
                &mut objects,
                &mut returned,
            );
            status.ok().map_err(com_error)?;
            if returned == 1 {
                let object = objects[0]
                    .take()
                    .ok_or(Error::Invalid("INVALID_PROCESS_QUERY_RESULT"))?;
                return Ok(Row {
                    pid: number_property(&object, w!("ProcessId"))?,
                    parent_pid: number_property(&object, w!("ParentProcessId"))?,
                    session_id: number_property(&object, w!("SessionId"))?,
                    created: string_property(&object, w!("CreationDate"))?
                        .ok_or(Error::Invalid("PROCESS_CREATION_TIME_UNAVAILABLE"))?,
                    command_line: string_property(&object, w!("CommandLine"))?,
                });
            }
            if returned != 0 {
                return Err(Error::Invalid("INVALID_PROCESS_QUERY_RESULT"));
            }
            if status.0 == WBEM_S_FALSE.0 {
                return Err(Error::Invalid("PROCESS_QUERY_NOT_FOUND"));
            }
            if status.0 != WBEM_S_TIMEDOUT.0 {
                return Err(Error::Invalid("INVALID_PROCESS_QUERY_RESULT"));
            }
        }
    }
}

fn property(object: &IWbemClassObject, name: PCWSTR) -> Result<VARIANT> {
    let mut value = VARIANT::default();
    // SAFETY: name is a static terminated property name; wrapper frees the output.
    unsafe { object.Get(name, 0, &mut value, None, None) }.map_err(com_error)?;
    Ok(value)
}
fn number_property(object: &IWbemClassObject, name: PCWSTR) -> Result<u32> {
    let value = property(object, name)?;
    // SAFETY: inspect the tag before reading its corresponding initialized union field.
    unsafe {
        let value = &value.Anonymous.Anonymous;
        match value.vt {
            VT_I4 => Ok(value.Anonymous.lVal as u32), // WMI CIM_UINT32 uses VT_I4.
            VT_UI4 => Ok(value.Anonymous.ulVal),
            _ => Err(Error::Invalid("INVALID_PROCESS_QUERY_PROPERTY")),
        }
    }
}
fn string_property(object: &IWbemClassObject, name: PCWSTR) -> Result<Option<Vec<u16>>> {
    let value = property(object, name)?;
    // SAFETY: only borrow a BSTR after verifying its tag, before VARIANT drops it.
    unsafe {
        let value = &value.Anonymous.Anonymous;
        match value.vt {
            VT_NULL | VT_EMPTY => Ok(None),
            VT_BSTR => {
                let text = &value.Anonymous.bstrVal;
                if text.len() > 32767 || text.contains(&0) {
                    return Err(Error::Invalid("PROCESS_QUERY_TEXT_LIMIT"));
                }
                Ok(Some(text.to_vec()))
            }
            _ => Err(Error::Invalid("INVALID_PROCESS_QUERY_PROPERTY")),
        }
    }
}

fn parse_arguments(command: &[u16]) -> Result<Option<Vec<OsString>>> {
    if command.is_empty() {
        return Ok(None);
    }
    if command.len() > 32767 || command.contains(&0) {
        return Err(Error::Invalid("INVALID_PROCESS_COMMAND_LINE"));
    }
    let terminated: Vec<_> = command.iter().copied().chain([0]).collect();
    let mut count = 0;
    // SAFETY: terminated bounded input and correctly sized output; returned argv
    // belongs to LocalAlloc and is freed after copying all arguments.
    unsafe {
        let argv = CommandLineToArgvW(terminated.as_ptr(), &mut count);
        if argv.is_null() {
            return Err(last_error("ParseProcessArguments"));
        }
        struct Arguments(*mut windows_sys::core::PWSTR);
        impl Drop for Arguments {
            fn drop(&mut self) {
                // SAFETY: exact allocation returned by CommandLineToArgvW, once.
                unsafe {
                    LocalFree(self.0.cast());
                }
            }
        }
        let _allocation = Arguments(argv);
        if !(1..=32767).contains(&count) {
            return Err(Error::Invalid("INVALID_PROCESS_COMMAND_LINE"));
        }
        let mut words = Vec::with_capacity(count as usize);
        for &word in std::slice::from_raw_parts(argv, count as usize) {
            let mut length = 0;
            while length <= command.len() && *word.add(length) != 0 {
                length += 1;
            }
            if length > command.len() {
                return Err(Error::Invalid("INVALID_PROCESS_COMMAND_LINE"));
            }
            words.push(OsString::from_wide(std::slice::from_raw_parts(
                word, length,
            )));
        }
        if words[0].is_empty() {
            return Err(Error::Invalid("INVALID_PROCESS_COMMAND_LINE"));
        }
        Ok(Some(words))
    }
}

fn dmtf_creation_time(value: &[u16]) -> Result<u64> {
    let invalid = || Error::Invalid("INVALID_PROCESS_CREATION_TIME");
    if value.len() != 25
        || value[14] != b'.' as u16
        || !b"+-".iter().any(|&c| value[21] == c as u16)
    {
        return Err(invalid());
    }
    let number = |start: usize, end: usize| -> Result<u32> {
        value[start..end].iter().try_fold(0, |acc, &c| {
            if !(b'0' as u16..=b'9' as u16).contains(&c) {
                return Err(invalid());
            }
            Ok(acc * 10 + u32::from(c - b'0' as u16))
        })
    };
    let time = SYSTEMTIME {
        wYear: number(0, 4)? as u16,
        wMonth: number(4, 6)? as u16,
        wDay: number(6, 8)? as u16,
        wHour: number(8, 10)? as u16,
        wMinute: number(10, 12)? as u16,
        wSecond: number(12, 14)? as u16,
        ..Default::default()
    };
    let mut filetime = FILETIME::default();
    // SAFETY: fully initialized input and sized output, no timezone conversion in API.
    if unsafe { SystemTimeToFileTime(&time, &mut filetime) } == 0 {
        return Err(invalid());
    }
    let ticks = (u64::from(filetime.dwHighDateTime) << 32) | u64::from(filetime.dwLowDateTime);
    let ticks = ticks
        .checked_add(u64::from(number(15, 21)?) * 10)
        .ok_or_else(invalid)?;
    let offset = u64::from(number(22, 25)?) * 60 * 10_000_000;
    if value[21] == b'+' as u16 {
        ticks.checked_sub(offset)
    } else {
        ticks.checked_add(offset)
    }
    .ok_or_else(invalid)
}

#[cfg(test)]
mod tests;
