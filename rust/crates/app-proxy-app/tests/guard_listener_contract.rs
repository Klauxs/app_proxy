#![cfg(windows)]
use std::process::Command;
use uuid::Uuid;

#[test]
fn real_host_parses_fixed_task_action_but_refuses_ordinary_listener() {
    app_proxy_windows::identity::assert_ordinary_user().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .args([
            "event-listen",
            "--store",
            &Uuid::new_v4().to_string(),
            "--generation",
            &Uuid::new_v4().to_string(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("ELEVATED_USER_REQUIRED"));
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .args([
            "event-listen",
            "--store",
            "not-a-uuid",
            "--generation",
            "not-a-uuid",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}
