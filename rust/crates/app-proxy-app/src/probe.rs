//! Bounded M0 experiments. These do not initialize a production store or install IFEO.
use app_proxy_core::{EnvPatch, ProcessIdentity};
use app_proxy_windows::{
    identity, package,
    process::{self, CreationMode, SpawnSpec},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use uuid::Uuid;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const MARKER: &str = ".app-proxy-rust-probe";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol: u32,
    id: Uuid,
    expires_at: u64,
    expected_package: Option<String>,
    hold_ms: u64,
    environment_names: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    id: Uuid,
    identity: ProcessIdentity,
    package_family: Option<String>,
    cwd: PathBuf,
    args: Vec<OsString>,
    environment: BTreeMap<String, Option<String>>,
    roundtrip: String,
}

#[derive(Serialize)]
pub struct ProcessReport {
    pub mode: &'static str,
    pub identity: ProcessIdentity,
    pub args_and_cwd_match: bool,
    pub environment_set_unset_match: bool,
    pub parent_environment_unchanged: bool,
    pub survived_creating_thread: bool,
    pub forged_identity_rejected: bool,
    pub exact_stop_confirmed: bool,
    pub ifeo_registration_tested: bool,
}

#[derive(Serialize)]
pub struct PackageReport {
    pub application: package::Package,
    pub helper_identity: ProcessIdentity,
    pub package_family_confirmed: bool,
    pub package_write_visible_outside: bool,
    pub target_application_started: bool,
}

fn now() -> Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn write_new_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(65537).read_to_end(&mut bytes)?;
    if bytes.len() > 65536 {
        return Err("PROBE_FILE_TOO_LARGE".into());
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn make_request(root: &Path, package: Option<String>, hold_ms: u64) -> Result<Request> {
    let request = Request {
        protocol: 1,
        id: Uuid::new_v4(),
        expires_at: now()? + 20,
        expected_package: package,
        hold_ms,
        environment_names: vec![
            "APP_PROXY_PROBE_VALUE".into(),
            "APP_PROXY_PROBE_REMOVE".into(),
            "APP_PROXY_PROBE_EMPTY".into(),
        ],
    };
    write_new_json(&root.join(MARKER), &request.id)?;
    write_new_json(&root.join("request.json"), &request)?;
    Ok(request)
}

fn helper_path() -> Result<PathBuf> {
    let path = std::env::current_exe()?.with_file_name("app-proxy-host.exe");
    if !path.is_file() {
        return Err("Build both binaries first: cargo build --workspace".into());
    }
    Ok(path)
}

fn await_receipt(root: &Path, request: &Request) -> Result<Receipt> {
    let deadline = Instant::now() + Duration::from_secs(22);
    loop {
        let path = root.join("receipt.json");
        if path.is_file() {
            let receipt: Receipt = read_json(&path)?;
            if receipt.id != request.id || receipt.package_family != request.expected_package {
                return Err("PROBE_RECEIPT_IDENTITY_MISMATCH".into());
            }
            let visible: String = read_json(&root.join("roundtrip.json"))?;
            if visible != receipt.roundtrip {
                return Err("PROBE_STORAGE_NOT_SHARED".into());
            }
            return Ok(receipt);
        }
        if Instant::now() >= deadline {
            return Err("PROBE_TIMEOUT_DO_NOT_RETRY_BLINDLY".into());
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

/// Only writes a receipt in a marked probe directory; cannot launch another executable.
pub fn child(request_path: &Path, args: Vec<OsString>) -> Result<()> {
    identity::assert_ordinary_user()?;
    if !request_path.is_absolute()
        || request_path.file_name() != Some(std::ffi::OsStr::new("request.json"))
    {
        return Err("INVALID_PROBE_PATH".into());
    }
    let root = request_path.parent().ok_or("INVALID_PROBE_PARENT")?;
    // Probe files never accept junction/symlink roots or files.
    for path in root
        .ancestors()
        .chain([request_path, root.join(MARKER).as_path()])
    {
        use std::os::windows::fs::MetadataExt;
        if fs::symlink_metadata(path)?.file_attributes() & 0x400 != 0 {
            return Err("PROBE_REPARSE_POINT".into());
        }
    }
    let request: Request = read_json(request_path)?;
    let marker: Uuid = read_json(&root.join(MARKER))?;
    if request.protocol != 1
        || request.id != marker
        || request.expires_at < now()?
        || request.expires_at > now()? + 30
        || request.hold_ms > 30000
        || request
            .environment_names
            .iter()
            .any(|k| !k.starts_with("APP_PROXY_PROBE_"))
    {
        return Err("INVALID_OR_EXPIRED_PROBE".into());
    }
    let family = identity::package_family()?;
    if family != request.expected_package {
        return Err("PROBE_PACKAGE_MISMATCH".into());
    }
    // Exclusive consumption prevents duplicate helpers from producing two results.
    let _claim = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(root.join("claim"))?;
    let roundtrip = format!("包内写入 / child roundtrip {}", request.id);
    let receipt = Receipt {
        id: request.id,
        identity: identity::current()?,
        package_family: family,
        cwd: std::env::current_dir()?,
        args,
        environment: request
            .environment_names
            .into_iter()
            .map(|k| {
                let value = std::env::var(&k).ok();
                (k, value)
            })
            .collect(),
        roundtrip: roundtrip.clone(),
    };
    write_new_json(&root.join("roundtrip.json"), &roundtrip)?;
    write_new_json(&root.join("receipt.tmp"), &receipt)?;
    fs::rename(root.join("receipt.tmp"), root.join("receipt.json"))?;
    std::thread::sleep(Duration::from_millis(request.hold_ms));
    Ok(())
}

pub fn process_probe(debug: bool) -> Result<ProcessReport> {
    identity::assert_ordinary_user()?;
    let directory = tempfile::Builder::new()
        .prefix("AppProxyRust-M0-中文 空格-")
        .tempdir()?;
    let root = directory.path();
    let request = make_request(root, None, 30000)?;
    let payload: Vec<OsString> = [
        "",
        "中文 参数",
        "a\"b",
        "C:\\space dir\\",
        "a\\\"b",
        "--literal=value",
        "雪🙂",
    ]
    .into_iter()
    .map(Into::into)
    .collect();
    let mut args = vec![
        "probe-child".into(),
        "--request".into(),
        root.join("request.json").into_os_string(),
        "--".into(),
    ];
    args.extend(payload.clone());
    let before: BTreeMap<_, _> = std::env::vars_os().collect();
    let environment = EnvPatch {
        set: [
            (
                "app_proxy_probe_value".into(),
                "中文 value with spaces".into(),
            ),
            ("APP_PROXY_PROBE_EMPTY".into(), String::new()),
        ]
        .into(),
        unset: vec!["APP_PROXY_PROBE_REMOVE".into()],
    };
    let mut child = process::spawn(SpawnSpec {
        exe: helper_path()?,
        args,
        cwd: root.into(),
        environment,
        mode: if debug {
            CreationMode::DebugDetach
        } else {
            CreationMode::Normal
        },
    })?;
    let result = (|| -> Result<ProcessReport> {
        let receipt = await_receipt(root, &request)?;
        let args_match =
            receipt.args == payload && fs::canonicalize(&receipt.cwd)? == fs::canonicalize(root)?;
        let environment_match = receipt.environment.get("APP_PROXY_PROBE_VALUE")
            == Some(&Some("中文 value with spaces".into()))
            && receipt.environment.get("APP_PROXY_PROBE_EMPTY") == Some(&Some(String::new()))
            && receipt.environment.get("APP_PROXY_PROBE_REMOVE") == Some(&None);
        let survived = child.try_wait()?.is_none();
        let identity_matches = receipt.identity == child.identity;
        let mut forged = child.identity.clone();
        forged.creation_time = forged.creation_time.wrapping_add(1);
        let rejected = matches!(
            process::terminate_exact(&forged),
            Err(app_proxy_windows::Error::IdentityMismatch)
        ) && child.try_wait()?.is_none();
        let parent_unchanged = before == std::env::vars_os().collect();
        if !args_match
            || !environment_match
            || !survived
            || !identity_matches
            || !rejected
            || !parent_unchanged
        {
            return Err("PROCESS_PROBE_CONTRACT_FAILED".into());
        }
        process::terminate_exact(&child.identity)?;
        let stopped = child.wait_timeout(Duration::from_secs(3))?.is_some();
        if !stopped {
            return Err("PROBE_STOP_UNCONFIRMED".into());
        }
        Ok(ProcessReport {
            mode: if debug { "debug_detach" } else { "normal" },
            identity: child.identity.clone(),
            args_and_cwd_match: args_match,
            environment_set_unset_match: environment_match,
            parent_environment_unchanged: parent_unchanged,
            survived_creating_thread: survived,
            forged_identity_rejected: rejected,
            exact_stop_confirmed: stopped,
            ifeo_registration_tested: false,
        })
    })();
    let cleanup = child.terminate();
    if let Err(error) = cleanup {
        return Err(error.into());
    }
    result
}

pub fn package_probe(app: &str) -> Result<PackageReport> {
    identity::assert_ordinary_user()?;
    let package = package::discover(app)?;
    // A package LocalState root remains visible both inside and outside virtualization.
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA_MISSING")?;
    let parent = PathBuf::from(local)
        .join("Packages")
        .join(&package.family_name)
        .join("LocalState");
    if !parent.is_dir() {
        return Err("PACKAGE_LOCAL_STATE_MISSING".into());
    }
    let directory = tempfile::Builder::new()
        .prefix("AppProxyRust-M0-")
        .tempdir_in(parent)?;
    let request = make_request(directory.path(), Some(package.family_name.clone()), 0)?;
    package::activate_probe(
        &package,
        &helper_path()?,
        &directory.path().join("request.json"),
    )?;
    let receipt = await_receipt(directory.path(), &request)?;
    if receipt.identity.image_file != identity::file_identity(&helper_path()?)? {
        return Err("PACKAGE_HELPER_IMAGE_MISMATCH".into());
    }
    Ok(PackageReport {
        application: package,
        helper_identity: receipt.identity,
        package_family_confirmed: true,
        package_write_visible_outside: true,
        target_application_started: false,
    })
}
