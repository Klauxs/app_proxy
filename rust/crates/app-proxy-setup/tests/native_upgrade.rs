#![cfg(windows)]
use app_proxy_setup::{FilePayload, Installation, Payload};
use app_proxy_windows::{identity, process, setup, store::Store};
use sha2::{Digest, Sha256};
use std::{
    os::windows::process::CommandExt,
    path::Path,
    process::Command,
    time::{Duration, Instant},
};

fn run(root: &Path, home: &Path, operation: &str) -> Vec<u8> {
    let mut command = Command::new(root.join("app-proxy.exe"));
    command.arg("--home").arg(home).arg(operation);
    if operation == "status" {
        command.arg("--json");
    }
    let output = command.creation_flags(0x08000000).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
#[ignore = "requires APP_PROXY_SETUP_TEST_PAYLOAD with the freshly built executable pair; creates only temporary user stores"]
fn real_pair_drains_coordinator_upgrades_restarts_and_preserves_configuration() {
    let payload_root =
        std::path::PathBuf::from(std::env::var_os("APP_PROXY_SETUP_TEST_PAYLOAD").unwrap());
    let bytes = ["app-proxy.exe", "app-proxy-host.exe"]
        .map(|n| std::fs::read(payload_root.join(n)).unwrap());
    let hashes = bytes.each_ref().map(|b| format!("{:x}", Sha256::digest(b)));
    let payload = Payload {
        build: "fixture-v1",
        files: [0, 1].map(|i| FilePayload {
            bytes: &bytes[i],
            sha256: &hashes[i],
        }),
    };
    let temp = tempfile::Builder::new()
        .prefix(".setup-upgrade-test-")
        .tempdir_in(app_proxy_windows::layout::ensure_root().unwrap())
        .unwrap();
    let root = temp.path().join("program");
    std::fs::create_dir(&root).unwrap();
    let home = temp.path().join("data");
    drop(Store::create(&home).unwrap());
    let before = std::fs::read(home.join("manifest.json")).unwrap();
    let mut transaction = Installation::begin(&root, &payload).unwrap();
    run(&root, &home, "setup-prepare");
    transaction.release_for_verification();
    run(&root, &home, "setup-verify");
    transaction.complete().unwrap();
    let status: serde_json::Value = serde_json::from_slice(&run(&root, &home, "status")).unwrap();
    let old_process =
        identity::inspect(status["coordinator_pid"].as_u64().unwrap() as u32).unwrap();
    // Distinct valid PE images force replacement instead of the identical-byte path.
    let mut newer = bytes.clone();
    for b in &mut newer {
        b.extend_from_slice(b"AppProxy setup upgrade fixture v2");
    }
    let next_hashes = newer.each_ref().map(|b| format!("{:x}", Sha256::digest(b)));
    let next = Payload {
        build: "fixture-v2",
        files: [0, 1].map(|i| FilePayload {
            bytes: &newer[i],
            sha256: &next_hashes[i],
        }),
    };
    let mut transaction = Installation::begin(&root, &next).unwrap();
    assert!(!process::is_running_exact(&old_process).unwrap());
    run(&root, &home, "setup-prepare");
    transaction.release_for_verification();
    run(&root, &home, "setup-verify");
    transaction.complete().unwrap();
    assert_eq!(std::fs::read(home.join("manifest.json")).unwrap(), before);
    let status: serde_json::Value = serde_json::from_slice(&run(&root, &home, "status")).unwrap();
    let next_process =
        identity::inspect(status["coordinator_pid"].as_u64().unwrap() as u32).unwrap();
    assert_ne!(old_process.creation_time, next_process.creation_time);
    let _maintenance = setup::Maintenance::acquire(&root).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while process::is_running_exact(&next_process).unwrap() {
        assert!(
            Instant::now() < deadline,
            "fixture coordinator did not drain"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    drop(_maintenance);
}

#[test]
#[ignore = "explicit administrator approval required twice; fixture metadata retained for scoped elevated cleanup"]
fn authorized_listener_install_and_generation_upgrade() {
    assert_eq!(
        std::env::var("APP_PROXY_SETUP_ETW_TEST").as_deref(),
        Ok("1")
    );
    let payload_root =
        std::path::PathBuf::from(std::env::var_os("APP_PROXY_SETUP_TEST_PAYLOAD").unwrap());
    let bytes = ["app-proxy.exe", "app-proxy-host.exe"]
        .map(|n| std::fs::read(payload_root.join(n)).unwrap());
    let hashes = bytes.each_ref().map(|b| format!("{:x}", Sha256::digest(b)));
    let payload = Payload {
        build: "etw-fixture-v1",
        files: [0, 1].map(|i| FilePayload {
            bytes: &bytes[i],
            sha256: &hashes[i],
        }),
    };
    let base = tempfile::Builder::new()
        .prefix("AppProxy-Setup-ETW-")
        .tempdir()
        .unwrap()
        .keep();
    let root = base.join("program");
    std::fs::create_dir(&root).unwrap();
    let home = base.join("data");
    let fixture = base.join("never-launched-fixture.exe");
    std::fs::write(&fixture, &bytes[1]).unwrap();
    let mut store = Store::create(&home).unwrap();
    let header = store.load().unwrap();
    let mut sample: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    sample["store_id"] = serde_json::json!(header.store_id);
    sample["owner_sid"] = serde_json::json!(header.owner_sid);
    sample["revision"] = serde_json::json!(header.revision);
    sample["applications"][0]["name"] = serde_json::json!("Setup fixture");
    sample["applications"][0]["locator"] = serde_json::json!({"kind":"exe", "path":fixture});
    sample["applications"][0]["template_ref"] = serde_json::json!("builtin.chromium@1");
    sample["instances"].as_array_mut().unwrap().truncate(1);
    store
        .commit(header.revision, serde_json::from_value(sample).unwrap())
        .unwrap();
    std::fs::write(
        base.join("fixture.json"),
        serde_json::to_vec_pretty(
            &serde_json::json!({"root":root,"home":home,"store":header.store_id}),
        )
        .unwrap(),
    )
    .unwrap();
    println!("isolated ETW fixture: {}", base.display());
    drop(store);
    let mut transaction = Installation::begin(&root, &payload).unwrap();
    run(&root, &home, "setup-prepare");
    transaction.release_for_verification();
    run(&root, &home, "setup-verify");
    transaction.complete().unwrap();
    let before_generation =
        app_proxy_windows::guard_deployment::Deployment::listener(header.store_id)
            .unwrap()
            .generation();
    let before = std::fs::read(home.join("manifest.json")).unwrap();
    let mut newer = bytes;
    for b in &mut newer {
        b.extend_from_slice(b"AppProxy elevated upgrade fixture v2");
    }
    let next_hashes = newer.each_ref().map(|b| format!("{:x}", Sha256::digest(b)));
    let next = Payload {
        build: "etw-fixture-v2",
        files: [0, 1].map(|i| FilePayload {
            bytes: &newer[i],
            sha256: &next_hashes[i],
        }),
    };
    let mut transaction = Installation::begin(&root, &next).unwrap();
    run(&root, &home, "setup-prepare");
    transaction.release_for_verification();
    run(&root, &home, "setup-verify");
    transaction.complete().unwrap();
    let deployment =
        app_proxy_windows::guard_deployment::Deployment::listener(header.store_id).unwrap();
    app_proxy_windows::guard_task::verify_registered(&deployment).unwrap();
    assert_ne!(before_generation, deployment.generation());
    assert_eq!(std::fs::read(home.join("manifest.json")).unwrap(), before);
    std::fs::write(base.join("result.json"), serde_json::to_vec_pretty(&serde_json::json!({"before":before_generation,"after":deployment.generation(),"host":deployment.host_path(),"store":header.store_id,"result":"active_etw before and after; manifest unchanged"})).unwrap()).unwrap();
    drop(deployment);
    let status: serde_json::Value = serde_json::from_slice(&run(&root, &home, "status")).unwrap();
    let process = identity::inspect(status["coordinator_pid"].as_u64().unwrap() as u32).unwrap();
    let _maintenance = setup::Maintenance::acquire(&root).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while process::is_running_exact(&process).unwrap() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    let store = Store::open_expected(&home, Some(header.store_id)).unwrap();
    if let Some((_, _, registration)) = store.login_registration().unwrap() {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match registration.remove_idle() {
                Ok(()) => break,
                Err(app_proxy_windows::Error::Invalid("GUARD_LOGIN_TASK_RUNNING"))
                    if Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(100))
                }
                Err(e) => panic!("fixture login cleanup: {e}"),
            }
        }
    }
    println!(
        "ETW generation upgrade passed; elevated event task cleanup requires explicit UAC. Evidence: {}",
        base.display()
    );
}
