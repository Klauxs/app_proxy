#![cfg(windows)]
use app_proxy_app::{configuration::CatalogPage, coordinator::Status};
use app_proxy_core::{ProcessIdentity, model::*};
use app_proxy_windows::{identity, process, store::Store};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use uuid::Uuid;

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> Value {
    let output = cli(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
struct Owner(Option<ProcessIdentity>);
impl Owner {
    fn capture(root: &Path) -> Self {
        let status: Status = serde_json::from_value(ok(root, &["status", "--json"])).unwrap();
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
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(identity) = self.0.take() {
            let _ = process::terminate_exact(&identity);
        }
    }
}
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("配置 store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only, never executed").unwrap();
    (temp, root, exe)
}

#[test]
fn advanced_cli_edits_uninstalled_application_with_private_input_and_revision_check() {
    let (_temp, root, exe) = setup();
    let created = create(&root, &exe, &[]);
    let id = created["receipt"]["entity_id"].as_str().unwrap();
    let revision = created["receipt"]["revision"].as_u64().unwrap();
    let mut owner = Owner::capture(&root);
    fs::remove_file(exe).unwrap();
    let input = root.join("state/edit-input.json");
    let bytes = serde_json::to_vec(&serde_json::json!({
        "args": ["private-argument", ""],
        "cwd": {"kind":"application"},
        "env": {"set":[{"name":"PRIVATE_TOKEN","value":"literal-${app_home}-value"},
                         {"name":"EMPTY_VALUE","value":""}],
                "unset":["OLD_TOKEN"], "inherit":["PREVIOUS_OVERRIDE"]}
    }))
    .unwrap();
    fs::write(&input, &bytes).unwrap();
    let arguments = [
        "instance",
        "edit",
        id,
        "--file",
        input.to_str().unwrap(),
        "--revision",
        &revision.to_string(),
        "--json",
    ];
    let result = ok(&root, &arguments);
    assert_eq!(result["takes_effect"], "next_launch");
    assert_eq!(result["application_restarted"], false);
    assert_eq!(result["receipt"]["revision"], revision + 1);
    let view = ok(&root, &["instance", "settings", id, "--json"]);
    assert_eq!(view["argument_count"], 2);
    assert_eq!(view["set_count"], 2);
    assert_eq!(view["unset_count"], 1);
    let summary = serde_json::to_string(&view).unwrap();
    assert!(!summary.contains("private-argument"));
    assert!(!summary.contains("literal-${app_home}-value"));
    let stale = cli(&root, &arguments);
    assert!(!stale.status.success());
    assert!(!String::from_utf8_lossy(&stale.stderr).contains("literal-${app_home}-value"));
    assert_eq!(fs::read(&input).unwrap(), bytes);
    owner.stop();
    let store = Store::open(&root).unwrap();
    let manifest = store.load().unwrap();
    assert_eq!(manifest.revision, revision + 1);
    assert_eq!(manifest.instances[0].args, ["private-argument", ""]);
    let EnvValue::SecretRef { id: secret } = manifest.instances[0].env.set["PRIVATE_TOKEN"] else {
        panic!()
    };
    assert_eq!(
        store.read_secret(secret).unwrap(),
        "literal-${app_home}-value"
    );
    assert_eq!(fs::read_dir(root.join("secrets")).unwrap().count(), 2);
    assert!(
        !fs::read_to_string(root.join("manifest.json"))
            .unwrap()
            .contains("literal-${app_home}-value")
    );
    assert!(store.launch_attempts().unwrap().is_empty());
}

#[test]
fn guard_cli_keeps_clone_only_scope_and_reports_authorization_until_components_exist() {
    let (_temp, root, exe) = setup();
    let mut owner = Owner::capture(&root);
    let proxy = ok(
        &root,
        &[
            "proxy",
            "create",
            "--name",
            "guard profile",
            "--protocol",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            "1",
            "--json",
        ],
    );
    let profile = proxy["receipt"]["entity_id"].as_str().unwrap();
    let created = cli(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "codex",
            "--data",
            "isolated",
            "--proxy",
            profile,
            "--json",
        ],
    );
    assert_eq!(
        created.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let created: Value = serde_json::from_slice(&created.stdout).unwrap();
    let id = created["receipt"]["entity_id"].as_str().unwrap();
    assert_eq!(created["requires_action"], "authorize_guard_components");
    assert_eq!(created["protection"]["phase"], "needs_authorization");
    assert!(created["protection"].get("ifeo").is_none());
    assert_eq!(
        created["protection"]["scan"]["observation"]["state"],
        "absent"
    );
    let status = ok(&root, &["guard", "status", id, "--json"]);
    assert_eq!(status["status"]["desired"], "enabled");
    assert_eq!(status["status"]["listener"], "needs_authorization");
    assert!(status["request_id"].is_null());
    let disabled = ok(&root, &["guard", "disable", id, "--json"]);
    assert_eq!(disabled["status"]["phase"], "disabled");
    let disabled_again = ok(&root, &["guard", "disable", id, "--json"]);
    assert!(disabled_again["request_id"].is_null());
    assert_eq!(
        disabled_again["status"]["revision"],
        disabled["status"]["revision"]
    );
    let enabled = cli(&root, &["guard", "enable", id, "--json"]);
    assert_eq!(enabled.status.code(), Some(5));
    let enabled: Value = serde_json::from_slice(&enabled.stdout).unwrap();
    let request = enabled["request_id"].as_str().unwrap();
    let receipt = ok(&root, &["instance", "request", request, "--json"]);
    assert_eq!(receipt["result"]["outcome"]["receipt"]["entity_id"], id);
    let login = ok(&root, &["guard", "login", "status", "--json"]);
    assert_eq!(login["ready"], false);
    assert!(login["integration"].is_null());
    let missing = Uuid::new_v4().to_string();
    assert!(ok(&root, &["guard", "login", "request", &missing, "--json"])["status"].is_null());
    let resumed = cli(&root, &["guard", "login", "resume", &missing, "--json"]);
    assert_eq!(resumed.status.code(), Some(6));
    let resumed: Value = serde_json::from_slice(&resumed.stdout).unwrap();
    assert_eq!(resumed["request_id"], missing);
    assert!(resumed["status"].is_null());
    owner.stop();
    let mut store = Store::open(&root).unwrap();
    let mut manifest = store.load().unwrap();
    assert_eq!(manifest.instances.len(), 1);
    assert!(matches!(
        manifest.instances[0].data,
        InstanceData::Isolated { .. }
    ));
    assert!(manifest.integrations.guard_login_task.is_none());
    assert!(store.launch_attempts().unwrap().is_empty());
    assert!(!root.join("instances").exists());
    // A login entry with unavailable ownership is an independent diagnostic;
    // it cannot hide a missing listener or prevent disabling current Guard.
    manifest.integrations.guard_login_task = Some(app_proxy_core::model::LoginTask {
        name: "unverified-fixture-login".into(),
        target: root.join("missing/app-proxy-host.exe"),
        args: vec![],
    });
    store.commit(manifest.revision, manifest).unwrap();
    drop(store);
    owner = Owner::capture(&root);
    let status = ok(&root, &["guard", "status", id, "--json"]);
    assert_eq!(status["status"]["listener"], "needs_authorization");
    assert_eq!(status["login"]["ready"], false);
    assert_eq!(
        status["login"]["diagnostic"],
        "GUARD_LOGIN_OWNERSHIP_UNAVAILABLE"
    );
    let disabled = ok(&root, &["guard", "disable", id, "--json"]);
    assert_eq!(disabled["status"]["phase"], "disabled");
    assert_eq!(disabled["login"]["ready"], false);
    owner.stop();
    assert!(
        Store::open(&root)
            .unwrap()
            .load()
            .unwrap()
            .integrations
            .guard_login_task
            .is_some()
    );
}

#[test]
fn guard_cli_original_requires_listener_and_can_disable_without_ifeo() {
    let (_temp, root, exe) = setup();
    let created = create(&root, &exe, &[]);
    let id = created["receipt"]["entity_id"].as_str().unwrap();
    let mut owner = Owner::capture(&root);
    let enable_direct = cli(&root, &["guard", "enable", id, "--json"]);
    assert_eq!(enable_direct.status.code(), Some(2));
    let proxy = ok(
        &root,
        &[
            "proxy",
            "create",
            "--name",
            "guard",
            "--protocol",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            "1",
            "--json",
        ],
    );
    let profile = proxy["receipt"]["entity_id"].as_str().unwrap();
    ok(
        &root,
        &["instance", "bind", id, "--proxy", profile, "--json"],
    );
    let enabled = cli(&root, &["guard", "enable", id, "--json"]);
    assert_eq!(enabled.status.code(), Some(5));
    let enabled: Value = serde_json::from_slice(&enabled.stdout).unwrap();
    assert!(enabled["status"].get("ifeo").is_none());
    assert_eq!(enabled["status"]["listener"], "needs_authorization");
    assert_eq!(enabled["status"]["phase"], "needs_authorization");
    let disabled = ok(&root, &["guard", "disable", id, "--json"]);
    assert_eq!(disabled["status"]["phase"], "disabled");
    assert!(disabled["status"].get("ifeo").is_none());
    owner.stop();
}

#[test]
fn proxy_cli_edits_credentials_keeps_endpoint_and_redacts_all_output() {
    use std::{io::Write, process::Stdio};
    let (_temp, root, _) = setup();
    let mut owner = Owner::capture(&root);
    let mut child = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args([
            "proxy",
            "create",
            "--name",
            "认证代理",
            "--protocol",
            "http",
            "--host",
            "proxy.example",
            "--port",
            "8080",
            "--username",
            "private-user",
            "--password-stdin",
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(b"private-cli-password\r\n")
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-"));
    let created: Value = serde_json::from_slice(&output.stdout).unwrap();
    let id = created["receipt"]["entity_id"].as_str().unwrap();
    let request = created["request_id"].as_str().unwrap();
    let initial = ok(&root, &["proxy", "show", id, "--json"]);
    assert_eq!(initial["profiles"][0]["authenticated"], true);
    assert!(!initial.to_string().contains("private-"));
    let endpoint = initial["profiles"][0]["endpoint"].clone();
    ok(&root, &["proxy", "rename", id, "改名", "--json"]);
    let rejected = cli(
        &root,
        &[
            "proxy",
            "update",
            id,
            "--protocol",
            "socks5",
            "--host",
            "changed.example",
            "--port",
            "1080",
            "--json",
        ],
    );
    assert_eq!(rejected.status.code(), Some(2));
    assert_eq!(
        ok(&root, &["proxy", "show", id, "--json"])["profiles"][0]["authenticated"],
        true
    );
    ok(
        &root,
        &[
            "proxy",
            "update",
            id,
            "--protocol",
            "socks5",
            "--host",
            "changed.example",
            "--port",
            "1080",
            "--no-auth",
            "--json",
        ],
    );
    let updated = ok(&root, &["proxy", "show", id, "--json"]);
    assert_eq!(updated["profiles"][0]["endpoint"], endpoint);
    assert_eq!(updated["profiles"][0]["revision"], 3);
    assert_eq!(updated["profiles"][0]["name"], "改名");
    assert_eq!(updated["profiles"][0]["authenticated"], false);
    assert_eq!(updated["profiles"][0]["protocol"], "socks5");
    assert!(
        !ok(&root, &["instance", "list", "--json"])
            .to_string()
            .contains("private-")
    );
    assert_eq!(
        ok(&root, &["proxy", "request", request, "--json"])["result"]["outcome"]["receipt"],
        created["receipt"]
    );
    owner.stop();
    let store = Store::open(&root).unwrap();
    assert_eq!(
        store
            .read_secret(Uuid::parse_str(request).unwrap())
            .unwrap(),
        "private-cli-password"
    );
    for entry in fs::read_dir(root.join("state/requests")).unwrap() {
        assert!(
            !fs::read_to_string(entry.unwrap().path())
                .unwrap()
                .contains("private-cli-password")
        );
    }
    drop(store);
    ok(&root, &["proxy", "remove", id, "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(
        ok(&root, &["proxy", "list", "--json"])["profiles"],
        serde_json::json!([])
    );
    assert!(root.join(format!("secrets/{request}.json")).exists());
    owner.stop();
}

#[test]
fn proxy_cli_assigns_distinct_ports_and_rejects_removal_while_bound() {
    let (_temp, root, exe) = setup();
    let mut owner = Owner::capture(&root);
    let first = ok(
        &root,
        &[
            "proxy",
            "create",
            "--name",
            "one",
            "--protocol",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            "8080",
            "--json",
        ],
    );
    let second = ok(
        &root,
        &[
            "proxy",
            "create",
            "--name",
            "two",
            "--protocol",
            "socks5",
            "--host",
            "127.0.0.1",
            "--port",
            "1080",
            "--json",
        ],
    );
    let id = first["receipt"]["entity_id"].as_str().unwrap();
    let profiles = ok(&root, &["proxy", "list", "--json"]);
    assert_ne!(
        profiles["profiles"][0]["endpoint"]["port"],
        profiles["profiles"][1]["endpoint"]["port"]
    );
    assert_ne!(
        first["receipt"]["entity_id"],
        second["receipt"]["entity_id"]
    );
    let instance = ok(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "environment",
            "--proxy",
            id,
            "--json",
        ],
    );
    let rejected = cli(&root, &["proxy", "remove", id, "--json"]);
    assert_eq!(rejected.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("PROFILE_IN_USE"));
    ok(
        &root,
        &[
            "instance",
            "bind",
            instance["receipt"]["entity_id"].as_str().unwrap(),
            "--direct",
            "--json",
        ],
    );
    ok(&root, &["proxy", "remove", id, "--json"]);
    owner.stop();
}

#[test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; CLI confirmation with an isolated real core"]
fn proxy_cli_previews_exact_impact_rejects_stale_confirmation_and_reports_restore_failure() {
    preview_case(false, false);
    preview_case(true, false);
}

#[test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; isolated real core recovery and removal"]
fn proxy_cli_recovers_start_then_previews_and_removes_last_active_profile() {
    preview_case(false, true);
}

#[test]
#[ignore = "subprocess fixture for the interrupted real-core CLI test"]
fn cli_start_crash_fixture() {
    use app_proxy_windows::{core_process::CoreProcess, core_state::CoreState, singbox_binary};
    let root = PathBuf::from(std::env::var_os("APP_PROXY_CLI_START_ROOT").unwrap());
    let id = std::env::var("APP_PROXY_CLI_START_GENERATION")
        .unwrap()
        .parse()
        .unwrap();
    let mut store = Store::open(&root).unwrap();
    let generation = store.open_core_generation(id).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let binary = runtime
        .block_on(singbox_binary::discover(&root))
        .unwrap()
        .unwrap();
    let prepared = CoreProcess::prepare(&mut store, &binary, &generation).unwrap();
    store
        .transition_core_state(
            &CoreState::Stopped {},
            CoreState::Starting { generation: id },
        )
        .unwrap();
    drop(prepared.spawn(&binary, &generation).unwrap());
    std::process::exit(0);
}

fn preview_case(expanding: bool, removing: bool) {
    use app_proxy_windows::{core_process::CoreProcess, core_state::CoreState, singbox_binary};
    struct OwnedCore(CoreProcess);
    impl Drop for OwnedCore {
        fn drop(&mut self) {
            let _ = self.0.stop();
        }
    }
    let (_temp, root, _) = setup();
    let mut store = Store::create(&root).unwrap();
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    let binary =
        PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").expect("validation binary"));
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: socket.local_addr().unwrap().port(),
    };
    let profile = Uuid::new_v4();
    let node = Uuid::new_v4();
    let unavailable = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let upstream_port = unavailable.local_addr().unwrap().port();
    drop(unavailable);
    let mut manifest = store.load().unwrap();
    manifest.profiles.push(ProxyProfile {
        id: profile,
        name: "CLI fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: endpoint.clone(),
        selected_node_id: node,
        source: ProxySource::Manual {
            nodes: vec![ManualNode {
                id: node,
                name: "fixture".into(),
                protocol: ManualProtocol::Http,
                host: "127.0.0.1".into(),
                port: upstream_port,
                credentials: None,
            }],
        },
    });
    manifest.settings.test_url = "https://fixture.invalid/health".into();
    store.commit(manifest.revision, manifest).unwrap();
    let generation = store.prepare_core_generation(&[profile]).unwrap();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let binary = runtime
        .block_on(singbox_binary::discover(&root))
        .unwrap()
        .unwrap();
    runtime
        .block_on(binary.check_config(generation.config_path()))
        .unwrap();
    let starting = CoreState::Starting {
        generation: generation.id(),
    };
    drop(socket);
    let core = if removing {
        drop(store);
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli_start_crash_fixture",
                "--ignored",
                "--test-threads=1",
            ])
            .env("APP_PROXY_CLI_START_ROOT", &root)
            .env(
                "APP_PROXY_CLI_START_GENERATION",
                generation.id().to_string(),
            )
            .output()
            .unwrap();
        assert!(
            status.status.success(),
            "{}",
            String::from_utf8_lossy(&status.stderr)
        );
        store = Store::open(&root).unwrap();
        OwnedCore(store.inspect_core_start(generation.id()).unwrap().unwrap())
    } else {
        let prepared = CoreProcess::prepare(&mut store, &binary, &generation).unwrap();
        store
            .transition_core_state(&CoreState::Stopped {}, starting.clone())
            .unwrap();
        OwnedCore(prepared.spawn(&binary, &generation).unwrap())
    };
    if !removing {
        store
            .transition_core_state(
                &starting,
                CoreState::Running {
                    generation: generation.id(),
                    process: core.0.identity().clone(),
                },
            )
            .unwrap();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !core
        .0
        .listeners_verified(std::slice::from_ref(&endpoint))
        .unwrap()
    {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(store);
    let mut owner = Owner::capture(&root);
    if removing {
        assert_eq!(
            ok(&root, &["core", "status", "--json"])["observed"],
            "indeterminate"
        );
        let recovered = ok(&root, &["core", "recover-start", "--json"]);
        assert_eq!(recovered["result"]["outcome"]["outcome"], "reconciled");
        assert_eq!(
            recovered["result"]["outcome"]["process"]["pid"],
            core.0.identity().pid
        );
    }
    let id = profile.to_string();
    let port = upstream_port.to_string();
    let added = if expanding {
        Some(
            ok(
                &root,
                &[
                    "proxy",
                    "create",
                    "--name",
                    "added",
                    "--protocol",
                    "http",
                    "--host",
                    "127.0.0.1",
                    "--port",
                    &port,
                    "--json",
                ],
            )["receipt"]["entity_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        )
    } else {
        None
    };
    let args = if removing {
        vec!["proxy", "remove", &id, "--json"]
    } else if let Some(added) = &added {
        vec!["core", "start", added.as_str(), "--json"]
    } else {
        vec![
            "proxy",
            "update",
            &id,
            "--protocol",
            "socks5",
            "--host",
            "127.0.0.1",
            "--port",
            &port,
            "--no-auth",
            "--json",
        ]
    };
    if let Some(added) = &added {
        let created = ok(
            &root,
            &[
                "instance",
                "create",
                "--exe",
                _temp.path().join("fixture.exe").to_str().unwrap(),
                "--adapter",
                "environment",
                "--proxy",
                added,
                "--json",
            ],
        );
        let instance = created["receipt"]["entity_id"].as_str().unwrap();
        let preview = cli(&root, &["launch", instance, "--json"]);
        assert_eq!(
            preview.status.code(),
            Some(5),
            "{}",
            String::from_utf8_lossy(&preview.stderr)
        );
        let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
        assert_eq!(
            preview["attempt"]["phase"]["code"],
            "CORE_RECONFIGURE_REQUIRES_CONFIRMATION"
        );
        assert!(preview["attempt"]["dispatch_id"].is_null());
        assert_eq!(preview["requires_action"]["action"], "confirm_core_update");
        let impact = &preview["requires_action"]["impact"];
        assert_eq!(impact["affected_profiles"], serde_json::json!([profile]));
        assert_eq!(impact["added_profiles"], serde_json::json!([added]));
        assert!(core.0.is_running().unwrap());
        let replay = cli(
            &root,
            &[
                "launch",
                instance,
                "--request-id",
                preview["request_id"].as_str().unwrap(),
                "--json",
            ],
        );
        assert_eq!(replay.status.code(), Some(3));
        assert_eq!(
            ok(&root, &["core", "status", "--json"])["update"]["impact"]["plan_id"],
            impact["plan_id"]
        );
    }
    let preview = cli(&root, &args);
    assert_eq!(preview.status.code(), Some(5));
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    let impact = &preview["result"]["outcome"]["impact"];
    assert_eq!(impact["affected_profiles"], serde_json::json!([profile]));
    if let Some(added) = &added {
        assert_eq!(impact["added_profiles"], serde_json::json!([added]));
    }
    assert!(core.0.is_running().unwrap());
    assert_eq!(
        ok(&root, &["proxy", "show", &id, "--json"])["profiles"][0]["protocol"],
        "http"
    );
    ok(
        &root,
        &["proxy", "rename", &id, "renamed after preview", "--json"],
    );
    let rejected = cli(
        &root,
        &[
            "core",
            "apply-update",
            impact["plan_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(rejected.status.code(), Some(3));
    assert!(core.0.is_running().unwrap());
    let preview = cli(&root, &args);
    assert_eq!(preview.status.code(), Some(5));
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    let plan = preview["result"]["outcome"]["impact"]["plan_id"]
        .as_str()
        .unwrap();
    let applied = if expanding {
        let mut args = args.clone();
        args.push("--apply-to-running");
        cli(&root, &args)
    } else {
        cli(&root, &["core", "apply-update", plan, "--json"])
    };
    if removing {
        assert!(
            applied.status.success(),
            "{}",
            String::from_utf8_lossy(&applied.stderr)
        );
        let applied: Value = serde_json::from_slice(&applied.stdout).unwrap();
        assert_eq!(applied["result"]["outcome"]["outcome"], "profile_removed");
        assert_eq!(applied["result"]["outcome"]["profile_id"], id);
        assert!(!core.0.is_running().unwrap());
        assert_eq!(
            ok(&root, &["core", "status", "--json"])["observed"],
            "stopped"
        );
        assert!(
            ok(&root, &["proxy", "list", "--json"])["profiles"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let request_id = applied["request_id"].as_str().unwrap();
        owner.stop();
        assert_eq!(
            ok(&root, &["core", "request", request_id, "--json"]),
            applied
        );
        owner = Owner::capture(&root);
        owner.stop();
        return;
    }
    assert_eq!(
        applied.status.code(),
        Some(3),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    let applied: Value = serde_json::from_slice(&applied.stdout).unwrap();
    assert_eq!(
        applied["result"]["outcome"],
        serde_json::json!({"outcome":"restored","core_down":true})
    );
    assert!(!core.0.is_running().unwrap());
    let snapshot = ok(&root, &["core", "status", "--json"]);
    assert_eq!(snapshot["observed"], "down");
    assert_eq!(
        ok(&root, &["proxy", "show", &id, "--json"])["profiles"][0]["protocol"],
        "http"
    );
    // A recovery query of a terminal plan is idempotent and cannot respawn it.
    let plan = snapshot["update"]["impact"]["plan_id"].as_str().unwrap();
    let recovered = cli(&root, &["core", "recover-update", plan, "--json"]);
    assert_eq!(recovered.status.code(), Some(3));
    assert_eq!(
        serde_json::from_slice::<Value>(&recovered.stdout).unwrap()["result"]["outcome"],
        applied["result"]["outcome"]
    );
    owner.stop();
    let store = Store::open(&root).unwrap();
    assert!(!store.has_unresolved_core_requests().unwrap());
}

#[test]
fn core_cli_stop_receipt_survives_owner_restart_and_missing_profile_fails_safely() {
    let (_temp, root, _) = setup();
    let initial = ok(&root, &["core", "status", "--json"]);
    let mut owner = Owner::capture(&root);
    assert_eq!(initial["observed"], "stopped");
    let stopped = ok(&root, &["core", "stop", "--json"]);
    assert_eq!(stopped["result"]["outcome"]["outcome"], "stopped");
    let request_id = stopped["request_id"].as_str().unwrap();
    owner.stop();
    let replay = ok(&root, &["core", "request", request_id, "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(stopped, replay);
    let profile = Uuid::new_v4().to_string();
    let failed = cli(&root, &["core", "start", &profile, "--json"]);
    assert_eq!(failed.status.code(), Some(3));
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["result"]["outcome"]["outcome"], "failed");
    assert_eq!(
        ok(&root, &["core", "status", "--json"])["observed"],
        "stopped"
    );
    owner.stop();
}

#[test]
fn core_cli_interrupted_request_stays_unknown_across_restart_without_execution() {
    use app_proxy_core::core_control::CoreAction;
    let (_temp, root, _) = setup();
    let mut store = Store::create(&root).unwrap();
    let id = Uuid::new_v4();
    store
        .begin_core_request(id, Uuid::new_v4(), &CoreAction::Stop {})
        .unwrap();
    drop(store);
    let queried = cli(&root, &["core", "request", &id.to_string(), "--json"]);
    let mut owner = Owner::capture(&root);
    assert_eq!(queried.status.code(), Some(6));
    let first: Value = serde_json::from_slice(&queried.stdout).unwrap();
    assert_eq!(first["result"]["status"], "indeterminate");
    assert_eq!(
        ok(&root, &["core", "status", "--json"])["observed"],
        "stopped"
    );
    owner.stop();
    let queried = cli(&root, &["core", "request", &id.to_string(), "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(queried.status.code(), Some(6));
    assert_eq!(
        first,
        serde_json::from_slice::<Value>(&queried.stdout).unwrap()
    );
    owner.stop();
}

#[test]
#[ignore = "downloads the pinned official sing-box archive; isolated temporary store only"]
fn core_install_cli_downloads_directly_and_never_starts_proxy_or_application() {
    let (_temp, root, _) = setup();
    // The first CLI creates the host, so the downloader inherits this poisoned
    // environment too. Poisoning only a later client would prove nothing.
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args(["core", "install", "--json"])
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "")
        .output()
        .unwrap();
    let mut owner = Owner::capture(&root);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(installed["result"]["outcome"]["outcome"], "installed");
    assert_eq!(
        installed["result"]["outcome"]["version"],
        app_proxy_windows::singbox_install::VERSION
    );
    assert_eq!(
        ok(&root, &["core", "status", "--json"])["observed"],
        "stopped"
    );
    let request = installed["request_id"].as_str().unwrap();
    owner.stop();
    let queried = ok(&root, &["core", "request", request, "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(installed, queried);
    let reused = ok(&root, &["core", "install", "--json"]);
    assert_eq!(reused["result"]["outcome"]["outcome"], "installed");
    owner.stop();
}
fn create(root: &Path, exe: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "instance",
        "create",
        "--exe",
        exe.to_str().unwrap(),
        "--adapter",
        "codex",
        "--direct",
        "--json",
    ];
    args.extend_from_slice(extra);
    ok(root, &args)
}

#[test]
fn real_cli_lifecycle_keeps_identity_and_data_without_launching_an_application() {
    let (_temp, root, exe) = setup();
    let first = create(&root, &exe, &["--name", "原版"]);
    let mut owner = Owner::capture(&root);
    let original = first["receipt"]["entity_id"].as_str().unwrap();
    assert_eq!(first["application_started"], false);
    let request = first["request_id"].as_str().unwrap();
    let queried = ok(&root, &["instance", "request", request, "--json"]);
    assert_eq!(
        queried["result"]["outcome"]["receipt"]["entity_id"],
        original
    );
    let cloned = ok(
        &root,
        &["instance", "clone", original, "--name", "分身", "--json"],
    );
    let clone = cloned["receipt"]["entity_id"].as_str().unwrap();
    assert_ne!(original, clone);
    let listed: CatalogPage =
        serde_json::from_value(ok(&root, &["instance", "list", "--json"])).unwrap();
    assert_eq!(listed.applications.len(), 1);
    assert_eq!(listed.instances.len(), 2);
    assert!(
        !listed
            .instances
            .iter()
            .find(|i| i.id.to_string() == original)
            .unwrap()
            .isolated
    );
    assert!(
        listed
            .instances
            .iter()
            .find(|i| i.id.to_string() == clone)
            .unwrap()
            .isolated
    );
    // Populate only our fixture's owned instance directory between owner runs.
    owner.stop();
    let store = Store::open(&root).unwrap();
    let data = store
        .prepare_instance_data(Uuid::parse_str(clone).unwrap(), None)
        .unwrap()
        .unwrap();
    let saved_file = data.paths.app_home.join("keep.txt");
    fs::write(&saved_file, "retained fixture data").unwrap();
    drop(data);
    drop(store);
    let renamed = ok(&root, &["instance", "rename", clone, "新名称", "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(renamed["receipt"]["entity_id"], clone);
    ok(&root, &["instance", "bind", clone, "--direct", "--json"]);
    let after: CatalogPage =
        serde_json::from_value(ok(&root, &["instance", "list", "--json"])).unwrap();
    assert_eq!(
        after
            .instances
            .iter()
            .find(|i| i.id.to_string() == clone)
            .unwrap()
            .name,
        "新名称"
    );
    ok(&root, &["instance", "remove", clone, "--json"]);
    ok(&root, &["instance", "remove", original, "--json"]);
    owner.stop();
    let store = Store::open(&root).unwrap();
    let manifest = store.load().unwrap();
    assert!(manifest.instances.is_empty());
    assert_eq!(manifest.applications.len(), 1);
    assert_eq!(
        fs::read_to_string(saved_file).unwrap(),
        "retained fixture data"
    );
    assert_eq!(fs::read(exe).unwrap(), b"fixture only, never executed");
}

#[test]
fn clone_only_creation_never_registers_original_and_alias_does_not_duplicate_application() {
    let (temp, root, exe) = setup();
    create(&root, &exe, &["--data", "isolated"]);
    let mut owner = Owner::capture(&root);
    let alias = temp.path().join("alias.exe");
    fs::hard_link(&exe, &alias).unwrap();
    create(&root, &alias, &["--data", "isolated", "--name", "second"]);
    owner.stop();
    let store = Store::open(&root).unwrap();
    let manifest = store.load().unwrap();
    assert_eq!(manifest.instances.len(), 2);
    assert_eq!(manifest.applications.len(), 1);
    assert!(
        manifest
            .instances
            .iter()
            .all(|i| matches!(i.data, InstanceData::Isolated { .. }))
    );
    assert!(!root.join("instances").exists()); // Data allocation remains deferred to launch.
}

#[test]
fn duplicate_original_is_rejected_and_cli_requires_explicit_network_choice() {
    let (temp, root, exe) = setup();
    let missing = cli(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "codex",
        ],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(!root.exists());
    create(&root, &exe, &[]);
    let mut owner = Owner::capture(&root);
    let alias = temp.path().join("alias.exe");
    fs::hard_link(exe, &alias).unwrap();
    let duplicate = cli(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            alias.to_str().unwrap(),
            "--adapter",
            "codex",
            "--direct",
            "--json",
        ],
    );
    assert_eq!(duplicate.status.code(), Some(4));
    let rejected: Value = serde_json::from_slice(&duplicate.stdout).unwrap();
    assert_eq!(rejected["outcome"]["code"], "DUPLICATE_ORIGINAL");
    let lookup = cli(
        &root,
        &[
            "instance",
            "request",
            rejected["request_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(lookup.status.code(), Some(4));
    owner.stop();
    let manifest = Store::open(&root).unwrap().load().unwrap();
    assert_eq!(manifest.applications.len(), 1);
    assert_eq!(manifest.instances.len(), 1);
}

#[test]
fn list_pages_return_every_instance_without_arguments_or_environment_values() {
    let (_temp, root, exe) = setup();
    let mut store = Store::create(&root).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe { path: exe },
        template_ref: Template::Codex,
    });
    for n in 0..37 {
        let id = Uuid::new_v4();
        let mut env = SavedEnvironment::default();
        env.set.insert(
            "PRIVATE_TOKEN".into(),
            EnvValue::Literal {
                value: "never-display-this-secret".into(),
            },
        );
        manifest.instances.push(Instance {
            id,
            application_id: app,
            name: format!("fixture {n}"),
            revision: 1,
            data: InstanceData::Isolated {
                location: StorageLocation::Store {
                    relative_path: PathBuf::from("instances").join(id.to_string()),
                },
            },
            args: vec!["--private-argument=hidden-argument-value".into()],
            env,
            cwd: WorkingDirectory::Application {},
            network: NetworkBinding::Direct {},
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
    }
    store.commit(1, manifest).unwrap();
    drop(store);
    let output = cli(&root, &["instance", "list", "--json"]);
    let mut owner = Owner::capture(&root);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for forbidden in [
        "PRIVATE_TOKEN",
        "never-display-this-secret",
        "hidden-argument-value",
    ] {
        assert!(!text.contains(forbidden));
    }
    let catalog: CatalogPage = serde_json::from_str(&text).unwrap();
    assert_eq!(catalog.revision, 2);
    assert_eq!(catalog.instances.len(), 37);
    assert_eq!(catalog.applications.len(), 1);
    let ids: std::collections::HashSet<_> = catalog.instances.iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), 37);
    owner.stop();
}
