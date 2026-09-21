use super::*;
use app_proxy_core::{launch::LaunchPhase, model::*};

#[tokio::test]
async fn login_busy_mutation_keeps_queries_available_and_validates_request_identity() {
    let fixture = Fixture::new();
    let held = fixture
        .shared
        .login_jobs
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let server = fixture.server();
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::LoginResume {
                    request_id: Uuid::new_v4()
                }
            )
            .await,
        Err(Error::Invalid("GUARD_LOGIN_BUSY"))
    ));
    assert!(
        matches!(fixture.rpc(Uuid::new_v4(), Operation::LoginStatus {}).await.unwrap(),
        Reply::LoginStatus { status } if !status.ready && status.integration.is_none())
    );
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::LoginRequest {
                    request_id: Uuid::new_v4()
                }
            )
            .await
            .unwrap(),
        Reply::LoginRequest { status: None }
    ));
    drop(held);
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::LoginApply {
                    request: app_proxy_windows::guard_task::login::journal::Request {
                        id: Uuid::new_v4(),
                        expected_revision: 2,
                        action: app_proxy_windows::guard_task::login::journal::Action::Create,
                        expected_creation: None,
                    }
                }
            )
            .await,
        Err(Error::Invalid("INVALID_LOGIN_REQUEST"))
    ));
    server.await.unwrap().unwrap();
    assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 2);
}

#[tokio::test]
async fn login_status_timeout_retains_worker_slot_and_owner_until_native_work_finishes() {
    let fixture = Fixture::new();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let configuration = fixture.shared.configuration.clone();
    let thread = std::thread::spawn(move || {
        let _held = configuration.lock().unwrap();
        ready_tx.send(()).unwrap();
        let _ = release_rx.recv();
    });
    ready_rx.recv().unwrap();
    // Drive only RPC handlers: the coordinator startup/idle paths legitimately
    // need the held store lock and are not the native query under test.
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let shared = fixture.shared.clone();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            handle(listener.accept().await.unwrap(), shared.clone())
                .await
                .unwrap();
        }
    });
    assert!(matches!(
        fixture.rpc(Uuid::new_v4(), Operation::LoginStatus {}).await,
        Err(Error::Invalid("GUARD_LOGIN_CHECK_TIMEOUT"))
    ));
    assert_eq!(fixture.shared.jobs.load(Ordering::SeqCst), 1);
    assert!(matches!(
        fixture.rpc(Uuid::new_v4(), Operation::LoginStatus {}).await,
        Err(Error::Invalid("GUARD_LOGIN_BUSY"))
    ));
    release_tx.send(()).unwrap();
    thread.join().unwrap();
    server.await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while fixture.shared.jobs.load(Ordering::SeqCst) != 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(fixture.shared.jobs.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.shared.login_queries.available_permits(), 1);
}

#[tokio::test]
async fn login_removal_survives_lost_reply_and_replays_after_source_disappears() {
    use app_proxy_windows::guard_task::login::journal::{
        Action, Request as LoginRequest, Status as LoginStatus,
    };
    use sha2::{Digest, Sha256};
    let fixture = Fixture::new();
    let create = LoginRequest {
        id: Uuid::new_v4(),
        expected_revision: 2,
        action: Action::Create,
        expected_creation: None,
    };
    let remove = LoginRequest {
        id: Uuid::new_v4(),
        expected_revision: 3,
        action: Action::Remove,
        expected_creation: Some(create.id),
    };
    {
        let mut store = fixture.shared.configuration.lock().unwrap();
        let mut model = store.load().unwrap();
        let scope = format!("{:x}", Sha256::digest(model.owner_sid.as_bytes()));
        let host = fixture.root.path().join("missing/app-proxy-host.exe");
        model.integrations.guard_login_task = Some(LoginTask {
            name: format!("AppProxy-Login-{}-{}", &scope[..16], model.store_id),
            target: host.clone(),
            args: vec![
                "serve".into(),
                "--home".into(),
                fixture.shared.root.to_str().unwrap().into(),
                "--expected-store".into(),
                model.store_id.to_string(),
            ],
        });
        let record = serde_json::json!({"version":1,"store_id":model.store_id,"entries":[{
            "create":create,"registration":{"store_id":model.store_id,"owner_sid":model.owner_sid,
                "home":fixture.shared.root,"host":host},"created_revision":3,"removal":remove,"removed_revision":null,"removed_at":null
        }]});
        store.commit(2, model).unwrap();
        std::fs::write(
            fixture.shared.root.join("state/login-task.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }
    let server = fixture.server();
    let mut connection = ipc::connect(
        fixture.shared.identity.store_id,
        &policy(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    connection
        .send(&hello(
            fixture.shared.identity.store_id,
            policy().session_id,
            None,
        ))
        .await
        .unwrap();
    connection.receive::<Welcome>().await.unwrap();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::LoginResume {
                request_id: remove.id,
            },
        })
        .await
        .unwrap();
    drop(connection);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                fixture
                    .rpc(
                        Uuid::new_v4(),
                        Operation::LoginRequest {
                            request_id: remove.id
                        }
                    )
                    .await
                    .unwrap(),
                Reply::LoginRequest {
                    status: Some(LoginStatus::Removed { revision: 4 })
                }
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let held = fixture
        .shared
        .login_jobs
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let mut conflict = create.clone();
    conflict.expected_revision = 4;
    assert!(matches!(
        fixture
            .rpc(conflict.id, Operation::LoginApply { request: conflict })
            .await,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::LoginResume {
                    request_id: create.id
                }
            )
            .await
            .unwrap(),
        Reply::LoginRequest {
            status: Some(LoginStatus::Created { revision: 3 })
        }
    ));
    assert!(matches!(
        fixture
            .rpc(create.id, Operation::LoginApply { request: create })
            .await
            .unwrap(),
        Reply::LoginRequest {
            status: Some(LoginStatus::Created { revision: 3 })
        }
    ));
    assert!(matches!(
        fixture
            .rpc(remove.id, Operation::LoginApply { request: remove })
            .await
            .unwrap(),
        Reply::LoginRequest {
            status: Some(LoginStatus::Removed { revision: 4 })
        }
    ));
    drop(held);
    assert!(
        matches!(fixture.rpc(Uuid::new_v4(), Operation::LoginStatus {}).await.unwrap(),
        Reply::LoginStatus { status } if !status.ready && status.integration.is_none())
    );
    server.await.unwrap().unwrap();
    assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 4);
}

#[tokio::test]
async fn shortcut_busy_worker_rejects_extra_mutations_and_leaves_queries_available() {
    let fixture = Fixture::new();
    let held = fixture
        .shared
        .shortcut_jobs
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let server = fixture.server();
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..MAX_CLIENTS * 2 {
        let store_id = fixture.shared.identity.store_id;
        clients.spawn(async move {
            rpc(
                store_id,
                &policy(),
                Duration::from_secs(3),
                Request {
                    protocol_major: PROTOCOL_MAJOR,
                    request_id: Uuid::new_v4(),
                    operation: Operation::ShortcutResume {
                        request_id: Uuid::new_v4(),
                    },
                },
            )
            .await
        });
    }
    while let Some(result) = clients.join_next().await {
        assert!(matches!(
            result.unwrap(),
            Err(Error::Invalid("SHORTCUT_OPERATION_BUSY"))
        ));
    }
    assert!(
        matches!(fixture.rpc(Uuid::new_v4(), Operation::ShortcutStatus { instance_id: fixture.instance }).await.unwrap(), Reply::ShortcutStatus { status } if status.integration.is_none())
    );
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::ShortcutRequest {
                    request_id: Uuid::new_v4()
                }
            )
            .await
            .unwrap(),
        Reply::ShortcutRequest { status: None }
    ));
    assert!(matches!(
        fixture
            .rpc(Uuid::new_v4(), Operation::Status {})
            .await
            .unwrap(),
        Reply::Status { .. }
    ));
    drop(held);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn shortcut_resume_survives_reply_loss_and_removal_uses_durable_registration() {
    use app_proxy_windows::shortcuts::{
        Spec,
        journal::{Action, Plan, Request as ShortcutRequest, Status as ShortcutStatus},
    };
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::new();
    let create = ShortcutRequest {
        id: Uuid::new_v4(),
        instance_id: fixture.instance,
        expected_revision: 2,
        action: Action::Create,
        expected_creation: None,
    };
    let path = fixture.root.path().join("fixture.lnk");
    let pinned = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.shared.root.join("manifest.json"))
        .unwrap();
    assert!(
        fixture
            .shared
            .configuration
            .lock()
            .unwrap()
            .apply_shortcut(
                &create,
                Some(Plan {
                    path: path.clone(),
                    spec: Spec {
                        store_id: fixture.shared.identity.store_id,
                        instance_id: fixture.instance,
                        home: fixture.shared.root.clone(),
                        host: fixture.root.path().join("app-proxy-host.exe"),
                        icon: fixture.root.path().join("fixture.ico"),
                    }
                })
            )
            .is_err()
    );
    drop(pinned);
    let server = fixture.server();
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::ShortcutRequest {
                    request_id: create.id
                }
            )
            .await
            .unwrap(),
        Reply::ShortcutRequest {
            status: Some(ShortcutStatus::Pending { .. })
        }
    ));
    assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 2);
    let policy = policy();
    let mut connection = ipc::connect(
        fixture.shared.identity.store_id,
        &policy,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    connection
        .send(&hello(
            fixture.shared.identity.store_id,
            policy.session_id,
            None,
        ))
        .await
        .unwrap();
    connection.receive::<Welcome>().await.unwrap();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::ShortcutResume {
                request_id: create.id,
            },
        })
        .await
        .unwrap();
    drop(connection);
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if matches!(
                fixture
                    .shared
                    .configuration
                    .lock()
                    .unwrap()
                    .shortcut_request_status(create.id)
                    .unwrap(),
                Some(ShortcutStatus::Created { .. })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let replay = fixture
        .rpc(
            create.id,
            Operation::ShortcutApply {
                request: create.clone(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        replay,
        Reply::ShortcutRequest {
            status: Some(ShortcutStatus::Created { revision: 3, .. })
        }
    ));
    let remove = ShortcutRequest {
        id: Uuid::new_v4(),
        expected_revision: 3,
        action: Action::Remove,
        expected_creation: Some(create.id),
        ..create
    };
    assert!(matches!(
        fixture
            .rpc(remove.id, Operation::ShortcutApply { request: remove })
            .await
            .unwrap(),
        Reply::ShortcutRequest {
            status: Some(ShortcutStatus::Removed { revision: 4, .. })
        }
    ));
    assert!(!path.exists());
    assert!(
        matches!(fixture.rpc(Uuid::new_v4(), Operation::ShortcutStatus { instance_id: fixture.instance }).await.unwrap(), Reply::ShortcutStatus { status } if status.integration.is_none())
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn runtime_status_busy_query_returns_unknown_and_keeps_other_rpcs_available() {
    let fixture = Fixture::new();
    let held = fixture
        .shared
        .guard_queries
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let server = fixture.server();
    let Reply::RuntimeStatus { status } = fixture
        .rpc(
            Uuid::new_v4(),
            Operation::RuntimeStatus {
                instance_id: fixture.instance,
            },
        )
        .await
        .unwrap()
    else {
        panic!("runtime response missing")
    };
    assert!(
        matches!(status.observation, crate::launch_engine::InstanceObservation::Unknown { code } if code == "INSTANCE_OBSERVATION_BUSY")
    );
    assert!(matches!(
        fixture
            .rpc(Uuid::new_v4(), Operation::Status {})
            .await
            .unwrap(),
        Reply::Status { .. }
    ));
    drop(held);
    let Reply::RuntimeStatus { status } = fixture
        .rpc(
            Uuid::new_v4(),
            Operation::RuntimeStatus {
                instance_id: fixture.instance,
            },
        )
        .await
        .unwrap()
    else {
        panic!("runtime response missing")
    };
    assert_eq!(status.instance_id, fixture.instance);
    assert_eq!(
        status.revision,
        fixture.shared.configuration.snapshot().unwrap().revision
    );
    assert!(matches!(
        status.observation,
        crate::launch_engine::InstanceObservation::Absent {}
    ));
    assert!(
        fixture
            .shared
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap()
            .is_empty()
    );
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn subscription_rpc_keeps_preview_alive_and_preserves_failed_stage_reason() {
    use crate::subscription_preview::{PreviewRequest, PreviewStatus, StageRequest};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let fixture = Fixture::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "http://{}/?token=private-source-token",
        listener.local_addr().unwrap()
    );
    let http = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(stream.read_u8().await.unwrap());
        }
        let body = "trojan://private-password@edge.invalid:443#Node";
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
    });
    let server = fixture.server();
    let id = Uuid::new_v4();
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::SubscriptionPreview {
                    id,
                    request: PreviewRequest::Import {
                        url,
                        network: NetworkBinding::Direct {}
                    }
                }
            )
            .await
            .unwrap(),
        Reply::SubscriptionPreview { .. }
    ));
    let page = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let Reply::SubscriptionPreview { page } = fixture
                .rpc(
                    Uuid::new_v4(),
                    Operation::SubscriptionPreviewPage { id, offset: 0 },
                )
                .await
                .unwrap()
            else {
                panic!()
            };
            match page.status {
                PreviewStatus::Pending {} => tokio::time::sleep(Duration::from_millis(10)).await,
                PreviewStatus::Ready { .. } => break page,
                PreviewStatus::Failed { code } => panic!("{code}"),
            }
        }
    })
    .await
    .unwrap();
    http.await.unwrap();
    assert_eq!(page.nodes[0].name, "Node");
    assert!(!serde_json::to_string(&page).unwrap().contains("private-"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!server.is_finished());
    assert!(
        fixture
            .shared
            .configuration
            .snapshot()
            .unwrap()
            .profiles
            .is_empty()
    );
    // Failed staging must return the same concrete code over real IPC too.
    let failed_id = Uuid::new_v4();
    for _ in 0..2 {
        assert!(matches!(
            fixture
                .rpc(
                    Uuid::new_v4(),
                    Operation::SubscriptionStage {
                        preview_id: id,
                        stage_id: failed_id,
                        request: StageRequest::Refresh {}
                    }
                )
                .await,
            Err(Error::Invalid("SUBSCRIPTION_PREVIEW_KIND_MISMATCH"))
        ));
    }
    fixture
        .rpc(Uuid::new_v4(), Operation::SubscriptionPreviewClose { id })
        .await
        .unwrap();
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::SubscriptionPreviewPage { id, offset: 0 }
            )
            .await,
        Err(Error::Invalid("SUBSCRIPTION_PREVIEW_EXPIRED"))
    ));
    server.await.unwrap().unwrap();
}

struct Fixture {
    shared: Arc<Shared>,
    instance: Uuid,
    events: PathBuf,
    root: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("store");
        let exe = root
            .path()
            .join(format!("rpc-launch-{}.exe", Uuid::new_v4()));
        std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        let events = root.path().join("events");
        std::fs::create_dir(&events).unwrap();
        let mut store = store::Store::create(&home).unwrap();
        let mut manifest = store.load().unwrap();
        let application_id = Uuid::new_v4();
        let instance = Uuid::new_v4();
        manifest.applications.push(Application {
            id: application_id,
            name: "RPC fixture".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path: exe },
            template_ref: Template::Environment,
        });
        manifest.instances.push(Instance {
            id: instance,
            application_id,
            name: "RPC fixture".into(),
            revision: 1,
            data: InstanceData::Original {},
            args: vec![
                "--ignored".into(),
                "--exact".into(),
                "launch_engine::tests::engine_child".into(),
            ],
            env: SavedEnvironment {
                set: [
                    (
                        "APP_PROXY_ENGINE_EVENTS".into(),
                        EnvValue::Literal {
                            value: events.to_str().unwrap().into(),
                        },
                    ),
                    (
                        "APP_PROXY_ENGINE_VALUE".into(),
                        EnvValue::Literal {
                            value: "rpc-private-value".into(),
                        },
                    ),
                ]
                .into(),
                unset: vec![],
            },
            cwd: WorkingDirectory::Explicit {
                path: root.path().to_owned(),
            },
            network: NetworkBinding::Direct {},
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
        store.commit(manifest.revision, manifest).unwrap();
        let current = identity::current().unwrap();
        let status = Status {
            store_id: store.load().unwrap().store_id,
            revision: 2,
            epoch: Uuid::new_v4(),
            coordinator_pid: current.pid,
            session_id: current.session_id,
            applications: 1,
            instances: 1,
            profiles: 0,
            phase: "bootstrap".into(),
        };
        Self {
            shared: Arc::new(Shared::new(home, store, status).unwrap()),
            instance,
            events,
            root,
        }
    }
    fn server(&self) -> tokio::task::JoinHandle<Result<()>> {
        let listener = ipc::Listener::bind(self.shared.identity.store_id, policy()).unwrap();
        tokio::spawn(serve_connections(
            listener,
            self.shared.clone(),
            Duration::from_millis(200),
        ))
    }
    async fn rpc(&self, request_id: Uuid, operation: Operation) -> Result<Reply> {
        rpc(
            self.shared.identity.store_id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation,
            },
        )
        .await
    }
    async fn result(&self, id: Uuid, ready_only: bool) -> LaunchAttempt {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let reply = self
                    .rpc(Uuid::new_v4(), Operation::LaunchStatus { request_id: id })
                    .await
                    .unwrap();
                let attempt = match reply {
                    Reply::LaunchStatus {
                        attempt: Some(attempt),
                    } => attempt,
                    // A separate query connection can overtake admission after
                    // the sender drops its connection without waiting for ACK.
                    Reply::LaunchStatus { attempt: None } => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                        continue;
                    }
                    _ => panic!("unexpected launch status reply"),
                };
                if attempt.finished_at.is_some()
                    || (ready_only && matches!(attempt.phase, LaunchPhase::ReadyToSpawn {}))
                {
                    return attempt;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(store) = self.shared.configuration.lock() {
            for attempt in store.launch_attempts().unwrap_or_default() {
                if let LaunchPhase::Confirmed { process } = attempt.phase {
                    let _ = process::terminate_exact(&process);
                }
            }
        }
    }
}
fn policy() -> ipc::PeerPolicy {
    ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
}

#[tokio::test]
async fn lost_launch_ack_replays_once_preserves_application_and_owner_observes_exit() {
    let fixture = Fixture::new();
    let server = fixture.server();
    let request_id = Uuid::new_v4();
    let mut client = ipc::connect(
        fixture.shared.identity.store_id,
        &policy(),
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    client
        .send(&hello(
            fixture.shared.identity.store_id,
            fixture.shared.identity.session_id,
            None,
        ))
        .await
        .unwrap();
    assert!(matches!(
        client.receive::<Welcome>().await.unwrap(),
        Welcome::Ready { .. }
    ));
    client
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id,
            operation: Operation::Launch {
                instance_id: fixture.instance,
                origin: LaunchOrigin::Interactive,
                expected_revision: None,
            },
        })
        .await
        .unwrap();
    drop(client);
    let attempt = fixture.result(request_id, false).await;
    let LaunchPhase::Confirmed { process } = attempt.phase else {
        panic!("launch failed")
    };
    let replay = fixture
        .rpc(
            request_id,
            Operation::Launch {
                instance_id: fixture.instance,
                origin: LaunchOrigin::Interactive,
                expected_revision: None,
            },
        )
        .await
        .unwrap();
    let wire = serde_json::to_string(&replay).unwrap();
    assert!(!wire.contains("rpc-private-value") && !wire.contains("APP_PROXY_ENGINE_VALUE"));
    assert!(
        matches!(replay, Reply::LaunchStatus { attempt: Some(a) } if a.id == request_id && matches!(a.phase, LaunchPhase::Confirmed { .. }))
    );
    assert!(matches!(
        fixture
            .rpc(
                request_id,
                Operation::Launch {
                    instance_id: fixture.instance,
                    origin: LaunchOrigin::Guard,
                    expected_revision: None,
                }
            )
            .await,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let cancelled = fixture
        .rpc(Uuid::new_v4(), Operation::CancelLaunch { request_id })
        .await
        .unwrap();
    assert!(
        matches!(cancelled, Reply::LaunchStatus { attempt: Some(a) } if matches!(a.phase, LaunchPhase::Confirmed { .. }))
    );
    assert!(process::is_running_exact(&process).unwrap());
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert!(!server.is_finished());
    assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 1);
    process::terminate_exact(&process).unwrap();
    tokio::time::timeout(Duration::from_secs(7), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .shared
            .configuration
            .lock()
            .unwrap()
            .launch_request(request_id)
            .unwrap()
            .unwrap()
            .session_exited
    );
}

#[tokio::test]
async fn launch_wait_does_not_hold_rpc_slots_and_cancellation_releases_idle_owner() {
    let fixture = Fixture::new();
    let gate = Arc::new(tokio::sync::Notify::new());
    fixture.shared.launch.hold_dispatch(gate.clone());
    let server = fixture.server();
    let first = Uuid::new_v4();
    for index in 0..MAX_CLIENTS + 1 {
        let request_id = if index == 0 { first } else { Uuid::new_v4() };
        let reply = fixture
            .rpc(
                request_id,
                Operation::Launch {
                    instance_id: fixture.instance,
                    origin: LaunchOrigin::Interactive,
                    expected_revision: None,
                },
            )
            .await
            .unwrap();
        assert!(matches!(reply, Reply::LaunchStatus { attempt: Some(a) } if a.id == first));
    }
    assert!(matches!(
        fixture.result(first, true).await.phase,
        LaunchPhase::ReadyToSpawn {}
    ));
    assert!(matches!(
        fixture
            .rpc(Uuid::new_v4(), Operation::Status {})
            .await
            .unwrap(),
        Reply::Status { .. }
    ));
    let cancelled = fixture
        .rpc(
            Uuid::new_v4(),
            Operation::CancelLaunch { request_id: first },
        )
        .await
        .unwrap();
    assert!(matches!(cancelled, Reply::LaunchStatus { attempt: Some(a) } if a.cancel_requested));
    gate.notify_one();
    // No further query is needed to notice detached completion and allow exit.
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        fixture.shared.launch.status(first).unwrap().unwrap().phase,
        LaunchPhase::Cancelled {}
    ));
    assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
    assert!(fixture.root.path().join("store/manifest.json").is_file());
}

#[tokio::test]
async fn guard_status_busy_scan_leaves_metadata_and_other_requests_available() {
    let fixture = Fixture::new();
    let held = fixture
        .shared
        .guard_queries
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let server = fixture.server();
    let mut queries = tokio::task::JoinSet::new();
    for _ in 0..MAX_CLIENTS * 2 {
        let store_id = fixture.shared.identity.store_id;
        let instance_id = fixture.instance;
        queries.spawn(async move {
            rpc(
                store_id,
                &policy(),
                Duration::from_secs(3),
                Request {
                    protocol_major: PROTOCOL_MAJOR,
                    request_id: Uuid::new_v4(),
                    operation: Operation::GuardStatus { instance_id },
                },
            )
            .await
            .unwrap()
        });
    }
    while let Some(reply) = queries.join_next().await {
        let Reply::GuardStatus { status } = reply.unwrap() else {
            panic!("missing guard status")
        };
        assert!(status.scan.is_none());
        assert_eq!(status.diagnostic.as_deref(), Some("GUARD_SCAN_BUSY"));
        assert!(status.phase == crate::guard_control::GuardPhase::Disabled);
    }
    assert!(matches!(
        fixture
            .rpc(Uuid::new_v4(), Operation::Status {})
            .await
            .unwrap(),
        Reply::Status { .. }
    ));
    drop(held);
    let Reply::GuardStatus { status } = fixture
        .rpc(
            Uuid::new_v4(),
            Operation::GuardStatus {
                instance_id: fixture.instance,
            },
        )
        .await
        .unwrap()
    else {
        panic!("missing status")
    };
    assert!(status.diagnostic.is_none());
    assert!(matches!(
        status.scan.unwrap().observation,
        crate::launch_engine::GuardObservation::Disabled {}
    ));
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .shared
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn guard_slow_observations_return_metadata_before_real_rpc_frame_deadline() {
    use crate::guard_control::{self, ComponentState, GuardPhase, GuardStatus};
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let instance = fixture.instance;
    let server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        connection
            .send(&Welcome::Ready {
                hello: hello(identity.store_id, identity.session_id, Some(identity.epoch)),
            })
            .await
            .unwrap();
        let request: Request = connection.receive().await.unwrap();
        assert!(
            matches!(request.operation, Operation::GuardStatus { instance_id } if instance_id == instance)
        );
        // The actual production deadline aggregator sees two unresolved native
        // observations. No fake short RPC timeout or elevated operation is used.
        let (scan, listener, diagnostic) =
            guard_control::observations(std::future::pending(), std::future::pending())
                .await
                .unwrap();
        connection
            .send(&Response {
                request_id: request.request_id,
                epoch: identity.epoch,
                result: Reply::GuardStatus {
                    status: Box::new(GuardStatus {
                        instance_id: instance,
                        revision: identity.revision,
                        desired: Desired::Enabled,
                        phase: GuardPhase::Blocked,
                        listener,
                        scan,
                        diagnostic,
                    }),
                },
            })
            .await
            .unwrap();
    });
    let before = tokio::time::Instant::now();
    let result = fixture
        .rpc(
            Uuid::new_v4(),
            Operation::GuardStatus {
                instance_id: instance,
            },
        )
        .await
        .unwrap();
    assert!(before.elapsed() < Duration::from_secs(5));
    let Reply::GuardStatus { status } = result else {
        panic!("missing metadata")
    };
    assert_eq!(status.instance_id, instance);
    assert!(status.scan.is_none());
    assert!(status.listener == ComponentState::Unverified);
    assert_eq!(status.diagnostic.as_deref(), Some("GUARD_SCAN_TIMEOUT"));
    server.await.unwrap();
}

#[tokio::test]
async fn client_refuses_a_server_of_another_major_before_sending_the_operation() {
    // Programs ship as a set, so there is nothing to negotiate below the major.
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let other_server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
        greeting.protocol_major = PROTOCOL_MAJOR + 1;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture.rpc(Uuid::new_v4(), Operation::CoreStatus {}).await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    other_server.await.unwrap();
}
