use super::*;
use app_proxy_core::registry::{ConfigAction, ConfigRequest};
use app_proxy_windows::{config_transaction::ConfigOutcome, store::Store};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Fixture {
    service: Arc<PreviewService>,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&path).unwrap()));
        let core = Arc::new(CoreManager::new(path, configuration.clone()));
        Self {
            service: PreviewService::new(configuration, core),
            root,
        }
    }
    fn secrets(&self) -> usize {
        std::fs::read_dir(self.root.path().join("store/secrets"))
            .unwrap()
            .count()
    }
    fn apply(&self, request: &ConfigRequest) {
        assert!(matches!(
            self.service.configuration.apply(request).unwrap(),
            ConfigOutcome::Applied { .. }
        ));
    }
}
struct Http {
    url: String,
    requests: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Http {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn http(body: String, stalled: bool) -> Http {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/list?token=private-source-token",
        listener.local_addr().unwrap()
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let task = tokio::spawn(async move {
        let mut children = tokio::task::JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let body = body.clone();
            let seen = seen.clone();
            children.spawn(async move {
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let Ok(byte) = stream.read_u8().await else {
                        return;
                    };
                    request.push(byte);
                    assert!(request.len() < 16384);
                }
                if request.starts_with(b"CONNECT ") {
                    if stream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    request.clear();
                    while !request.ends_with(b"\r\n\r\n") {
                        let Ok(byte) = stream.read_u8().await else {
                            return;
                        };
                        request.push(byte);
                        assert!(request.len() < 16384);
                    }
                }
                seen.fetch_add(1, Ordering::SeqCst);
                if stalled {
                    let mut byte = [0];
                    let _ = stream.read(&mut byte).await;
                } else {
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                }
            });
        }
    });
    Http {
        url,
        requests,
        task,
    }
}
fn body(count: usize) -> String {
    (0..count)
        .map(|n| format!("trojan://private-node-password@edge.invalid:443#Node{n}\n"))
        .collect()
}
fn request(url: &str) -> PreviewRequest {
    PreviewRequest::Import {
        url: url.into(),
        network: NetworkBinding::Direct {},
    }
}
fn import(profile_id: Uuid, selected_name: &str) -> StageRequest {
    StageRequest::Import {
        profile_id,
        name: "subscription".into(),
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 18998,
        },
        selected_names: vec![selected_name.into()],
    }
}
async fn until(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn completed(service: &PreviewService, id: Uuid) -> PreviewPage {
    until(|| {
        !matches!(
            service.page(id, 0).unwrap().status,
            PreviewStatus::Pending {}
        )
    })
    .await;
    service.page(id, 0).unwrap()
}

#[tokio::test]
async fn download_preview_is_read_only_and_stage_replays_the_exact_durable_request() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let server = http(body(70), false).await;
    let id = Uuid::new_v4();
    service.begin(id, request(&server.url)).unwrap();
    service.begin(id, request(&server.url)).unwrap();
    assert!(matches!(
        service.begin(id, request(&format!("{}-changed", server.url))),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let page = completed(service, id).await;
    assert!(matches!(
        page.status,
        PreviewStatus::Ready {
            nodes: 70,
            unsupported: 0
        }
    ));
    assert_eq!(page.nodes.len(), 64);
    assert_eq!(page.next_offset, Some(64));
    let last = service.page(id, 64).unwrap();
    assert_eq!(last.nodes.len(), 6);
    assert_eq!(last.next_offset, None);
    assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    assert!(
        service
            .configuration
            .snapshot()
            .unwrap()
            .profiles
            .is_empty()
    );
    assert_eq!(fixture.secrets(), 0);
    assert!(service.keeps_alive().unwrap());
    let visible = serde_json::to_string(&page).unwrap();
    assert!(!visible.contains("private-source-token"));
    assert!(!visible.contains("private-node-password"));
    let profile = Uuid::new_v4();
    let stage_id = Uuid::new_v4();
    let staged = service
        .stage(id, stage_id, import(profile, "Node0"))
        .unwrap();
    assert!(
        service
            .configuration
            .snapshot()
            .unwrap()
            .profiles
            .is_empty()
    );
    assert_eq!(fixture.secrets(), 71);
    let encoded = serde_json::to_vec(&staged).unwrap();
    assert!(!String::from_utf8_lossy(&encoded).contains("private-"));
    fixture.apply(&staged.request);
    let saved = saved_page(&service.configuration.snapshot().unwrap(), profile, 0, None).unwrap();
    assert_eq!(saved.nodes.len(), 64);
    assert_eq!(saved.next_offset, Some(64));
    assert!(!serde_json::to_string(&saved).unwrap().contains("secret"));
    // Simulate losing both stage and commit replies: the identical ID recovers
    // the original expected revision even after the commit advanced it.
    let repeated = service
        .stage(id, stage_id, import(profile, "Node0"))
        .unwrap();
    assert_eq!(serde_json::to_vec(&repeated).unwrap(), encoded);
    fixture.apply(&repeated.request);
    assert_eq!(service.configuration.snapshot().unwrap().revision, 2);
    let mut changed = service.configuration.snapshot().unwrap();
    changed.revision += 1;
    assert!(matches!(
        saved_page(&changed, profile, 64, Some(saved.revision)),
        Err(Error::Invalid("CATALOG_CHANGED"))
    ));
    assert!(matches!(
        service.stage(id, stage_id, import(profile, "Node1")),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    service.close(id).unwrap();
    assert!(!service.keeps_alive().unwrap());
}

#[tokio::test]
async fn failed_stage_keeps_the_original_error_on_retry_without_writing_secrets() {
    let fixture = Fixture::new();
    let server = http(body(1), false).await;
    let id = Uuid::new_v4();
    fixture.service.begin(id, request(&server.url)).unwrap();
    completed(&fixture.service, id).await;
    let stage_id = Uuid::new_v4();
    let profile = Uuid::new_v4();
    for _ in 0..2 {
        assert!(matches!(
            fixture
                .service
                .stage(id, stage_id, import(profile, "missing")),
            Err(Error::Invalid("SELECTED_NODE_NOT_FOUND"))
        ));
    }
    assert_eq!(fixture.secrets(), 0);
    assert_eq!(
        fixture.service.configuration.snapshot().unwrap().revision,
        1
    );
}

#[tokio::test]
async fn cancelling_pending_downloads_releases_bounded_slots_without_configuration_changes() {
    let fixture = Fixture::new();
    let server = http(body(1), true).await;
    let ids: Vec<_> = (0..LIMIT).map(|_| Uuid::new_v4()).collect();
    for id in &ids {
        fixture.service.begin(*id, request(&server.url)).unwrap();
    }
    until(|| server.requests.load(Ordering::SeqCst) == LIMIT).await;
    assert!(matches!(
        fixture.service.begin(Uuid::new_v4(), request(&server.url)),
        Err(Error::Invalid("SUBSCRIPTION_PREVIEW_LIMIT"))
    ));
    for id in &ids {
        fixture.service.close(*id).unwrap();
        assert!(matches!(
            fixture.service.page(*id, 0),
            Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))
        ));
    }
    until(|| !fixture.service.keeps_alive().unwrap()).await;
    assert_eq!(fixture.secrets(), 0);
    assert_eq!(
        fixture.service.configuration.snapshot().unwrap().revision,
        1
    );
    let next = http(body(1), false).await;
    let id = Uuid::new_v4();
    fixture.service.begin(id, request(&next.url)).unwrap();
    assert!(matches!(
        completed(&fixture.service, id).await.status,
        PreviewStatus::Ready { .. }
    ));
}

#[tokio::test]
async fn expired_or_previous_epoch_previews_cannot_stage_configuration() {
    let fixture = Fixture::new();
    let server = http(body(1), false).await;
    let id = Uuid::new_v4();
    fixture.service.begin(id, request(&server.url)).unwrap();
    completed(&fixture.service, id).await;
    let replacement = PreviewService::new(
        fixture.service.configuration.clone(),
        fixture.service.core.clone(),
    );
    assert!(matches!(
        replacement.page(id, 0),
        Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))
    ));
    fixture
        .service
        .lock()
        .unwrap()
        .get_mut(&id)
        .unwrap()
        .expires = Instant::now() - Duration::from_secs(1);
    assert!(matches!(
        fixture
            .service
            .stage(id, Uuid::new_v4(), import(Uuid::new_v4(), "Node0")),
        Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))
    ));
    assert!(!fixture.service.keeps_alive().unwrap());
    assert_eq!(fixture.secrets(), 0);
}

#[tokio::test]
async fn refresh_captures_source_revision_before_download_but_allows_unrelated_rename() {
    let fixture = Fixture::new();
    let service = &fixture.service;
    let server = http(body(2), false).await;
    let id = Uuid::new_v4();
    let profile_id = Uuid::new_v4();
    service.begin(id, request(&server.url)).unwrap();
    completed(service, id).await;
    let staged = service
        .stage(id, Uuid::new_v4(), import(profile_id, "Node0"))
        .unwrap();
    fixture.apply(&staged.request);
    service.close(id).unwrap();
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();
    for id in [a, b] {
        service
            .begin(
                id,
                PreviewRequest::Refresh {
                    profile_id,
                    network: NetworkBinding::Direct {},
                },
            )
            .unwrap();
        completed(service, id).await;
    }
    fixture.apply(&ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 2,
        action: ConfigAction::RenameProfile {
            profile_id,
            name: "renamed".into(),
        },
    });
    let refresh = service
        .stage(a, Uuid::new_v4(), StageRequest::Refresh {})
        .unwrap();
    assert_eq!(refresh.request.expected_revision, 3);
    fixture.apply(&refresh.request);
    let stage_id = Uuid::new_v4();
    for _ in 0..2 {
        assert!(matches!(
            service.stage(b, stage_id, StageRequest::Refresh {}),
            Err(Error::Invalid("STALE_SUBSCRIPTION_SOURCE"))
        ));
    }
    let manifest = service.configuration.snapshot().unwrap();
    assert_eq!(manifest.revision, 4);
    assert_eq!(manifest.profiles[0].name, "renamed");
    assert_eq!(fixture.secrets(), 3);
}

#[tokio::test]
async fn profile_download_never_falls_back_to_direct_or_adopts_an_external_listener() {
    let fixture = Fixture::new();
    let server = http(body(1), false).await;
    let profile_id = Uuid::new_v4();
    let external = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    fixture.apply(&ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        action: ConfigAction::CreateManualProfile {
            profile_id,
            name: "external fixture".into(),
            endpoint: Endpoint {
                host: "127.0.0.1".parse().unwrap(),
                port: external.local_addr().unwrap().port(),
            },
            node: app_proxy_core::registry::ManualProxyInput {
                protocol: app_proxy_core::model::ManualProtocol::Http,
                host: "127.0.0.1".into(),
                port: 18080,
                credentials: None,
            },
        },
    });
    let id = Uuid::new_v4();
    fixture
        .service
        .begin(
            id,
            PreviewRequest::Import {
                url: server.url.clone(),
                network: NetworkBinding::Profile { profile_id },
            },
        )
        .unwrap();
    assert!(
        matches!(completed(&fixture.service, id).await.status, PreviewStatus::Failed { code } if code == "SUBSCRIPTION_DOWNLOAD_PROXY_NOT_READY")
    );
    assert_eq!(server.requests.load(Ordering::SeqCst), 0);
    external.set_nonblocking(true).unwrap();
    assert!(matches!(external.accept(), Err(e) if e.kind() == std::io::ErrorKind::WouldBlock));
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; own fixture core and loopback HTTP proxy only"]
async fn real_owned_core_download_uses_selected_route_and_preserves_process_on_failure() {
    use app_proxy_windows::{core_process::CoreProcess, core_state::CoreState};
    struct Cleanup(Arc<Configuration>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            if let Ok(store) = self.0.lock()
                && let Ok(CoreState::Running { process, .. }) = store.core_state()
                && let Ok(core) = CoreProcess::attach(&process)
            {
                let _ = core.stop();
            }
        }
    }
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let fixture = Fixture::new();
    let _cleanup = Cleanup(fixture.service.configuration.clone());
    let installed = fixture.root.path().join("store/bin/sing-box/1.14.1");
    std::fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        std::fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    // This responds as a synthetic HTTP upstream proxy. The target .invalid
    // domain cannot be downloaded directly; observing its request proves route use.
    let upstream = http(body(2), false).await;
    let upstream_port = reqwest::Url::parse(&upstream.url).unwrap().port().unwrap();
    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = reservation.local_addr().unwrap().port();
    let profile_id = Uuid::new_v4();
    fixture.apply(&ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        action: ConfigAction::CreateManualProfile {
            profile_id,
            name: "download route".into(),
            endpoint: Endpoint {
                host: "127.0.0.1".parse().unwrap(),
                port,
            },
            node: app_proxy_core::registry::ManualProxyInput {
                protocol: app_proxy_core::model::ManualProtocol::Http,
                host: "127.0.0.1".into(),
                port: upstream_port,
                credentials: None,
            },
        },
    });
    drop(reservation);
    let manager = &fixture.service.core;
    let ready = manager
        .ensure_with(&[profile_id], profile_id, |_| async { Ok(()) })
        .await
        .unwrap();
    let id = Uuid::new_v4();
    fixture
        .service
        .begin(
            id,
            PreviewRequest::Import {
                url: "http://subscription.invalid/fixture?token=private-source-token".into(),
                network: NetworkBinding::Profile { profile_id },
            },
        )
        .unwrap();
    let status = completed(&fixture.service, id).await.status;
    assert!(
        matches!(status, PreviewStatus::Ready { nodes: 2, .. }),
        "{status:?}"
    );
    assert_eq!(upstream.requests.load(Ordering::SeqCst), 1);
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    fixture.service.close(id).unwrap();
    // A failing download is read-only and does not stop the running shared core.
    assert!(
        manager
            .download_subscription(profile_id, "not-a-url")
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    assert!(
        CoreProcess::attach(&ready.process)
            .unwrap()
            .is_running()
            .unwrap()
    );
    manager.stop().await.unwrap();
}
