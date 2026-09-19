#![cfg(windows)]
use app_proxy_app::coordinator::Status;
use app_proxy_windows::{identity, process, store::Store};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn query(root: &Path) -> Status {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(root)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "CLI output stayed open after response"
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

struct OwnedFixture(Option<app_proxy_core::ProcessIdentity>);
impl OwnedFixture {
    fn capture(status: &Status) -> Self {
        let observed = identity::inspect(status.coordinator_pid).unwrap();
        assert_eq!(
            observed.image_file,
            identity::file_identity(Path::new(env!("CARGO_BIN_EXE_app-proxy-host"))).unwrap()
        );
        Self(Some(observed))
    }
    fn stop(&mut self) {
        if let Some(identity) = self.0.take() {
            process::terminate_exact(&identity).unwrap();
        }
    }
}
impl Drop for OwnedFixture {
    fn drop(&mut self) {
        if let Some(identity) = self.0.take() {
            let _ = process::terminate_exact(&identity);
        }
    }
}

#[test]
fn simultaneous_fresh_clients_share_one_owner_and_recover_after_its_exit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("新配置 store");
    let clients: Vec<_> = (0..6)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || query(&root))
        })
        .collect();
    let mut results = clients.into_iter().map(|client| client.join().unwrap());
    let first = results.next().unwrap();
    let mut fixture = OwnedFixture::capture(&first);
    for status in results {
        assert_eq!(status.store_id, first.store_id);
        assert_eq!(status.epoch, first.epoch);
        assert_eq!(status.coordinator_pid, first.coordinator_pid);
    }
    assert!(Store::open(&root).is_err());
    let trailing_separator = std::path::PathBuf::from(format!("{}\\", root.display()));
    let repeated = query(&trailing_separator);
    assert_eq!(repeated.epoch, first.epoch);
    fixture.stop();
    let mut editable = Store::open(&root).unwrap();
    editable.commit(1, editable.load().unwrap()).unwrap();
    drop(editable);
    let replacement = query(&root);
    let mut replacement_fixture = OwnedFixture::capture(&replacement);
    assert_ne!(replacement.epoch, first.epoch);
    assert_eq!(replacement.store_id, first.store_id);
    assert_eq!(replacement.revision, 2);
    replacement_fixture.stop();
    Store::open(&root).unwrap();
}

#[test]
fn corrupt_store_does_not_start_an_owner_or_replace_user_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    std::fs::write(root.join("manifest.json"), "broken sensitive value").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args(["status", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("INVALID_STORE_JSON"));
    assert!(!error.contains("sensitive"));
    assert_eq!(
        std::fs::read_to_string(root.join("manifest.json")).unwrap(),
        "broken sensitive value"
    );
}

#[test]
fn short_lived_untrusted_clients_cannot_stop_the_owner() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let first = query(&root);
    let _fixture = OwnedFixture::capture(&first);
    let sid = identity::current().unwrap().user_sid;
    let pipe = app_proxy_windows::ipc::address(first.store_id, &sid).unwrap();
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    // Connect and close immediately without sending a frame. Peer authentication can
    // race either the disconnect or process exit; neither may stop the coordinator.
    let output = Command::new(powershell).args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; 1..20 | ForEach-Object { $p=[System.IO.Pipes.NamedPipeClientStream]::new('.', $env:APP_PROXY_TEST_PIPE, [System.IO.Pipes.PipeDirection]::InOut); $p.Connect(2000); $p.Dispose() }"])
        .env("APP_PROXY_TEST_PIPE", pipe.strip_prefix(r"\\.\pipe\").unwrap()).output().unwrap();
    assert!(output.status.success());
    let after = query(&root);
    assert_eq!(after.epoch, first.epoch);
    assert_eq!(after.coordinator_pid, first.coordinator_pid);
}

#[test]
fn resource_free_coordinator_exits_after_idle_timeout() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    let mut child = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .arg("serve")
        .arg("--home")
        .arg(&root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut fixture = OwnedFixture(Some(identity::inspect(child.id()).unwrap()));
    // The server may still be binding its pipe; wait for it without launching a second owner.
    std::thread::sleep(Duration::from_millis(250));
    let status = query(&root);
    assert_eq!(status.coordinator_pid, child.id());
    let deadline = Instant::now() + Duration::from_secs(35);
    loop {
        if let Some(exit) = child.try_wait().unwrap() {
            assert!(exit.success());
            fixture.0 = None;
            break;
        }
        assert!(Instant::now() < deadline, "idle coordinator did not exit");
        std::thread::sleep(Duration::from_millis(100));
    }
    Store::open(&root).unwrap();
}
