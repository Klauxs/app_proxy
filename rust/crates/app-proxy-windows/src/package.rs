use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const BRIDGE: &str = include_str!("../../../assets/msix-bridge.ps1");

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub family_name: String,
    pub full_name: String,
    pub app_id: String,
    pub exe: std::path::PathBuf,
    pub isolated_storage: bool,
}

pub fn discover(app: &str) -> Result<Package> {
    let (family, app_id) = match app {
        "claude" => ("Claude_pzs8sxrjxfjjc", "Claude"),
        "codex" => ("OpenAI.Codex_2p2nqsd0c76g0", "App"),
        _ => return Err(Error::Invalid("UNKNOWN_APPLICATION")),
    };
    bridge(&serde_json::json!({"operation":"discover", "family_name":family, "app_id":app_id}))
}

/// Activates only our bounded probe helper; not a general package launch service.
pub fn activate_probe(package: &Package, helper: &Path, request: &Path) -> Result<()> {
    let helper = helper
        .to_str()
        .ok_or(Error::Invalid("NON_UNICODE_HELPER"))?;
    let request = request
        .to_str()
        .ok_or(Error::Invalid("NON_UNICODE_REQUEST"))?;
    let _: serde_json::Value = bridge(&serde_json::json!({
        "operation":"probe", "family_name":package.family_name, "app_id":package.app_id,
        "expected_full_name":package.full_name, "helper":helper, "request":request,
    }))?;
    Ok(())
}

fn bridge<T: serde::de::DeserializeOwned>(request: &impl Serialize) -> Result<T> {
    crate::identity::assert_ordinary_user()?;
    let mut script = tempfile::Builder::new().suffix(".ps1").tempfile()?;
    script.write_all(BRIDGE.as_bytes())?;
    script.flush()?;
    // PowerShell opens scripts without sharing write access. Close our writer first,
    // retaining only the automatic path cleanup guard.
    let script = script.into_temp_path();
    let mut output = tempfile::tempfile()?;
    // SystemRoot is resolved through the Windows directory API, not caller-supplied PATH.
    let powershell = system_powershell()?;
    let mut child = Command::new(powershell)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .stdin(Stdio::piped())
        .stdout(output.try_clone()?)
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()?;
    let bytes = serde_json::to_vec(request)?;
    let write_result = child
        .stdin
        .take()
        .ok_or(Error::Invalid("BRIDGE_STDIN_MISSING"))?
        .write_all(&bytes);
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(error.into());
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Error::Invalid("PACKAGE_OPERATION_TIMEOUT_RESULT_UNKNOWN"));
        }
        std::thread::sleep(Duration::from_millis(30));
    };
    if !status.success() {
        return Err(Error::Invalid("PACKAGE_SCRIPT_EXIT_FAILED"));
    }
    output.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    output.take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > 1024 * 1024 {
        return Err(Error::Invalid("PACKAGE_RESPONSE_TOO_LARGE"));
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes)?;
    if let Some(code) = value.get("error").and_then(|v| v.as_str()) {
        return Err(Error::Invalid(match code {
            "APP_NOT_INSTALLED" => "APP_NOT_INSTALLED",
            "PACKAGE_CHANGED" => "PACKAGE_CHANGED",
            "PACKAGE_NOT_FULL_TRUST" => "PACKAGE_NOT_FULL_TRUST",
            _ => "PACKAGE_BRIDGE_FAILED",
        }));
    }
    Ok(serde_json::from_value(value)?)
}

fn system_powershell() -> Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    // SAFETY: the output buffer is valid for the supplied capacity.
    unsafe {
        let mut buffer = vec![0u16; 32768];
        let count = windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(
            buffer.as_mut_ptr(),
            buffer.len() as u32,
        );
        if count == 0 || count as usize >= buffer.len() {
            return Err(crate::last_error("GetSystemDirectoryW"));
        }
        buffer.truncate(count as usize);
        Ok(
            std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer))
                .join("WindowsPowerShell/v1.0/powershell.exe"),
        )
    }
}
