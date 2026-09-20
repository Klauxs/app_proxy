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
        let mut fixture = Self {
            root,
            exe,
            events,
            instance,
            owner: None,
            _temp: temp,
        };
        fixture.capture_owner();
        fixture
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
