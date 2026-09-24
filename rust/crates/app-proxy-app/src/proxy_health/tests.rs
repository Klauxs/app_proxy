use super::*;
use std::{process::Command, sync::Arc};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
};

const HOST: &str = "health.app-proxy.invalid";
const TARGET: &str = "https://health.app-proxy.invalid/check?secret=never-log-this";

#[test]
fn health_failure_receipt_keeps_specific_code_and_readable_cli_reason() {
    use app_proxy_core::core_control::{CoreOutcome, CoreRequestStatus};
    let error = failure(Stage::ConnectAndTls, Failure::TotalTimeout);
    let receipt = CoreRequestStatus::Complete {
        outcome: CoreOutcome::Failed {
            code: error.code().into(),
        },
        completed_at: 0,
    };
    let failure = crate::core_cli::outcome(Some(receipt)).unwrap_err();
    assert_eq!(failure.exit_code, crate::exit::UNAVAILABLE);
    let message = failure.to_string();
    assert!(message.contains("30 秒"));
    assert!(message.contains(error.code()));
    assert!(!message.contains(TARGET));
}

#[tokio::test(start_paused = true)]
async fn fifth_retry_can_succeed_and_exhaustion_stops_after_six_attempts() {
    for succeeds in [true, false] {
        let mut attempts = 0;
        let result = check_with_retries(|| {
            attempts += 1;
            let attempt = attempts;
            async move {
                if succeeds && attempt == 6 {
                    Ok(Evidence {
                        status: 204,
                        elapsed_ms: 0,
                    })
                } else {
                    Err(failure(Stage::ConnectAndTls, Failure::Timeout))
                }
            }
        })
        .await;
        assert_eq!(attempts, 6);
        if succeeds {
            assert_eq!(result.unwrap().elapsed_ms, 9000);
        } else {
            assert_eq!(result.unwrap_err().code(), "CORE_PROXY_CONNECT_TIMEOUT");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn overall_deadline_bounds_inflight_request_and_retry_delay() {
    for per_attempt in [Duration::from_secs(15), Duration::from_millis(29500)] {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let result = check_with_retries(|| {
            attempts += 1;
            async move {
                tokio::time::sleep(per_attempt).await;
                Err(failure(Stage::Response, Failure::Timeout))
            }
        })
        .await;
        assert_eq!(result.unwrap_err().kind, Failure::TotalTimeout);
        assert_eq!(started.elapsed(), CHECK_TIMEOUT);
        assert_eq!(attempts, if per_attempt.as_secs() == 15 { 2 } else { 1 });
    }
}

#[tokio::test(start_paused = true)]
async fn permanent_failures_do_not_retry_or_wait() {
    for kind in [
        Failure::InvalidInput,
        Failure::ClientSetup,
        Failure::Transport,
        Failure::UnexpectedStatus,
        Failure::BodyTooLarge,
    ] {
        let started = tokio::time::Instant::now();
        let mut attempts = 0;
        let error = check_with_retries(|| {
            attempts += 1;
            async move { Err(failure(Stage::Response, kind)) }
        })
        .await
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(started.elapsed(), Duration::ZERO);
        assert_eq!(error.kind, kind);
        assert!(failure_message(error.code()).is_some());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelling_check_during_backoff_stops_further_attempts() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = attempts.clone();
    let task = tokio::spawn(async move {
        check_with_retries(|| {
            counter.fetch_add(1, Ordering::SeqCst);
            async { Err(failure(Stage::ConnectAndTls, Failure::Timeout)) }
        })
        .await
    });
    tokio::task::yield_now().await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::advance(CHECK_TIMEOUT).await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn refused_proxy_connection_recovers_on_same_endpoint_without_direct_fallback() {
    let upstream = fixture(
        Reply::Https(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()),
        HOST,
    )
    .await;
    let reservation = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    let endpoint = Endpoint {
        host: address.ip(),
        port: address.port(),
    };
    drop(reservation);
    let upstream_address = SocketAddr::new(upstream.endpoint.host, upstream.endpoint.port);
    let mut proxy = None;
    let mut attempts = 0;
    let client = client_builder(&endpoint)
        .unwrap()
        .tls_certs_only([upstream.certificate.clone()])
        .build()
        .unwrap();
    let result = check_with_retries(|| {
        attempts += 1;
        // Start only after a real refused connection; Windows TCP retries can
        // hide a fixed listener-start delay inside the first health attempt.
        if attempts == 2 {
            let listener = std::net::TcpListener::bind(address).unwrap();
            listener.set_nonblocking(true).unwrap();
            let listener = TcpListener::from_std(listener).unwrap();
            proxy = Some(tokio::spawn(async move {
                let (mut incoming, _) = listener.accept().await.unwrap();
                let mut outgoing = tokio::net::TcpStream::connect(upstream_address)
                    .await
                    .unwrap();
                let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
            }));
        }
        request(client.clone(), validate(TARGET, &[204]).unwrap(), &[204])
    })
    .await;
    if let Some(proxy) = proxy {
        proxy.abort();
    }
    let evidence = result.unwrap();
    assert_eq!(evidence.status, 204);
    assert_eq!(attempts, 2);
}

#[derive(Clone)]
enum Reply {
    Https(Vec<u8>),
    PlainHttp,
    StallConnect,
    StallBody,
}
struct Fixture {
    endpoint: Endpoint,
    certificate: reqwest::Certificate,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    fn trusted_client(&self) -> ClientBuilder {
        client_builder(&self.endpoint)
            .unwrap()
            .tls_certs_only([self.certificate.clone()])
    }
}

async fn header(stream: &mut (impl AsyncRead + Unpin)) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 8192);
        bytes.push(stream.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}

async fn fixture(reply: Reply, cert_host: &str) -> Fixture {
    fixture_repeat(reply, cert_host, 1).await
}

async fn fixture_repeat(reply: Reply, cert_host: &str, count: usize) -> Fixture {
    let cert = rcgen::generate_simple_self_signed(vec![cert_host.into()]).unwrap();
    let certificate = reqwest::Certificate::from_der(cert.cert.der()).unwrap();
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
    let config = Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: listener.local_addr().unwrap().port(),
    };
    let task = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(15), async move {
            for _ in 0..count {
                let (mut tcp, _) = listener.accept().await.unwrap();
                let connect = header(&mut tcp).await;
                assert!(connect.starts_with(&format!("CONNECT {HOST}:443 HTTP/1.1\r\n")));
                if matches!(reply, Reply::StallConnect) {
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    return;
                }
                tcp.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n")
                    .await
                    .unwrap();
                if matches!(reply, Reply::PlainHttp) {
                    let _ = tcp
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                        .await;
                    return;
                }
                let Ok(mut tls) = TlsAcceptor::from(config.clone()).accept(tcp).await else {
                    return;
                };
                let get = header(&mut tls).await;
                assert!(get.starts_with("GET /check?secret=never-log-this HTTP/1.1\r\n"));
                match reply.clone() {
                    Reply::Https(bytes) => {
                        let _ = tls.write_all(&bytes).await;
                    }
                    Reply::StallBody => {
                        tls.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx")
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }
                    _ => unreachable!(),
                }
                let _ = tls.shutdown().await;
            }
        })
        .await
        .unwrap();
    });
    Fixture {
        endpoint,
        certificate,
        task,
    }
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; owns only isolated fixture core processes"]
async fn managed_core_persists_reuses_recovers_and_preserves_runtime_failures() {
    use crate::{configuration::Configuration, core_manager::CoreManager};
    use app_proxy_core::model::*;
    use app_proxy_windows::{
        Error as PlatformError, core_process::CoreProcess, core_state::CoreState, store::Store,
    };
    use std::{fs, path::PathBuf};
    use uuid::Uuid;
    struct Cleanup(Vec<CoreProcess>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for p in &self.0 {
                let _ = p.stop();
            }
        }
    }
    let mut cleanup = Cleanup(Vec::new());
    let binary = PathBuf::from(
        std::env::var_os("APP_PROXY_TEST_SING_BOX").expect("explicit validation binary"),
    );
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owned");
    let mut store = Store::create(&root).unwrap();
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let upstream = fixture_repeat(
        Reply::Https(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()),
        HOST,
        4,
    )
    .await;
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: reserved.local_addr().unwrap().port(),
    };
    let node = ManualNode {
        id: Uuid::new_v4(),
        name: "fixture".into(),
        protocol: ManualProtocol::Http,
        host: upstream.endpoint.host.to_string(),
        port: upstream.endpoint.port,
        credentials: None,
    };
    let id = Uuid::new_v4();
    let mut manifest = store.load().unwrap();
    manifest.profiles.push(ProxyProfile {
        id,
        name: "fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: endpoint.clone(),
        selected_node_id: node.id,
        source: ProxySource::Manual { nodes: vec![node] },
    });
    let second_reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut second = manifest.profiles[0].clone();
    second.id = Uuid::new_v4();
    second.endpoint.port = second_reserved.local_addr().unwrap().port();
    let second_id = second.id;
    manifest.profiles.push(second);
    store.commit(manifest.revision, manifest).unwrap();
    let configuration = Arc::new(Configuration::new(store));
    let manager = CoreManager::new(root.clone(), configuration.clone());
    let probe = |endpoint: Endpoint| {
        let cert = upstream.certificate.clone();
        async move {
            let client = client_builder(&endpoint)
                .unwrap()
                .tls_certs_only([cert])
                .build()
                .unwrap();
            request(client, validate(TARGET, &[204]).unwrap(), &[204])
                .await
                .map(|_| ())
                .map_err(|_| PlatformError::Invalid("FIXTURE_HEALTH_FAILED"))
        }
    };
    // Foreign listeners stay alive while both conflicting entries move to
    // OS-selected ports and the same request completes its real TLS probe.
    let ready = manager
        .ensure_with(&[id, second_id], id, &probe)
        .await
        .unwrap();
    cleanup.0.push(CoreProcess::attach(&ready.process).unwrap());
    let saved = configuration.snapshot().unwrap();
    let endpoint = saved
        .profiles
        .iter()
        .find(|p| p.id == id)
        .unwrap()
        .endpoint
        .clone();
    let second_endpoint = saved
        .profiles
        .iter()
        .find(|p| p.id == second_id)
        .unwrap()
        .endpoint
        .clone();
    assert_ne!(endpoint.port, reserved.local_addr().unwrap().port());
    assert_ne!(
        second_endpoint.port,
        second_reserved.local_addr().unwrap().port()
    );
    assert!(
        tokio::net::TcpStream::connect(reserved.local_addr().unwrap())
            .await
            .is_ok()
    );
    // Dropping the owner-side manager must not kill the shared core. Recover via
    // the persisted full identity and exact native listener owner evidence.
    drop(manager);
    drop(configuration);
    let configuration = Arc::new(Configuration::new(Store::open(&root).unwrap()));
    let manager = CoreManager::new(root.clone(), configuration.clone());
    let reused = manager.ensure_with(&[id], id, &probe).await.unwrap();
    assert_eq!(ready.process, reused.process);
    assert_eq!(ready.generation, reused.generation);
    {
        let mut store = configuration.lock().unwrap();
        let mut manifest = store.load().unwrap();
        manifest.profiles[0].name = "rename".into();
        manifest.profiles[0].revision += 1;
        store.commit(manifest.revision, manifest).unwrap();
    }
    assert_eq!(
        manager
            .ensure_with(&[id], id, &probe)
            .await
            .unwrap()
            .process,
        ready.process
    );
    assert!(
        manager
            .ensure_with(&[id], id, |_| async {
                Err(PlatformError::Invalid("FIXTURE_NETWORK_DOWN"))
            })
            .await
            .is_err()
    );
    assert!(cleanup.0[0].is_running().unwrap());
    assert!(matches!(
        manager.state().unwrap(),
        CoreState::Running { .. }
    ));
    let change_port = |port| {
        let mut store = configuration.lock().unwrap();
        let mut manifest = store.load().unwrap();
        let ProxySource::Manual { nodes } = &mut manifest.profiles[0].source else {
            panic!("manual fixture required")
        };
        nodes[0].port = port;
        store.commit(manifest.revision, manifest).unwrap();
    };
    assert!(matches!(
        manager
            .ensure_with(&[id], id, |_| async {
                change_port(1);
                Ok(())
            })
            .await,
        Err(PlatformError::Invalid("CORE_CONFIG_CHANGED"))
    ));
    assert!(cleanup.0[0].is_running().unwrap());
    change_port(upstream.endpoint.port);
    manager.stop().await.unwrap();
    assert!(!cleanup.0[0].is_running().unwrap());
    assert_eq!(manager.state().unwrap(), CoreState::Stopped {});
    // Initial health failure cleans up only the newly created core, then leaves
    // a retryable Down journal retaining its profiles. No app processes are used.
    assert!(
        manager
            .ensure_with(&[id], id, |_| async {
                Err(PlatformError::Invalid("FIXTURE_NETWORK_DOWN"))
            })
            .await
            .is_err()
    );
    assert!(matches!(manager.state().unwrap(), CoreState::Down { .. }));
    assert!(std::net::TcpListener::bind((endpoint.host, endpoint.port)).is_ok());
    assert!(matches!(
        manager
            .ensure_with(&[id], id, |_| async {
                change_port(1);
                Ok(())
            })
            .await,
        Err(PlatformError::Invalid("CORE_CONFIG_CHANGED"))
    ));
    assert!(matches!(manager.state().unwrap(), CoreState::Down { .. }));
    change_port(upstream.endpoint.port);
    // Lifecycle-only callbacks isolate crash setup and port-conflict handling;
    // the final recovered start must pass the real TLS health check again.
    let before_crash = manager
        .ensure_with(&[id, second_id], id, |_| async { Ok(()) })
        .await
        .unwrap();
    let crashed = CoreProcess::attach(&before_crash.process).unwrap();
    crashed.stop().unwrap();
    // Model a persisted Running record left by a previous desktop session.
    // Only this isolated fixture journal is changed; the exited native process
    // handle remains open so recovery must also handle signaled process objects.
    let runtime_path = root.join("state/core/runtime.json");
    let mut previous: serde_json::Value =
        serde_json::from_slice(&fs::read(&runtime_path).unwrap()).unwrap();
    previous["state"]["process"]["session_id"] =
        before_crash.process.session_id.wrapping_add(1).into();
    fs::write(&runtime_path, serde_json::to_vec(&previous).unwrap()).unwrap();
    assert!(matches!(
        manager.snapshot().unwrap().observed,
        crate::core_manager::CoreObserved::Down
    ));
    let blocker =
        std::net::TcpListener::bind((second_endpoint.host, second_endpoint.port)).unwrap();
    let after_crash = manager.ensure_with(&[id], id, &probe).await.unwrap();
    cleanup
        .0
        .push(CoreProcess::attach(&after_crash.process).unwrap());
    let saved = configuration.snapshot().unwrap();
    let new_second_endpoint = saved
        .profiles
        .iter()
        .find(|p| p.id == second_id)
        .unwrap()
        .endpoint
        .clone();
    assert!(new_second_endpoint != second_endpoint);
    assert!(saved.profiles.iter().find(|p| p.id == id).unwrap().endpoint == endpoint);
    assert!(std::net::TcpStream::connect((second_endpoint.host, second_endpoint.port)).is_ok());
    drop(manager);
    drop(configuration);
    let configuration = Arc::new(Configuration::new(Store::open(&root).unwrap()));
    let manager = CoreManager::new(root, configuration);
    let reused = manager
        .ensure_with(&[id], id, |_| async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(reused.process, after_crash.process);
    assert_ne!(before_crash.process, after_crash.process);
    assert!(
        cleanup
            .0
            .last()
            .unwrap()
            .listeners_verified(&[endpoint, new_second_endpoint])
            .unwrap()
    );
    manager.stop().await.unwrap();
    drop((reserved, second_reserved, blocker));
}

async fn trusted(f: &Fixture, statuses: &[u16]) -> Result<Evidence, Error> {
    request(
        f.trusted_client().build().unwrap(),
        validate(TARGET, statuses).unwrap(),
        statuses,
    )
    .await
}

#[tokio::test]
async fn connect_tls_and_explicit_status_are_required_for_success() {
    for status in [200, 204, 201] {
        let f = fixture(
            Reply::Https(format!("HTTP/1.1 {status} OK\r\nContent-Length: 0\r\n\r\n").into_bytes()),
            HOST,
        )
        .await;
        assert_eq!(trusted(&f, &[status]).await.unwrap().status, status);
    }
}

#[tokio::test]
async fn plain_http_unknown_certificate_and_wrong_hostname_are_rejected() {
    let plain = fixture(Reply::PlainHttp, HOST).await;
    assert_eq!(
        trusted(&plain, &[200, 204]).await.unwrap_err().kind,
        Failure::Transport
    );
    let untrusted = fixture(Reply::Https(Vec::new()), HOST).await;
    let error = check(&untrusted.endpoint, TARGET, &[200, 204])
        .await
        .unwrap_err();
    assert_eq!(error.kind, Failure::Transport);
    assert!(!error.retryable);
    let wrong_host = fixture(Reply::Https(Vec::new()), "different.app-proxy.invalid").await;
    assert_eq!(
        trusted(&wrong_host, &[200, 204]).await.unwrap_err().kind,
        Failure::Transport
    );
}

#[tokio::test]
async fn redirects_errors_and_oversized_bodies_are_not_health() {
    for status in [302, 407, 500] {
        let f = fixture(Reply::Https(format!("HTTP/1.1 {status} Error\r\nLocation: https://must-not-follow.invalid/secret\r\nContent-Length: 0\r\n\r\n").into_bytes()), HOST).await;
        let error = trusted(&f, &[200, 204]).await.unwrap_err();
        assert_eq!(error.kind, Failure::UnexpectedStatus);
        assert_eq!(error.status, Some(status));
    }
    for length_header in [true, false] {
        let mut bytes = if length_header {
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                BODY_LIMIT + 1
            )
            .into_bytes()
        } else {
            b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec()
        };
        bytes.extend(vec![b'x'; BODY_LIMIT + 1]);
        let f = fixture(Reply::Https(bytes), HOST).await;
        assert_eq!(
            trusted(&f, &[200, 204]).await.unwrap_err().kind,
            Failure::BodyTooLarge
        );
    }
}

#[tokio::test]
async fn timeout_covers_connect_and_body_and_errors_exclude_secrets() {
    for (reply, stage) in [
        (Reply::StallConnect, Stage::ConnectAndTls),
        (Reply::StallBody, Stage::Response),
    ] {
        let f = fixture(reply, HOST).await;
        let client = f
            .trusted_client()
            .timeout(Duration::from_millis(250))
            .build()
            .unwrap();
        let error = request(client, validate(TARGET, &[200]).unwrap(), &[200])
            .await
            .unwrap_err();
        assert_eq!(error.kind, Failure::Timeout);
        assert_eq!(error.stage, stage);
        let diagnostic = format!(
            "{error} {error:?} {}",
            serde_json::to_string(&error).unwrap()
        );
        assert!(!diagnostic.contains("secret"));
        assert!(!diagnostic.contains("app-proxy.invalid"));
    }
}

#[test]
fn invalid_input_is_rejected_before_network_work() {
    for target in [
        "http://example.invalid",
        "https://user:secret@example.invalid",
        "https://example.invalid/#secret",
        "https://example.invalid/\n",
    ] {
        assert!(validate(target, &[200, 204]).is_err());
    }
    for statuses in [&[][..], &[302][..], &[500][..]] {
        assert!(validate(TARGET, statuses).is_err());
    }
    for endpoint in [
        Endpoint {
            host: "1.1.1.1".parse().unwrap(),
            port: 80,
        },
        Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 0,
        },
    ] {
        assert!(client_builder(&endpoint).is_err());
    }
}

#[test]
fn inherited_proxy_and_bypass_do_not_replace_explicit_profile() {
    use std::os::windows::process::CommandExt;
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "proxy_health::tests::connect_tls_and_explicit_status_are_required_for_success",
        ])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "*")
        .creation_flags(0x08000000)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child fixture failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX pointing to the verified development-only sing-box executable"]
async fn real_singbox_carries_connect_and_verified_tls_to_selected_upstream() {
    use app_proxy_core::{model::*, singbox};
    use app_proxy_windows::{singbox_binary, store::Store};
    use std::{
        fs,
        os::windows::process::CommandExt,
        path::PathBuf,
        process::{Child, Stdio},
    };
    use uuid::Uuid;

    struct Core(Child);
    impl Drop for Core {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let binary = PathBuf::from(
        std::env::var_os("APP_PROXY_TEST_SING_BOX").expect("explicit validation binary required"),
    );
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owned");
    let store = Store::create(&root).unwrap();
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let selected = singbox_binary::discover(&root).await.unwrap().unwrap();
    assert_eq!(selected.version(), "1.14.1");
    let upstream = fixture(
        Reply::Https(b"HTTP/1.1 204 No Content\r\n\r\n".to_vec()),
        HOST,
    )
    .await;
    let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: reserved.local_addr().unwrap().port(),
    };
    let node = ManualNode {
        id: Uuid::new_v4(),
        name: "TLS fixture".into(),
        protocol: ManualProtocol::Http,
        host: upstream.endpoint.host.to_string(),
        port: upstream.endpoint.port,
        credentials: None,
    };
    let profile = ProxyProfile {
        id: Uuid::new_v4(),
        name: "owned core TLS fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: endpoint.clone(),
        selected_node_id: node.id,
        source: ProxySource::Manual { nodes: vec![node] },
    };
    let profile_id = profile.id;
    let mut manifest = store.load().unwrap();
    manifest.profiles = vec![profile];
    let compiled =
        singbox::compile(&manifest, &[profile_id], |_| panic!("no fixture secrets")).unwrap();
    let config = root.join("core.json");
    fs::write(&config, compiled.bytes()).unwrap();
    selected.check_config(&config).await.unwrap();
    drop(reserved);
    let mut core = Core(
        Command::new(selected.executable())
            .args(["run", "-c"])
            .arg(config)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        assert!(
            core.0.try_wait().unwrap().is_none(),
            "owned core exited before ready"
        );
        if tokio::net::TcpStream::connect(SocketAddr::new(endpoint.host, endpoint.port))
            .await
            .is_ok()
        {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let client = client_builder(&endpoint)
        .unwrap()
        .tls_certs_only([upstream.certificate.clone()])
        .build()
        .unwrap();
    let evidence = request(client, validate(TARGET, &[204]).unwrap(), &[204])
        .await
        .unwrap();
    assert_eq!(evidence.status, 204);
    // The selected core is the sole route; loss of that core must not cause a
    // request to another service or a direct request to the destination.
    drop(core);
    let client = client_builder(&endpoint)
        .unwrap()
        .tls_certs_only([upstream.certificate.clone()])
        .build()
        .unwrap();
    assert_eq!(
        request(client, validate(TARGET, &[204]).unwrap(), &[204])
            .await
            .unwrap_err()
            .kind,
        Failure::Transport
    );
}
