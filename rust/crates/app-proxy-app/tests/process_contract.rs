#![cfg(windows)]

use serde_json::Value;
use std::process::Command;

fn run_probe(debug: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_app-proxy"));
    command
        .args(["probe", "process"])
        .env("APP_PROXY_PROBE_REMOVE", "must not reach the child")
        .env("APP_PROXY_PROBE_VALUE", "must be replaced only in child");
    if debug {
        command.arg("--debug-detach");
    }
    let output = command.output().expect("run probe CLI");
    assert!(
        output.status.success(),
        "probe failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("JSON report");
    for check in [
        "args_and_cwd_match",
        "environment_set_unset_match",
        "parent_environment_unchanged",
        "survived_creating_thread",
        "forged_identity_rejected",
        "exact_stop_confirmed",
    ] {
        assert_eq!(report[check], true, "{check}");
    }
    assert_eq!(report["ifeo_registration_tested"], false);
    assert!(report["identity"]["creation_time"].as_u64().unwrap() > 0);
}

#[test]
fn normal_spawn_roundtrips_real_windows_args_environment_and_identity() {
    run_probe(false);
}

#[test]
fn debuggee_survives_detach_and_debug_thread_exit() {
    run_probe(true);
}

#[test]
fn expired_helper_request_does_not_write_or_claim() {
    let root = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    std::fs::write(
        root.path().join(".app-proxy-rust-probe"),
        serde_json::to_vec(&id).unwrap(),
    )
    .unwrap();
    let request = root.path().join("request.json");
    std::fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({
            "protocol": 1, "id": id, "expires_at": 0, "expected_package": null,
            "hold_ms": 0, "environment_names": []
        }))
        .unwrap(),
    )
    .unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .arg("probe-child")
        .arg("--request")
        .arg(&request)
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!root.path().join("receipt.json").exists());
    assert!(!root.path().join("claim").exists());
}

#[test]
fn package_identity_mismatch_is_rejected_before_writing() {
    let root = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    std::fs::write(
        root.path().join(".app-proxy-rust-probe"),
        serde_json::to_vec(&id).unwrap(),
    )
    .unwrap();
    let request = root.path().join("request.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(&request, serde_json::to_vec(&serde_json::json!({
        "protocol": 1, "id": id, "expires_at": now + 20, "expected_package": "not-the-current-package",
        "hold_ms": 0, "environment_names": []
    })).unwrap()).unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .arg("probe-child")
        .arg("--request")
        .arg(&request)
        .status()
        .unwrap();
    assert!(!status.success());
    assert!(!root.path().join("receipt.json").exists());
    assert!(!root.path().join("claim").exists());
}

#[test]
fn helper_replay_is_rejected_without_replacing_receipt() {
    let root = tempfile::tempdir().unwrap();
    let id = uuid::Uuid::new_v4();
    std::fs::write(
        root.path().join(".app-proxy-rust-probe"),
        serde_json::to_vec(&id).unwrap(),
    )
    .unwrap();
    let request = root.path().join("request.json");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(
        &request,
        serde_json::to_vec(&serde_json::json!({
            "protocol": 1, "id": id, "expires_at": now + 20, "expected_package": null,
            "hold_ms": 0, "environment_names": []
        }))
        .unwrap(),
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
            .arg("probe-child")
            .arg("--request")
            .arg(&request)
            .status()
            .unwrap()
    };
    assert!(run().success());
    let first = std::fs::read(root.path().join("receipt.json")).unwrap();
    assert!(!run().success());
    assert_eq!(
        std::fs::read(root.path().join("receipt.json")).unwrap(),
        first
    );
}

#[test]
#[ignore = "requires a current-user Claude MSIX installation; only starts our probe helper"]
fn claude_package_helper_receipt_is_visible_outside_package() {
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .args(["probe", "package", "claude"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["package_family_confirmed"], true);
    assert_eq!(report["package_write_visible_outside"], true);
    assert_eq!(report["target_application_started"], false);
    assert_eq!(report["application"]["family_name"], "Claude_pzs8sxrjxfjjc");
}
