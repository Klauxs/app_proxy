#![cfg(windows)]
use app_proxy_app::coordinator::Status;
use app_proxy_core::ProcessIdentity;
use app_proxy_windows::{identity, process, store::Store};
use serde_json::Value;
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

fn cli(root: &Path, args: &[&str], input: Option<&str>) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(root)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    if let Some(input) = input {
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
    }
    drop(child.stdin.take());
    child.wait_with_output().unwrap()
}
fn ok(root: &Path, args: &[&str], input: Option<&str>) -> Value {
    let output = cli(root, args, input);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!text.contains("private-node-password"));
    assert!(!text.contains("private-source-token"));
    serde_json::from_slice(&output.stdout).unwrap()
}
struct Owner(Option<ProcessIdentity>);
impl Owner {
    fn capture(root: &Path) -> Self {
        let status: Status = serde_json::from_value(ok(root, &["status", "--json"], None)).unwrap();
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
struct Http {
    url: String,
    body: Arc<Mutex<String>>,
    requests: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Http {
    fn new(body: String) -> Self {
        Self::with_stall(body, false)
    }
    fn with_stall(body: String, stalled: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!(
            "http://{}/list?token=private-source-token",
            listener.local_addr().unwrap()
        );
        let body = Arc::new(Mutex::new(body));
        let requests = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (payload, seen, stopped) = (body.clone(), requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            let mut children = Vec::new();
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                let (payload, seen, stopped) = (payload.clone(), seen.clone(), stopped.clone());
                children.push(std::thread::spawn(move || {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") {
                        let mut byte = [0];
                        if stream.read_exact(&mut byte).is_err() {
                            break;
                        }
                        request.push(byte[0]);
                    }
                    if request.starts_with(b"CONNECT ") {
                        if stream
                            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                            .is_err()
                        {
                            return;
                        }
                        request.clear();
                        while !request.ends_with(b"\r\n\r\n") {
                            let mut byte = [0];
                            if stream.read_exact(&mut byte).is_err() {
                                return;
                            }
                            request.push(byte[0]);
                        }
                    }
                    seen.fetch_add(1, Ordering::SeqCst);
                    if stalled {
                        while !stopped.load(Ordering::SeqCst) {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        return;
                    }
                    let body = payload.lock().unwrap().clone();
                    let _ = stream.write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                );
                }));
            }
            for child in children {
                child.join().unwrap();
            }
        });
        Self {
            url,
            body,
            requests,
            stop,
            thread: Some(thread),
        }
    }
}
impl Drop for Http {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}
fn body(count: usize) -> String {
    (0..count)
        .map(|n| format!("trojan://private-node-password@edge.invalid:443#Node{n}\n"))
        .collect()
}
fn setup() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("subscription store");
    (temp, root)
}

#[test]
fn real_cli_import_pages_select_refresh_and_preserve_old_profile_on_bad_refresh() {
    let (_temp, root) = setup();
    let server = Http::new(body(70));
    let mut owner = Owner::capture(&root);
    let imported = ok(
        &root,
        &[
            "proxy",
            "import",
            "--name",
            "测试订阅",
            "--node",
            "Node0",
            "--url-stdin",
            "--json",
        ],
        Some(&server.url),
    );
    let profile = imported["receipt"]["entity_id"].as_str().unwrap();
    assert_eq!(imported["changes"]["added"].as_array().unwrap().len(), 70);
    let nodes = ok(&root, &["proxy", "nodes", profile, "--json"], None);
    assert_eq!(nodes["nodes"].as_array().unwrap().len(), 70);
    let node = nodes["nodes"][69]["id"].as_str().unwrap();
    assert!(!serde_json::to_string(&nodes).unwrap().contains("secret"));
    ok(&root, &["proxy", "select", profile, node, "--json"], None);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    *server.body.lock().unwrap() = body(71);
    let refreshed = ok(&root, &["proxy", "refresh", profile, "--json"], None);
    assert_eq!(refreshed["changes"]["added"], serde_json::json!(["Node70"]));
    let after = ok(&root, &["proxy", "nodes", profile, "--json"], None);
    assert_eq!(after["selected_node_id"], node);
    assert_eq!(after["source_revision"], 2);
    *server.body.lock().unwrap() = body(1);
    let failed = cli(&root, &["proxy", "refresh", profile, "--json"], None);
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("SUBSCRIPTION_SELECTED_NODE_REMOVED"));
    assert_eq!(
        ok(&root, &["proxy", "nodes", profile, "--json"], None),
        after
    );
    let receipt = ok(
        &root,
        &[
            "proxy",
            "request",
            imported["request_id"].as_str().unwrap(),
            "--json",
        ],
        None,
    );
    assert_eq!(
        receipt["result"]["outcome"]["receipt"]["entity_id"],
        profile
    );
    owner.stop();
    let store = Store::open(&root).unwrap();
    assert_eq!(store.load().unwrap().profiles.len(), 1);
    assert!(store.launch_attempts().unwrap().is_empty());
    assert!(
        !fs::read_to_string(root.join("manifest.json"))
            .unwrap()
            .contains("private-")
    );
}

#[test]
fn missing_selection_and_invalid_urls_do_not_download_or_create_profiles() {
    let (_temp, root) = setup();
    let server = Http::new(body(2));
    let mut owner = Owner::capture(&root);
    let missing = cli(
        &root,
        &["proxy", "import", "--name", "test", "--url-stdin", "--json"],
        Some(&server.url),
    );
    assert_eq!(missing.status.code(), Some(2));
    for input in [
        "https://user:private-password@invalid/",
        "",
        "https://invalid/#private-source-token",
    ] {
        let output = cli(
            &root,
            &[
                "proxy",
                "import",
                "--name",
                "test",
                "--node",
                "Node0",
                "--url-stdin",
                "--json",
            ],
            Some(input),
        );
        assert_eq!(output.status.code(), Some(2));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-"));
    }
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    owner.stop();
    assert!(
        Store::open(&root)
            .unwrap()
            .load()
            .unwrap()
            .profiles
            .is_empty()
    );
}

#[test]
fn preview_capacity_rejection_is_reported_immediately_without_a_new_download() {
    let (_temp, root) = setup();
    let server = Http::with_stall(body(2), true);
    let mut owner = Owner::capture(&root);
    struct Clients(Vec<std::process::Child>);
    impl Drop for Clients {
        fn drop(&mut self) {
            for child in &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
    let mut clients = Clients(Vec::new());
    for _ in 0..4 {
        let mut child = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
            .arg("--home")
            .arg(&root)
            .args([
                "proxy",
                "import",
                "--name",
                "pending",
                "--node",
                "Node0",
                "--url-stdin",
                "--json",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(server.url.as_bytes())
            .unwrap();
        drop(child.stdin.take());
        clients.0.push(child);
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while server.requests.load(Ordering::SeqCst) < 4 {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let start = std::time::Instant::now();
    let output = cli(
        &root,
        &[
            "proxy",
            "import",
            "--name",
            "overflow",
            "--node",
            "Node0",
            "--url-stdin",
            "--json",
        ],
        Some(&server.url),
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("SUBSCRIPTION_PREVIEW_LIMIT"));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert_eq!(server.requests.load(Ordering::SeqCst), 4);
    drop(clients);
    owner.stop();
}

#[test]
#[ignore = "interactive console validation: enter the printed fixture URL, then Ctrl+Z and Enter at node choice"]
fn console_hidden_url_and_node_eof_preserve_configuration() {
    let (_temp, root) = setup();
    let server = Http::new(body(2));
    let mut owner = Owner::capture(&root);
    // The path/token here is synthetic. The operator checks that typing it at
    // the following prompt does not echo and that normal choice input does.
    eprintln!("Synthetic URL to enter: {}", server.url);
    let status = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args(["proxy", "import", "--name", "console fixture"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(5));
    owner.stop();
    assert!(
        Store::open(&root)
            .unwrap()
            .load()
            .unwrap()
            .profiles
            .is_empty()
    );
}

#[test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; CLI with one owned core and local synthetic HTTP upstream"]
fn real_cli_uses_owned_download_route_and_previews_only_connection_changes() {
    use app_proxy_core::{model::*, registry::*, subscription};
    use app_proxy_windows::{
        core_process::CoreProcess, core_state::CoreState, singbox_binary,
        subscription_stage::ImportRequest,
    };
    use uuid::Uuid;
    struct Owned(CoreProcess);
    impl Drop for Owned {
        fn drop(&mut self) {
            let _ = self.0.stop();
        }
    }
    let (_temp, root) = setup();
    let server = Http::new(body(2));
    let mut store = Store::create(&root).unwrap();
    let binary = PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let reserves: Vec<_> = (0..2)
        .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let endpoint = |index: usize| Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: reserves[index].local_addr().unwrap().port(),
    };
    let profile = Uuid::new_v4();
    let staged = store
        .stage_subscription_import(
            &ImportRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 1,
                profile_id: profile,
                name: "active subscription".into(),
                endpoint: endpoint(0),
                url: server.url.clone(),
                selected_name: "Node0".into(),
            },
            &subscription::parse(&body(2)).unwrap(),
        )
        .unwrap();
    store.apply_config(&staged.request).unwrap();
    let route = Uuid::new_v4();
    store
        .apply_config(&ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::CreateManualProfile {
                profile_id: route,
                name: "download route".into(),
                endpoint: endpoint(1),
                node: ManualProxyInput {
                    protocol: ManualProtocol::Http,
                    host: "127.0.0.1".into(),
                    port: reqwest::Url::parse(&server.url).unwrap().port().unwrap(),
                    credentials: None,
                },
            },
        })
        .unwrap();
    let generation = store.prepare_core_generation(&[profile, route]).unwrap();
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
    store
        .transition_core_state(&CoreState::Stopped {}, starting.clone())
        .unwrap();
    let endpoints = vec![endpoint(0), endpoint(1)];
    drop(reserves);
    let owned = Owned(CoreProcess::spawn(&binary, &generation).unwrap());
    store
        .transition_core_state(
            &starting,
            CoreState::Running {
                generation: generation.id(),
                process: owned.0.identity().clone(),
            },
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !owned.0.listeners_verified(&endpoints).unwrap() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    drop(store);
    let mut owner = Owner::capture(&root);
    // This .invalid address cannot work directly; it is answered by our route.
    ok(
        &root,
        &[
            "proxy",
            "import",
            "--name",
            "via owned",
            "--node",
            "Node0",
            "--url-stdin",
            "--via",
            &route.to_string(),
            "--json",
        ],
        Some("http://subscription.invalid/list?token=private-source-token"),
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    let profile = profile.to_string();
    let nodes = ok(&root, &["proxy", "nodes", &profile, "--json"], None);
    let second = nodes["nodes"][1]["id"].as_str().unwrap();
    ok(
        &root,
        &["proxy", "select", &profile, second, "--json"],
        None,
    );
    assert!(owned.0.is_running().unwrap());
    *server.body.lock().unwrap() = "trojan://private-node-password@edge.invalid:443#Node0\ntrojan://changed-private-password@edge.invalid:443#Node1".into();
    let preview = cli(&root, &["proxy", "refresh", &profile, "--json"], None);
    assert_eq!(
        preview.status.code(),
        Some(5),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview: Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(
        preview["result"]["outcome"]["outcome"], "prepared",
        "{preview}"
    );
    assert_eq!(
        preview["result"]["outcome"]["impact"]["affected_profiles"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let after = ok(&root, &["proxy", "nodes", &profile, "--json"], None);
    assert_eq!(after["selected_node_id"], second);
    assert_eq!(after["source_revision"], 1);
    assert!(owned.0.is_running().unwrap());
    let observed = ok(&root, &["core", "status", "--json"], None);
    assert_eq!(
        observed["recorded"]["process"]["pid"],
        owned.0.identity().pid
    );
    ok(&root, &["core", "stop", "--json"], None);
    owner.stop();
}

#[test]
#[ignore = "interactive console with no discoverable sing-box: choose Return at inline install prompt"]
fn console_missing_core_return_preserves_subscription() {
    use app_proxy_core::{model::*, registry::*, subscription};
    use app_proxy_windows::subscription_stage::ImportRequest;
    use uuid::Uuid;
    let (_temp, root) = setup();
    let server = Http::new(body(2));
    let mut store = Store::create(&root).unwrap();
    let profile = Uuid::new_v4();
    let staged = store
        .stage_subscription_import(
            &ImportRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 1,
                profile_id: profile,
                name: "subscription".into(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 18123,
                },
                url: server.url.clone(),
                selected_name: "Node0".into(),
            },
            &subscription::parse(&body(2)).unwrap(),
        )
        .unwrap();
    store.apply_config(&staged.request).unwrap();
    let route = Uuid::new_v4();
    store
        .apply_config(&ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::CreateManualProfile {
                profile_id: route,
                name: "unstarted route".into(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 18124,
                },
                node: ManualProxyInput {
                    protocol: ManualProtocol::Http,
                    host: "127.0.0.1".into(),
                    port: 1,
                    credentials: None,
                },
            },
        })
        .unwrap();
    let before = fs::read(root.join("manifest.json")).unwrap();
    drop(store);
    let mut owner = Owner::capture(&root);
    let status = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args([
            "proxy",
            "refresh",
            &profile.to_string(),
            "--via",
            &route.to_string(),
        ])
        .env("PATH", "")
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(5));
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    owner.stop();
    assert_eq!(fs::read(root.join("manifest.json")).unwrap(), before);
}
