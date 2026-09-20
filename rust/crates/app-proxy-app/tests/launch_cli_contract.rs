#![cfg(windows)]
use app_proxy_app::coordinator::Status;
use app_proxy_core::{ProcessIdentity, model::*};
use app_proxy_windows::{identity, process, store::Store};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    time::{Duration, Instant},
};
use uuid::Uuid;

struct Fixture {
    root: PathBuf,
    exe: PathBuf,
    events: PathBuf,
    instance: Uuid,
    owner: Option<ProcessIdentity>,
    _temp: tempfile::TempDir,
}
impl Fixture {
    fn new(valid: bool, proxy: bool) -> Self {
        let mut fixture = Self::unconnected(valid, proxy);
        fixture.capture_owner();
        fixture
    }
    fn unconnected(valid: bool, proxy: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let exe = temp
            .path()
            .join(format!("cli-launch-{}.exe", Uuid::new_v4()));
        if valid {
            fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        } else {
            fs::write(&exe, b"invalid PE fixture").unwrap();
        }
        let events = temp.path().join("events");
        fs::create_dir(&events).unwrap();
        let mut store = Store::create(&root).unwrap();
        let mut manifest = store.load().unwrap();
        let application_id = Uuid::new_v4();
        let instance = Uuid::new_v4();
        manifest.applications.push(Application {
            id: application_id,
            name: "CLI fixture".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path: exe.clone() },
            template_ref: Template::Environment,
        });
        let network = if proxy {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let profile = Uuid::new_v4();
            let node = Uuid::new_v4();
            manifest.profiles.push(ProxyProfile {
                id: profile,
                name: "offline".into(),
                revision: 1,
                kind: ProxyKind::Managed,
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: listener.local_addr().unwrap().port(),
                },
                selected_node_id: node,
                source: ProxySource::Manual {
                    nodes: vec![ManualNode {
                        id: node,
                        name: "fixture".into(),
                        protocol: ManualProtocol::Http,
                        host: "127.0.0.1".into(),
                        port: 1,
                        credentials: None,
                    }],
                },
            });
            NetworkBinding::Profile {
                profile_id: profile,
            }
        } else {
            NetworkBinding::Direct {}
        };
        manifest.instances.push(Instance {
            id: instance,
            application_id,
            name: "CLI fixture".into(),
            revision: 1,
            data: InstanceData::Original {},
            args: vec!["--ignored".into(), "--exact".into(), "launch_child".into()],
            env: SavedEnvironment {
                set: [
                    (
                        "APP_PROXY_CLI_EVENTS".into(),
                        EnvValue::Literal {
                            value: events.to_str().unwrap().into(),
                        },
                    ),
                    (
                        "APP_PROXY_CLI_PRIVATE".into(),
                        EnvValue::Literal {
                            value: "never-display-cli-private-value".into(),
                        },
                    ),
                ]
                .into(),
                unset: vec![],
            },
            cwd: WorkingDirectory::Explicit {
                path: temp.path().to_owned(),
            },
            network,
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
        store.commit(manifest.revision, manifest).unwrap();
        drop(store);
        Self {
            root,
            exe,
            events,
            instance,
            owner: None,
            _temp: temp,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_app-proxy"));
        command.arg("--home").arg(&self.root).args(args);
        command
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn capture_owner(&mut self) {
        let status: Status = serde_json::from_value(self.ok(&["status", "--json"])).unwrap();
        let process = identity::inspect(status.coordinator_pid).unwrap();
        assert_eq!(
            process.image_file,
            identity::file_identity(Path::new(env!("CARGO_BIN_EXE_app-proxy-host"))).unwrap()
        );
        self.owner = Some(process);
    }
    fn stop_owner(&mut self) {
        if let Some(owner) = self.owner.take() {
            process::terminate_exact(&owner).unwrap();
        }
    }
    fn events(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while fs::read_dir(&self.events).unwrap().count() != count {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(bytes) = fs::read(self.root.join("state/launch.json"))
            && let Ok(journal) = serde_json::from_slice::<Value>(&bytes)
            && let Some(attempts) = journal["attempts"].as_array()
        {
            for attempt in attempts {
                if let Ok(process) =
                    serde_json::from_value::<ProcessIdentity>(attempt["phase"]["process"].clone())
                    && identity::file_identity(&self.exe)
                        .is_ok_and(|image| image == process.image_file)
                {
                    let _ = process::terminate_exact(&process);
                }
            }
        }
        if let Some(owner) = self.owner.take() {
            let _ = process::terminate_exact(&owner);
        }
    }
}

#[test]
fn hidden_shortcut_launch_reuses_engine_and_running_session_without_output() {
    let fixture = Fixture::new(true, false);
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
            .arg("launch")
            .arg(fixture.instance.to_string())
            .arg("--home")
            .arg(&fixture.root)
            .arg("--notify")
            .output()
            .unwrap()
    };
    let first = run();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(first.stdout.is_empty() && first.stderr.is_empty());
    fixture.events(1);
    let before: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/launch.json")).unwrap()).unwrap();
    let attempts = before["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["origin"], "shortcut");
    let process: ProcessIdentity =
        serde_json::from_value(attempts[0]["phase"]["process"].clone()).unwrap();
    assert!(process::is_running_exact(&process).unwrap());
    let repeated = run();
    assert!(repeated.status.success());
    assert!(repeated.stdout.is_empty() && repeated.stderr.is_empty());
    fixture.events(1);
    let after: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/launch.json")).unwrap()).unwrap();
    assert_eq!(after["attempts"], before["attempts"]);
}

#[test]
fn hidden_entry_rejects_missing_store_and_reports_exact_failed_request_without_ui() {
    let temp = tempfile::tempdir().unwrap();
    let absent = temp.path().join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
        .args(["launch", &Uuid::new_v4().to_string(), "--home"])
        .arg(&absent)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!absent.exists());
    for proxy in [false, true] {
        let fixture = Fixture::new(proxy, proxy);
        let output = Command::new(env!("CARGO_BIN_EXE_app-proxy-host"))
            .arg("launch")
            .arg(fixture.instance.to_string())
            .arg("--home")
            .arg(&fixture.root)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let report = String::from_utf8(output.stderr).unwrap();
        let journal: Value =
            serde_json::from_slice(&fs::read(fixture.root.join("state/launch.json")).unwrap())
                .unwrap();
        let attempts = journal["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["phase"]["phase"], "failed");
        assert_eq!(attempts[0]["origin"], "shortcut");
        assert!(report.contains(attempts[0]["id"].as_str().unwrap()));
        assert!(!report.contains("never-display-cli-private-value"));
        assert_eq!(fs::read_dir(&fixture.events).unwrap().count(), 0);
    }
}

#[test]
#[ignore = "opens an isolated foreground console and selects Return; explicit desktop validation"]
fn shortcut_missing_core_returns_from_shared_foreground_without_launch() {
    use std::os::windows::process::CommandExt;
    let temp = tempfile::tempdir().unwrap();
    let result = temp.path().join("result.txt");
    // Preserve the production peer policy: this test client occupies the CLI
    // slot beside a private copy of the real host, not a bypass or extra peer.
    let client = temp.path().join("app-proxy.exe");
    fs::copy(std::env::current_exe().unwrap(), &client).unwrap();
    fs::copy(
        env!("CARGO_BIN_EXE_app-proxy-host"),
        temp.path().join("app-proxy-host.exe"),
    )
    .unwrap();
    let mut child = Command::new(client)
        .args(["--ignored", "--exact", "shortcut_prompt_child"])
        .env("APP_PROXY_SHORTCUT_TEST_OUTPUT", &result)
        .creation_flags(0x00000008) // Match a GUI host with no associated console.
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            let output = child.wait_with_output().unwrap();
            assert!(
                status.success(),
                "{status:?}\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("shortcut prompt fixture timed out");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        fs::read(result).unwrap(),
        b"returned without application or installation"
    );
}

#[test]
#[ignore = "child for shortcut_missing_core_returns_from_shared_foreground_without_launch"]
fn shortcut_prompt_child() {
    let output =
        PathBuf::from(std::env::var_os("APP_PROXY_SHORTCUT_TEST_OUTPUT").expect("fixture output"));
    let mut fixture = Fixture::unconnected(true, true);
    let writer = std::thread::spawn(|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if app_proxy_windows::console::test_support::line("2\r").is_ok() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "shared prompt console never appeared"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(app_proxy_app::launch_cli::from_shortcut(
        fixture.root.clone(),
        fixture.instance,
        true,
    ));
    let status = runtime
        .block_on(app_proxy_app::coordinator::status(fixture.root.clone()))
        .unwrap();
    let owner = identity::inspect(status.coordinator_pid).unwrap();
    assert_eq!(
        owner.image_file,
        identity::file_identity(
            &std::env::current_exe()
                .unwrap()
                .with_file_name("app-proxy-host.exe")
        )
        .unwrap()
    );
    fixture.owner = Some(owner);
    writer.join().unwrap();
    let error = result.unwrap_err();
    assert_eq!(error.exit_code, 5);
    let journal: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/launch.json")).unwrap()).unwrap();
    let attempts = journal["attempts"].as_array().unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0]["phase"]["code"], "CORE_BINARY_MISSING");
    assert!(
        error
            .to_string()
            .contains(attempts[0]["id"].as_str().unwrap())
    );
    assert_eq!(fs::read_dir(&fixture.events).unwrap().count(), 0);
    assert!(!fixture.root.join("state/core-requests").exists());
    fs::write(output, b"returned without application or installation").unwrap();
}

#[test]
fn cli_launch_is_once_only_survives_owner_restart_and_keeps_historical_receipts() {
    let mut fixture = Fixture::new(true, false);
    let instance = fixture.instance.to_string();
    let request = Uuid::new_v4().to_string();
    let launched = fixture.ok(&["launch", &instance, "--request-id", &request, "--json"]);
    let child: ProcessIdentity =
        serde_json::from_value(launched["attempt"]["phase"]["process"].clone()).unwrap();
    assert!(process::is_running_exact(&child).unwrap());
    fixture.events(1);
    for replay in [
        fixture.ok(&["launch", &instance, "--request-id", &request, "--json"]),
        fixture.ok(&["launch", &instance, "--json"]),
    ] {
        assert_eq!(replay["attempt"]["id"], request);
        assert_eq!(
            replay["attempt"]["phase"]["process"],
            launched["attempt"]["phase"]["process"]
        );
        assert!(
            !replay
                .to_string()
                .contains("never-display-cli-private-value")
        );
    }
    let conflict = fixture.cli(&[
        "launch",
        &Uuid::new_v4().to_string(),
        "--request-id",
        &request,
        "--json",
    ]);
    assert_eq!(conflict.status.code(), Some(2));
    assert_eq!(
        serde_json::from_slice::<Value>(&conflict.stdout).unwrap()["error"],
        "REQUEST_ID_CONFLICT"
    );
    fixture.stop_owner();
    let recovered = fixture.ok(&["launch", "inspect", &request, "--json"]);
    fixture.capture_owner();
    assert_eq!(recovered["attempt"]["phase"], launched["attempt"]["phase"]);
    fixture.ok(&["launch", "cancel", &request, "--json"]);
    assert!(process::is_running_exact(&child).unwrap());
    fixture.events(1);
    process::terminate_exact(&child).unwrap();
    let exited = fixture.ok(&["launch", "inspect", &request, "--json"]);
    assert_eq!(exited["attempt"]["session_exited"], true);
    fixture.ok(&["instance", "remove", &instance, "--json"]);
    let historical = fixture.ok(&["launch", &instance, "--request-id", &request, "--json"]);
    assert_eq!(historical["attempt"]["phase"], launched["attempt"]["phase"]);
    fixture.events(1);
}

#[test]
fn instance_inspect_observes_disabled_guard_session_and_keeps_historical_network() {
    let fixture = Fixture::new(true, false);
    let instance = fixture.instance.to_string();
    let before = fixture.ok(&["instance", "inspect", &instance, "--json"]);
    assert_eq!(before["runtime"]["observation"]["state"], "absent");
    assert_eq!(before["protection"]["phase"], "disabled");
    let launched = fixture.ok(&["launch", &instance, "--json"]);
    let child: ProcessIdentity =
        serde_json::from_value(launched["attempt"]["phase"]["process"].clone()).unwrap();
    fixture.events(1);
    let running = fixture.ok(&["instance", "inspect", &instance, "--json"]);
    assert_eq!(running["runtime"]["observation"]["state"], "session");
    assert_eq!(
        running["runtime"]["observation"]["process"]["pid"],
        child.pid
    );
    assert_eq!(
        running["runtime"]["observation"]["configuration_changed"],
        false
    );
    assert_eq!(running["target_traffic_evidence"], "not_observed");
    assert!(
        !running
            .to_string()
            .contains("never-display-cli-private-value")
    );
    assert!(!running.to_string().contains("dependency_digest"));
    let proxy = fixture.ok(&[
        "proxy",
        "create",
        "--name",
        "unused",
        "--protocol",
        "http",
        "--host",
        "127.0.0.1",
        "--port",
        "1",
        "--json",
    ]);
    fixture.ok(&[
        "instance",
        "bind",
        &instance,
        "--proxy",
        proxy["receipt"]["entity_id"].as_str().unwrap(),
        "--json",
    ]);
    let changed = fixture.ok(&["instance", "inspect", &instance, "--json"]);
    assert_eq!(
        changed["runtime"]["observation"]["network"]["mode"],
        "direct"
    );
    assert_eq!(
        changed["runtime"]["observation"]["configuration_changed"],
        true
    );
    assert_eq!(changed["instance"]["network"]["kind"], "profile");
    assert_eq!(changed["protection"]["phase"], "disabled");
    assert!(process::is_running_exact(&child).unwrap());
    fixture.events(1);
    process::terminate_exact(&child).unwrap();
    assert_eq!(
        fixture.ok(&["instance", "inspect", &instance, "--json"])["runtime"]["observation"]["state"],
        "absent"
    );
}

#[test]
fn definite_create_failure_and_missing_dependency_never_run_a_direct_fallback() {
    for proxy in [false, true] {
        let fixture = Fixture::new(proxy, proxy);
        let instance = fixture.instance.to_string();
        let id = Uuid::new_v4().to_string();
        let output = fixture.cli(&["launch", &instance, "--request-id", &id, "--json"]);
        let result: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["attempt"]["phase"]["phase"], "failed");
        if proxy && result["attempt"]["phase"]["code"] == "CORE_BINARY_MISSING" {
            assert_eq!(output.status.code(), Some(5));
            assert_eq!(result["requires_action"]["action"], "install_sing_box");
        } else {
            assert_eq!(output.status.code(), Some(3));
            if !proxy {
                assert_eq!(
                    result["attempt"]["phase"]["code"],
                    "APPLICATION_NOT_CREATED"
                );
            }
        }
        let replay = fixture.cli(&["launch", &instance, "--request-id", &id, "--json"]);
        let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
        assert_eq!(replay["attempt"], result["attempt"]);
        assert!(replay.get("requires_action").is_none());
        assert_eq!(fs::read_dir(&fixture.events).unwrap().count(), 0);
        let journal: Value =
            serde_json::from_slice(&fs::read(fixture.root.join("state/launch.json")).unwrap())
                .unwrap();
        assert_eq!(journal["attempts"].as_array().unwrap().len(), 1);
    }
}

#[test]
#[ignore = "child fixture launched only by the CLI contract tests"]
fn launch_child() {
    let root = std::env::var_os("APP_PROXY_CLI_EVENTS").unwrap();
    fs::write(Path::new(&root).join(format!("{}.json", std::process::id())), serde_json::to_vec(&serde_json::json!({
        "proxy": std::env::var("HTTP_PROXY").ok(), "value": std::env::var("APP_PROXY_CLI_PRIVATE").ok(),
    })).unwrap()).unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
