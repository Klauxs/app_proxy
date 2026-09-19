use super::*;
use app_proxy_core::{launch::LaunchPhase, model::*};

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
                let Reply::LaunchStatus {
                    attempt: Some(attempt),
                } = self
                    .rpc(Uuid::new_v4(), Operation::LaunchStatus { request_id: id })
                    .await
                    .unwrap()
                else {
                    panic!("missing attempt")
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
