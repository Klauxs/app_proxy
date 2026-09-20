use super::*;
use app_proxy_core::{launch::LaunchPhase, model::*};

#[tokio::test]
async fn advanced_settings_require_minor_seventeen_in_both_directions() {
    use app_proxy_core::registry::{ConfigAction, InstanceEdit};
    let fixture = Fixture::new();
    let operations = || {
        [
            Operation::InstanceSettings {
                instance_id: fixture.instance,
            },
            Operation::Configure {
                expected_revision: 2,
                action: ConfigAction::EditInstance {
                    instance_id: fixture.instance,
                    edit: InstanceEdit {
                        args: Some(vec![]),
                        ..Default::default()
                    },
                },
            },
        ]
    };
    for operation in operations() {
        let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
        let identity = fixture.shared.identity.clone();
        let old_server = tokio::spawn(async move {
            let mut connection = listener.accept().await.unwrap();
            connection.receive::<Hello>().await.unwrap();
            let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
            greeting.protocol_minor = 16;
            connection
                .send(&Welcome::Ready { hello: greeting })
                .await
                .unwrap();
            assert!(connection.receive::<Request>().await.is_err());
        });
        assert!(matches!(
            fixture.rpc(Uuid::new_v4(), operation).await,
            Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
        ));
        old_server.await.unwrap();
    }
    let server = fixture.server();
    for operation in operations() {
        let policy = policy();
        let mut connection = ipc::connect(
            fixture.shared.identity.store_id,
            &policy,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
        greeting.protocol_minor = 16;
        connection.send(&greeting).await.unwrap();
        connection.receive::<Welcome>().await.unwrap();
        connection
            .send(&Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id: Uuid::new_v4(),
                operation,
            })
            .await
            .unwrap();
        let response: Response = connection.receive().await.unwrap();
        assert!(
            matches!(response.result, Reply::Error { code } if code == "INSTANCE_EDIT_PROTOCOL_UPDATE_REQUIRED")
        );
    }
    server.await.unwrap().unwrap();
    assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 2);
}

#[tokio::test]
async fn shortcut_protocol_requires_minor_sixteen_in_both_directions() {
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let old_server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
        greeting.protocol_minor = 15;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::ShortcutStatus {
                    instance_id: fixture.instance
                }
            )
            .await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    old_server.await.unwrap();
    let server = fixture.server();
    for operation in [
        Operation::ShortcutStatus {
            instance_id: fixture.instance,
        },
        Operation::ShortcutRequest {
            request_id: Uuid::new_v4(),
        },
        Operation::ShortcutResume {
            request_id: Uuid::new_v4(),
        },
        Operation::ShortcutApply {
            request: app_proxy_windows::shortcuts::journal::Request {
                id: Uuid::new_v4(),
                instance_id: fixture.instance,
                expected_revision: 2,
                action: app_proxy_windows::shortcuts::journal::Action::Create,
                expected_creation: None,
            },
        },
    ] {
        let policy = policy();
        let mut connection = ipc::connect(
            fixture.shared.identity.store_id,
            &policy,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
        greeting.protocol_minor = 15;
        connection.send(&greeting).await.unwrap();
        connection.receive::<Welcome>().await.unwrap();
        connection
            .send(&Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id: Uuid::new_v4(),
                operation,
            })
            .await
            .unwrap();
        let response: Response = connection.receive().await.unwrap();
        assert!(
            matches!(response.result, Reply::Error { code } if code == "SHORTCUT_PROTOCOL_UPDATE_REQUIRED")
        );
    }
    server.await.unwrap().unwrap();
    assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 2);
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
async fn runtime_status_requires_minor_fifteen_in_both_directions() {
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let old_server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
        greeting.protocol_minor = 14;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::RuntimeStatus {
                    instance_id: fixture.instance
                }
            )
            .await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    old_server.await.unwrap();
    let server = fixture.server();
    let policy = policy();
    let mut connection = ipc::connect(
        fixture.shared.identity.store_id,
        &policy,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
    greeting.protocol_minor = 14;
    connection.send(&greeting).await.unwrap();
    connection.receive::<Welcome>().await.unwrap();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::RuntimeStatus {
                instance_id: fixture.instance,
            },
        })
        .await
        .unwrap();
    let response: Response = connection.receive().await.unwrap();
    assert!(
        matches!(response.result, Reply::Error { code } if code == "RUNTIME_PROTOCOL_UPDATE_REQUIRED")
    );
    drop(connection);
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
async fn subscription_preview_and_nodes_require_their_minor_in_both_directions() {
    use crate::subscription_preview::{PreviewRequest, StageRequest};
    for operation in [
        Operation::SubscriptionNodes {
            profile_id: Uuid::new_v4(),
            offset: 0,
            expected_revision: None,
        },
        Operation::SubscriptionPreview {
            id: Uuid::new_v4(),
            request: PreviewRequest::Import {
                url: "http://127.0.0.1:1/private-token".into(),
                network: NetworkBinding::Direct {},
            },
        },
        Operation::SubscriptionPreviewPage {
            id: Uuid::new_v4(),
            offset: 0,
        },
        Operation::SubscriptionPreviewClose { id: Uuid::new_v4() },
        Operation::SubscriptionStage {
            preview_id: Uuid::new_v4(),
            stage_id: Uuid::new_v4(),
            request: StageRequest::Refresh {},
        },
    ] {
        let fixture = Fixture::new();
        let previous_minor = if matches!(&operation, Operation::SubscriptionNodes { .. }) {
            13
        } else {
            12
        };
        let bytes = serde_json::to_vec(&operation).unwrap();
        let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
        let identity = fixture.shared.identity.clone();
        let old_server = tokio::spawn(async move {
            let mut connection = listener.accept().await.unwrap();
            connection.receive::<Hello>().await.unwrap();
            let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
            greeting.protocol_minor = previous_minor;
            connection
                .send(&Welcome::Ready { hello: greeting })
                .await
                .unwrap();
            assert!(connection.receive::<Request>().await.is_err());
        });
        assert!(matches!(
            fixture.rpc(Uuid::new_v4(), operation).await,
            Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
        ));
        old_server.await.unwrap();
        let server = fixture.server();
        let policy = policy();
        let mut connection = ipc::connect(
            fixture.shared.identity.store_id,
            &policy,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
        greeting.protocol_minor = previous_minor;
        connection.send(&greeting).await.unwrap();
        connection.receive::<Welcome>().await.unwrap();
        connection
            .send(&Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id: Uuid::new_v4(),
                operation: serde_json::from_slice(&bytes).unwrap(),
            })
            .await
            .unwrap();
        let response: Response = connection.receive().await.unwrap();
        assert!(
            matches!(response.result, Reply::Error { code } if code == "SUBSCRIPTION_PROTOCOL_UPDATE_REQUIRED")
        );
        assert!(!fixture.shared.subscription.keeps_alive().unwrap());
        assert_eq!(fixture.shared.configuration.snapshot().unwrap().revision, 2);
        drop(connection);
        server.await.unwrap().unwrap();
    }
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
async fn guard_status_client_rejects_old_minor_before_sending_operation() {
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let status = fixture.shared.identity.clone();
    let server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(status.store_id, status.session_id, Some(status.epoch));
        greeting.protocol_minor = 8;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::GuardStatus {
                    instance_id: fixture.instance
                }
            )
            .await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    server.await.unwrap();
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
                        ifeo: ComponentState::NotApplicable,
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
async fn launch_client_rejects_old_minor_before_sending_operation() {
    for (minor, expected_revision) in [(6, None), (7, Some(1))] {
        let fixture = Fixture::new();
        let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
        let status = fixture.shared.identity.clone();
        let server = tokio::spawn(async move {
            let mut connection = listener.accept().await.unwrap();
            connection.receive::<Hello>().await.unwrap();
            let mut greeting = hello(status.store_id, status.session_id, Some(status.epoch));
            greeting.protocol_minor = minor;
            connection
                .send(&Welcome::Ready { hello: greeting })
                .await
                .unwrap();
            assert!(connection.receive::<Request>().await.is_err());
        });
        assert!(matches!(
            fixture
                .rpc(
                    Uuid::new_v4(),
                    Operation::Launch {
                        instance_id: fixture.instance,
                        origin: LaunchOrigin::Interactive,
                        expected_revision,
                    }
                )
                .await,
            Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
        ));
        server.await.unwrap();
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
}

#[tokio::test]
async fn guard_status_requires_current_minor_in_both_directions() {
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let old_server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
        greeting.protocol_minor = 9;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::GuardStatus {
                    instance_id: fixture.instance
                }
            )
            .await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    old_server.await.unwrap();

    let server = fixture.server();
    let policy = policy();
    let mut connection = ipc::connect(
        fixture.shared.identity.store_id,
        &policy,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
    greeting.protocol_minor = 9;
    connection.send(&greeting).await.unwrap();
    connection.receive::<Welcome>().await.unwrap();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::GuardStatus {
                instance_id: fixture.instance,
            },
        })
        .await
        .unwrap();
    let response: Response = connection.receive().await.unwrap();
    assert!(
        matches!(response.result, Reply::Error { code } if code == "GUARD_PROTOCOL_UPDATE_REQUIRED")
    );
    drop(connection);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn subscription_catalog_requires_minor_eleven_in_both_directions() {
    let fixture = Fixture::new();
    let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
    let identity = fixture.shared.identity.clone();
    let old_server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        connection.receive::<Hello>().await.unwrap();
        let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
        greeting.protocol_minor = 10;
        connection
            .send(&Welcome::Ready { hello: greeting })
            .await
            .unwrap();
        assert!(connection.receive::<Request>().await.is_err());
    });
    assert!(matches!(
        fixture
            .rpc(
                Uuid::new_v4(),
                Operation::Catalog {
                    offset: 0,
                    expected_revision: None
                }
            )
            .await,
        Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
    ));
    old_server.await.unwrap();
    let server = fixture.server();
    let policy = policy();
    let mut connection = ipc::connect(
        fixture.shared.identity.store_id,
        &policy,
        Duration::from_secs(1),
    )
    .await
    .unwrap();
    let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
    greeting.protocol_minor = 10;
    connection.send(&greeting).await.unwrap();
    connection.receive::<Welcome>().await.unwrap();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::Catalog {
                offset: 0,
                expected_revision: None,
            },
        })
        .await
        .unwrap();
    let response: Response = connection.receive().await.unwrap();
    assert!(
        matches!(response.result, Reply::Error { code } if code == "CATALOG_PROTOCOL_UPDATE_REQUIRED")
    );
    drop(connection);
    server.await.unwrap().unwrap();
}

#[tokio::test]
async fn subscription_edits_require_minor_twelve_before_either_admission() {
    for core in [false, true] {
        let fixture = Fixture::new();
        let operation = || {
            let edit = app_proxy_core::registry::SubscriptionEdit::Select {
                expected_source_revision: 1,
                node_id: Uuid::new_v4(),
            };
            if core {
                Operation::ControlCore {
                    action: CoreAction::PrepareSubscription {
                        expected_revision: 1,
                        profile_id: Uuid::new_v4(),
                        edit,
                    },
                }
            } else {
                Operation::Configure {
                    expected_revision: 1,
                    action: ConfigAction::EditSubscriptionProfile {
                        profile_id: Uuid::new_v4(),
                        edit,
                    },
                }
            }
        };
        let mut listener = ipc::Listener::bind(fixture.shared.identity.store_id, policy()).unwrap();
        let identity = fixture.shared.identity.clone();
        let old_server = tokio::spawn(async move {
            let mut connection = listener.accept().await.unwrap();
            connection.receive::<Hello>().await.unwrap();
            let mut greeting = hello(identity.store_id, identity.session_id, Some(identity.epoch));
            greeting.protocol_minor = 11;
            connection
                .send(&Welcome::Ready { hello: greeting })
                .await
                .unwrap();
            assert!(connection.receive::<Request>().await.is_err());
        });
        assert!(matches!(
            fixture.rpc(Uuid::new_v4(), operation()).await,
            Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"))
        ));
        old_server.await.unwrap();
        let server = fixture.server();
        let policy = policy();
        let mut connection = ipc::connect(
            fixture.shared.identity.store_id,
            &policy,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        let mut greeting = hello(fixture.shared.identity.store_id, policy.session_id, None);
        greeting.protocol_minor = 11;
        connection.send(&greeting).await.unwrap();
        connection.receive::<Welcome>().await.unwrap();
        let request_id = Uuid::new_v4();
        connection
            .send(&Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation: operation(),
            })
            .await
            .unwrap();
        let response: Response = connection.receive().await.unwrap();
        assert!(
            matches!(response.result, Reply::Error { code } if code == "SUBSCRIPTION_PROTOCOL_UPDATE_REQUIRED")
        );
        assert!(
            fixture
                .shared
                .configuration
                .lock()
                .unwrap()
                .config_request_status(request_id)
                .unwrap()
                .is_none()
        );
        assert!(
            fixture
                .shared
                .configuration
                .lock()
                .unwrap()
                .core_request_status(request_id)
                .unwrap()
                .is_none()
        );
        drop(connection);
        server.await.unwrap().unwrap();
    }
}
