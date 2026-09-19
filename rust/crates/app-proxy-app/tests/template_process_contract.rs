#![cfg(windows)]
use app_proxy_core::{
    model::*,
    template::{self, TemplatePaths},
};
use app_proxy_windows::{process, store::Store};
use std::path::{Path, PathBuf};
use std::time::Duration;

fn powershell() -> PathBuf {
    PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe")
}

// Selected only by the two parent tests below. No global environment mutation in
// the test runner: the parent supplies inherited pollution to a dedicated child.
#[test]
#[ignore = "subprocess fixture selected by the parent contract tests"]
fn template_receipt_helper() {
    let Some(root) = std::env::var_os("APP_PROXY_TEST_TEMPLATE_STORE") else {
        return;
    };
    let store = Store::open(Path::new(&root)).unwrap();
    let manifest = store.load().unwrap();
    let index: usize = std::env::var("APP_PROXY_TEST_TEMPLATE_INDEX")
        .unwrap()
        .parse()
        .unwrap();
    let instance = &manifest.instances[index];
    let prepared = store.prepare_instance_data(instance.id, None).unwrap();
    let executable = powershell();
    let compiled = template::compile(
        &manifest,
        instance.id,
        TemplatePaths {
            executable: &executable,
            instance_root: prepared.as_ref().map(|p| p.paths.root.as_path()),
        },
        |_| panic!("no fixture secrets"),
    )
    .unwrap();
    let script = PathBuf::from(std::env::var_os("APP_PROXY_TEST_TEMPLATE_SCRIPT").unwrap());
    let mut args = vec![
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.into_os_string(),
    ];
    args.extend(compiled.args);
    let native_patch = app_proxy_core::EnvPatch {
        set: compiled.environment.set.clone(),
        unset: compiled.environment.unset.clone(),
    };
    let mut child = process::spawn(process::SpawnSpec {
        exe: executable,
        args,
        cwd: compiled.cwd.clone(),
        environment: compiled.environment,
        mode: process::CreationMode::Normal,
    })
    .unwrap();
    if let Some(exit) = child.wait_timeout(Duration::from_secs(5)).unwrap() {
        assert!(exit.success());
    } else {
        child.terminate().unwrap();
        panic!("fixture did not finish");
    }
    // PowerShell/.NET may conflate an empty environment variable with absence.
    // A native Rust child observes the same patch independently.
    let mut native = process::spawn(process::SpawnSpec {
        exe: std::env::current_exe().unwrap(),
        args: ["--exact", "native_environment_receipt", "--ignored"]
            .into_iter()
            .map(Into::into)
            .collect(),
        cwd: compiled.cwd,
        environment: native_patch,
        mode: process::CreationMode::Normal,
    })
    .unwrap();
    if let Some(exit) = native.wait_timeout(Duration::from_secs(5)).unwrap() {
        assert!(exit.success());
    } else {
        native.terminate().unwrap();
        panic!("native fixture did not finish");
    }
}

#[test]
#[ignore = "native subprocess fixture selected by template_receipt_helper"]
fn native_environment_receipt() {
    let path = PathBuf::from(std::env::var_os("APP_PROXY_TEST_REPORT").unwrap())
        .with_extension("native.json");
    let value: std::collections::BTreeMap<_, _> = [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "CODEX_HOME",
        "CODEX_ELECTRON_USER_DATA_PATH",
        "CLAUDE_CONFIG_DIR",
        "EMPTY",
        "REMOVE_ME",
    ]
    .into_iter()
    .map(|key| (key, std::env::var(key).ok()))
    .collect();
    std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
}

fn run(index: usize) -> (tempfile::TempDir, serde_json::Value) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("store 中文");
    let report = fixture.path().join("receipt.json");
    let script = fixture.path().join("child.ps1");
    std::fs::write(&script, r#"$ErrorActionPreference='Stop'
$value=[ordered]@{args=@($args);cwd=(Get-Location).Path;http=$env:HTTP_PROXY;https=$env:HTTPS_PROXY;all=$env:ALL_PROXY;no=$env:NO_PROXY;codex=$env:CODEX_HOME;user_data=$env:CODEX_ELECTRON_USER_DATA_PATH;claude=$env:CLAUDE_CONFIG_DIR;empty=$env:EMPTY;removed=$env:REMOVE_ME}
[System.IO.File]::WriteAllText($env:APP_PROXY_TEST_REPORT, ($value | ConvertTo-Json -Depth 4))
"#).unwrap();
    let mut store = Store::create(&root).unwrap();
    let header = store.load().unwrap();
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    manifest.store_id = header.store_id;
    manifest.owner_sid = header.owner_sid;
    manifest.applications[0].locator = ApplicationLocator::Exe { path: powershell() };
    manifest.instances[0].network = NetworkBinding::Direct {};
    manifest.instances[0].guard.desired = Desired::Disabled;
    manifest.instances[1].network = NetworkBinding::Profile {
        profile_id: manifest.profiles[0].id,
    };
    for instance in &mut manifest.instances {
        instance.env.set.insert(
            "APP_PROXY_TEST_REPORT".into(),
            EnvValue::Literal {
                value: report.to_str().unwrap().into(),
            },
        );
        instance.env.set.insert(
            "EMPTY".into(),
            EnvValue::Literal {
                value: String::new(),
            },
        );
        instance.env.unset.push("REMOVE_ME".into());
        instance.args = vec!["--test-tag=hello space".into()];
    }
    manifest.instances[1]
        .args
        .push("--test-file=${user_data}/a b.json".into());
    store.commit(1, manifest).unwrap();
    drop(store);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "template_receipt_helper",
            "--ignored",
            "--nocapture",
        ])
        .env("APP_PROXY_TEST_TEMPLATE_STORE", &root)
        .env("APP_PROXY_TEST_TEMPLATE_INDEX", index.to_string())
        .env("APP_PROXY_TEST_TEMPLATE_SCRIPT", script)
        .env("http_proxy", "http://old.invalid:1")
        .env("HTTPS_PROXY", "http://old.invalid:2")
        .env("ALL_PROXY", "socks5://old.invalid:3")
        .env("NO_PROXY", "*")
        .env("CODEX_HOME", r"C:\unrelated-data")
        .env("CODEX_ELECTRON_USER_DATA_PATH", r"C:\unrelated-user-data")
        .env("CLAUDE_CONFIG_DIR", r"C:\another-app-home")
        .env("REMOVE_ME", "must disappear")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut receipt: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    receipt["native"] =
        serde_json::from_slice(&std::fs::read(report.with_extension("native.json")).unwrap())
            .unwrap();
    (fixture, receipt)
}

#[test]
fn original_child_does_not_inherit_another_instances_data_or_proxy() {
    let (fixture, receipt) = run(0);
    for name in [
        "http",
        "https",
        "all",
        "no",
        "codex",
        "user_data",
        "claude",
        "removed",
    ] {
        assert!(receipt[name].is_null(), "{name}: {}", receipt[name]);
    }
    assert_eq!(receipt["native"]["EMPTY"], "");
    for name in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "NO_PROXY",
        "CODEX_HOME",
        "CODEX_ELECTRON_USER_DATA_PATH",
        "CLAUDE_CONFIG_DIR",
        "REMOVE_ME",
    ] {
        assert!(receipt["native"][name].is_null());
    }
    assert_eq!(
        receipt["args"],
        serde_json::json!(["--test-tag=hello space", "--no-proxy-server"])
    );
    assert!(!fixture.path().join("store 中文/instances").exists());
}

#[test]
fn isolated_child_gets_its_own_paths_and_bound_proxy() {
    let (fixture, receipt) = run(1);
    let store = Store::open(&fixture.path().join("store 中文")).unwrap();
    let instance_id = store.load().unwrap().instances[1].id;
    let data = store
        .prepare_instance_data(instance_id, None)
        .unwrap()
        .unwrap();
    assert_eq!(receipt["user_data"], data.paths.user_data.to_str().unwrap());
    assert_eq!(receipt["codex"], data.paths.app_home.to_str().unwrap());
    assert!(receipt["claude"].is_null());
    for name in ["http", "https", "all"] {
        assert_eq!(receipt[name], "http://127.0.0.1:18099");
    }
    assert_eq!(receipt["no"], "");
    assert_eq!(receipt["native"]["EMPTY"], "");
    assert_eq!(receipt["native"]["NO_PROXY"], "");
    assert_eq!(
        receipt["args"][1],
        format!("--test-file={}/a b.json", data.paths.user_data.display())
    );
    assert_eq!(
        receipt["args"][2],
        format!("--user-data-dir={}", data.paths.user_data.display())
    );
    assert_eq!(receipt["args"][3], "--proxy-server=http://127.0.0.1:18099");
}
