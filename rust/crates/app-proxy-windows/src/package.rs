use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const BRIDGE: &str = include_str!("../../../assets/msix-bridge.ps1");
mod lookup;

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    resolve(family, app_id)
}

/// Read-only lookup for a saved stable package locator, repeated for each plan.
pub fn resolve(family: &str, app_id: &str) -> Result<Package> {
    if [family, app_id]
        .iter()
        .any(|s| s.is_empty() || s.len() > 256 || s.contains('\0'))
    {
        return Err(Error::Invalid("INVALID_PACKAGE_LOCATOR"));
    }
    lookup::resolve(family, app_id)
}

/// Activates only our bounded probe helper; not a general package launch service.
pub fn activate_probe(package: &Package, helper: &Path, request: &Path) -> Result<()> {
    activate(package, helper, request, "probe")
}

/// The fixed host entry consumes a protected one-use request; bridge success
/// only acknowledges activation and is never an application creation receipt.
pub fn activate_launch(package: &Package, helper: &Path, request: &Path) -> Result<()> {
    activate(package, helper, request, "launch")
}

fn activate(package: &Package, helper: &Path, request: &Path, operation: &str) -> Result<()> {
    let helper = helper
        .to_str()
        .ok_or(Error::Invalid("NON_UNICODE_HELPER"))?;
    let request = request
        .to_str()
        .ok_or(Error::Invalid("NON_UNICODE_REQUEST"))?;
    let _: serde_json::Value = bridge(&serde_json::json!({
        "operation":operation, "family_name":package.family_name, "app_id":package.app_id,
        "expected_full_name":package.full_name, "helper":helper, "request":request,
    }))?;
    Ok(())
}

fn bridge<T: serde::de::DeserializeOwned>(request: &impl Serialize) -> Result<T> {
    bridge_script(request, BRIDGE)
}

fn bridge_script<T: serde::de::DeserializeOwned>(
    request: &impl Serialize,
    source: &str,
) -> Result<T> {
    crate::identity::assert_ordinary_user()?;
    let mut script = tempfile::Builder::new().suffix(".ps1").tempfile()?;
    script.write_all(source.as_bytes())?;
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
        crate::diagnostic_timing::mark("package.bridge_error", || {
            format!(
                "{code}:{}",
                value
                    .get("system_code")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0)
            )
        });
        return Err(Error::Invalid(match code {
            "APP_NOT_INSTALLED" => "APP_NOT_INSTALLED",
            "AMBIGUOUS_PACKAGE" => "AMBIGUOUS_PACKAGE",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_query(
        root: &Path,
        copies: usize,
        entry_point: &str,
        namespace: &str,
        operation: &str,
    ) -> Result<Package> {
        let manifest = format!(
            r#"<Package xmlns="http://schemas.microsoft.com/appx/manifest/foundation/windows10" xmlns:d6="{namespace}"><Properties><DisplayName>Fixture</DisplayName><d6:FileSystemWriteVirtualization>disabled</d6:FileSystemWriteVirtualization></Properties><Applications><Application Id="App" EntryPoint="{entry_point}" Executable="app.exe" /></Applications></Package>"#
        );
        let package = serde_json::json!({ "PackageFamilyName":"Fixture_publisher", "PackageFullName":"Fixture_1.0_x64__publisher", "InstallLocation":root });
        let fixture =
            serde_json::json!({ "packages": vec![package; copies], "manifest": manifest });
        // Escape a PowerShell literal, not a command; test-only data never enters
        // the production bridge source. Stub only read-only Appx queries.
        let literal = serde_json::to_string(&fixture).unwrap().replace('\'', "''");
        let source = format!(
            "$script:Fixture = '{literal}' | ConvertFrom-Json\nfunction Get-AppxPackage {{ param($Name) $script:Fixture.packages }}\nfunction Get-AppxPackageManifest {{ param($Package) [xml]$script:Fixture.manifest }}\n{BRIDGE}"
        );
        bridge_script(
            &serde_json::json!({"operation":operation,"family_name":"Fixture_publisher","app_id":"App"}),
            &source,
        )
    }

    #[test]
    fn bridge_distinguishes_absent_ambiguous_and_non_full_trust_packages() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("app.exe"), b"never executed").unwrap();
        for (copies, entry, expected) in [
            (0, "Windows.FullTrustApplication", "APP_NOT_INSTALLED"),
            (2, "Windows.FullTrustApplication", "AMBIGUOUS_PACKAGE"),
            (1, "Fixture.App", "PACKAGE_NOT_FULL_TRUST"),
        ] {
            let result = fixture_query(temp.path(), copies, entry, "urn:other", "discover");
            assert!(matches!(result, Err(Error::Invalid(code)) if code == expected));
        }
        // General read-only lookup must not broaden the probe activation allowlist.
        assert!(matches!(
            fixture_query(
                temp.path(),
                1,
                "Windows.FullTrustApplication",
                "urn:other",
                "probe"
            ),
            Err(Error::Invalid("PACKAGE_NOT_FULL_TRUST"))
        ));
    }

    #[test]
    fn bridge_resolves_other_full_trust_packages_and_uses_virtualization_namespace() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("app.exe"), b"never executed").unwrap();
        for (namespace, isolated) in [
            (
                "http://schemas.microsoft.com/appx/manifest/desktop/windows10/6",
                false,
            ),
            ("urn:other", true),
        ] {
            let result = fixture_query(
                temp.path(),
                1,
                "Windows.FullTrustApplication",
                namespace,
                "discover",
            )
            .unwrap();
            assert_eq!(result.family_name, "Fixture_publisher");
            assert_eq!(result.exe, temp.path().join("app.exe"));
            assert_eq!(result.isolated_storage, isolated);
        }
    }
}
