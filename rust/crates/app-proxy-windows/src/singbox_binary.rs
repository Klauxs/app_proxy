//! Discover program files only. No running service or foreign configuration is read.
use crate::{Error, Result, installation};
use app_proxy_core::model::ApplicationLocator;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

const OUTPUT_LIMIT: u64 = 16 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Managed,
    Path,
    Scoop,
    Winget,
}

/// A version-checked file pinned only during preparation. Every generated
/// configuration still needs check_config; version alone is not compatibility.
pub struct CoreBinary {
    resolved: installation::ResolvedApplication,
    version: String,
    source: Source,
}

impl CoreBinary {
    pub fn executable(&self) -> &Path {
        self.resolved.executable()
    }
    pub fn version(&self) -> &str {
        &self.version
    }
    pub fn source(&self) -> Source {
        self.source
    }
    pub fn verify_current(&self) -> Result<()> {
        self.resolved.verify_current()
    }

    /// The caller owns this generated config and its protected directory. Check
    /// diagnostics are discarded because they may echo credentials or URLs.
    pub async fn check_config(&self, config: &Path) -> Result<()> {
        if !config.is_absolute() {
            return Err(Error::Invalid("ABSOLUTE_CORE_CONFIG_REQUIRED"));
        }
        self.verify_current()?;
        let mut command = self.command();
        command.arg("check").arg("-c").arg(config);
        probe(command, false, PROBE_TIMEOUT).await?;
        self.verify_current()
    }

    fn command(&self) -> Command {
        let mut command = Command::new(self.executable());
        command.current_dir(self.executable().parent().expect("absolute executable"));
        command
    }
}

/// None means no candidate passed a real version probe. The asynchronous probe
/// budget is distinct from missing software. Synchronous filesystem operations
/// (including network drives in PATH) remain subject to Windows I/O timeouts.
pub async fn discover(store_root: &Path) -> Result<Option<CoreBinary>> {
    crate::identity::assert_ordinary_user()?;
    if !store_root.is_absolute() {
        return Err(Error::Invalid("ABSOLUTE_STORE_REQUIRED"));
    }
    let candidates = candidates(store_root)?;
    timeout(DISCOVERY_TIMEOUT, select(candidates))
        .await
        .map_err(|_| Error::Invalid("CORE_DISCOVERY_TIMEOUT"))
}

async fn select(candidates: Vec<(PathBuf, Source)>) -> Option<CoreBinary> {
    let mut seen = Vec::new();
    for (path, source) in candidates {
        // Scoop shims can prepend args, elevate, and launch a different image.
        // Discover Scoop's actual executable below instead of running the shim.
        if path.with_extension("shim").try_exists().unwrap_or(true) {
            continue;
        }
        let Ok(resolved) = installation::resolve(&ApplicationLocator::Exe { path }) else {
            continue;
        };
        if seen.contains(resolved.image()) {
            continue;
        }
        seen.push(resolved.image().clone());
        let mut binary = CoreBinary {
            resolved,
            version: String::new(),
            source,
        };
        let mut command = binary.command();
        command.arg("version");
        let Ok(output) = probe(command, true, PROBE_TIMEOUT).await else {
            continue;
        };
        let Some(version) = parse_version(&output) else {
            continue;
        };
        if binary.verify_current().is_err() {
            continue;
        }
        binary.version = version;
        return Some(binary);
    }
    None
}

pub(crate) async fn inspect_managed(path: PathBuf) -> Result<CoreBinary> {
    select(vec![(path, Source::Managed)])
        .await
        .ok_or(Error::Invalid("CORE_INSTALL_PROBE_FAILED"))
}

/// Reconfiguration pins the original image so rollback cannot silently select
/// another installation/version. This does not attach any external service.
pub async fn inspect_recorded(process: &app_proxy_core::ProcessIdentity) -> Result<CoreBinary> {
    let binary = select(vec![(process.image_path.clone(), Source::Managed)])
        .await
        .ok_or(Error::Invalid("CORE_ORIGINAL_BINARY_UNAVAILABLE"))?;
    if *binary.resolved.image() != process.image_file {
        return Err(Error::Invalid("CORE_ORIGINAL_BINARY_CHANGED"));
    }
    Ok(binary)
}

fn candidates(root: &Path) -> Result<Vec<(PathBuf, Source)>> {
    let mut found = Vec::new();
    let managed = root.join("bin/sing-box");
    match std::fs::read_dir(managed) {
        Ok(entries) => {
            // Ignore staging directories. Installer publishes complete versions
            // atomically to these numeric names; never enumerate their contents.
            let mut versions: Vec<_> = entries
                .take(128)
                .filter_map(|e| e.ok())
                .filter_map(|e| {
                    Some((
                        version_parts(e.file_name().to_str()?)?,
                        e.path().join("sing-box.exe"),
                    ))
                })
                .collect();
            versions.sort_by_key(|(version, _)| *version);
            versions.reverse();
            found.extend(versions.into_iter().map(|(_, p)| (p, Source::Managed)));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path)
            .filter(|p| p.is_absolute())
            .take(128)
        {
            // Also covers custom/global Scoop roots visible only through PATH.
            if dir
                .file_name()
                .is_some_and(|s| s.eq_ignore_ascii_case("shims"))
                && let Some(parent) = dir.parent()
            {
                found.push((
                    parent.join("apps/sing-box/current/sing-box.exe"),
                    Source::Scoop,
                ));
            }
            found.push((dir.join("sing-box.exe"), Source::Path));
        }
    }
    if let Some(scoop) = std::env::var_os("SCOOP") {
        found.push((
            PathBuf::from(scoop).join("apps/sing-box/current/sing-box.exe"),
            Source::Scoop,
        ));
    }
    if let Some(home) = std::env::var_os("USERPROFILE") {
        found.push((
            PathBuf::from(home).join("scoop/apps/sing-box/current/sing-box.exe"),
            Source::Scoop,
        ));
    }
    found.push((
        crate::instance_data::local_app_data()?.join("Microsoft/WinGet/Links/sing-box.exe"),
        Source::Winget,
    ));
    found.retain(|(path, _)| path.is_absolute());
    Ok(found)
}

use app_proxy_core::singbox::{valid_version, version_parts};

fn parse_version(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?;
    let version = text.lines().next()?.strip_prefix("sing-box version ")?;
    // Stable releases only until prerelease configurations have been accepted.
    valid_version(version).then(|| version.to_owned())
}

async fn probe(mut command: Command, capture: bool, budget: Duration) -> Result<Vec<u8>> {
    command
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(if capture {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true);
    // Do not expose OS errors with paths or raw core diagnostics to callers.
    let mut child = command
        .spawn()
        .map_err(|_| Error::Invalid("CORE_PROBE_START_FAILED"))?;
    let result = timeout(budget, async {
        let mut output = Vec::new();
        if let Some(stdout) = child.stdout.take() {
            stdout
                .take(OUTPUT_LIMIT + 1)
                .read_to_end(&mut output)
                .await
                .map_err(|_| Error::Invalid("CORE_PROBE_READ_FAILED"))?;
            if output.len() as u64 > OUTPUT_LIMIT {
                return Err(Error::Invalid("CORE_PROBE_OUTPUT_TOO_LARGE"));
            }
        }
        let status = child
            .wait()
            .await
            .map_err(|_| Error::Invalid("CORE_PROBE_WAIT_FAILED"))?;
        if !status.success() {
            return Err(Error::Invalid("CORE_PROBE_EXIT_FAILED"));
        }
        Ok(output)
    })
    .await
    .unwrap_or(Err(Error::Invalid("CORE_PROBE_TIMEOUT")));
    if result.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn version_requires_exact_stable_header() {
        assert_eq!(
            parse_version(b"sing-box version 1.14.1\r\nEnvironment: go\r\n"),
            Some("1.14.1".into())
        );
        for value in [
            "1.14.1",
            "other version 1.14.1",
            "sing-box version 1.14.1-beta.1",
            "sing-box version 1.14.1 secret",
            "sing-box version 1.14",
            "sing-box version 1..1",
        ] {
            assert!(parse_version(value.as_bytes()).is_none());
        }
    }

    #[test]
    fn managed_discovery_ignores_staging_and_never_searches_working_directory() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("bin/sing-box");
        for name in [
            "1.14.1",
            ".staging-1.14.2",
            "unfinished",
            "1.9.7",
            "1.14.9",
            "1.14.10",
        ] {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        let found = candidates(temp.path()).unwrap();
        let managed: Vec<_> = found
            .iter()
            .filter(|(_, s)| matches!(s, Source::Managed))
            .collect();
        assert_eq!(managed.len(), 4);
        for (entry, version) in managed.iter().zip(["1.14.10", "1.14.9", "1.14.1", "1.9.7"]) {
            assert!(entry.0.ends_with(Path::new(version).join("sing-box.exe")));
        }
        assert!(found.iter().all(|(path, _)| path.is_absolute()));
    }

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--ignored",
                "--exact",
                "singbox_binary::tests::probe_child",
                "--nocapture",
            ])
            .env("APP_PROXY_BINARY_TEST_MODE", mode);
        command
    }

    #[test]
    #[ignore = "controlled child fixture invoked by parent test"]
    fn probe_child() {
        let mode = std::env::var("APP_PROXY_BINARY_TEST_MODE").unwrap();
        match mode.as_str() {
            "success" => {
                println!("fixture-ok");
            }
            "failure" => {
                eprintln!("sensitive://password@example.invalid");
                std::process::exit(9);
            }
            "oversize" => {
                let _ = std::io::stdout().write_all(&vec![b'x'; OUTPUT_LIMIT as usize + 4096]);
            }
            "hang" => {
                std::thread::sleep(Duration::from_secs(60));
            }
            _ => panic!("unknown fixture"),
        }
    }

    #[tokio::test]
    async fn probe_bounds_output_timeout_and_sanitizes_failures() {
        let output = probe(fixture("success"), true, PROBE_TIMEOUT)
            .await
            .unwrap();
        assert!(String::from_utf8(output).unwrap().contains("fixture-ok"));
        for (mode, expected, budget) in [
            ("failure", "CORE_PROBE_EXIT_FAILED", PROBE_TIMEOUT),
            ("oversize", "CORE_PROBE_OUTPUT_TOO_LARGE", PROBE_TIMEOUT),
            ("hang", "CORE_PROBE_TIMEOUT", Duration::from_millis(150)),
        ] {
            let start = std::time::Instant::now();
            let error = probe(fixture(mode), true, budget).await.unwrap_err();
            assert_eq!(error.to_string(), expected);
            assert!(start.elapsed() < Duration::from_secs(5));
        }
    }

    #[tokio::test]
    async fn candidate_failure_does_not_select_unrelated_executable() {
        let temp = tempfile::tempdir().unwrap();
        let invalid = temp.path().join("sing-box.exe");
        std::fs::write(&invalid, "not an executable").unwrap();
        let found = select(vec![
            (invalid, Source::Managed),
            (std::env::current_exe().unwrap(), Source::Path),
        ])
        .await;
        assert!(found.is_none());
    }
}
