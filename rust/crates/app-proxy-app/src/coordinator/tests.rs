use super::*;

/// Keeps a test from touching the per-user resource registry.
pub(super) fn test_resources(
    root: &Path,
) -> app_proxy_windows::instance_resource::ResourceRegistry {
    app_proxy_windows::instance_resource::ResourceRegistry::for_test_at(
        &root.join("test-resources"),
    )
    .unwrap()
}

fn snapshot() -> (tempfile::TempDir, Arc<Shared>) {
    let temp = tempfile::tempdir().unwrap();
    let store = store::Store::create(&temp.path().join("store")).unwrap();
    let id = store.load().unwrap().store_id;
    let current = identity::current().unwrap();
    let status = Status {
        store_id: id,
        revision: 1,
        epoch: Uuid::new_v4(),
        coordinator_pid: current.pid,
        session_id: current.session_id,
        applications: 0,
        instances: 0,
        profiles: 0,
        phase: "bootstrap".into(),
    };
    let root = temp.path().join("store");
    let shared = Arc::new(
        Shared::with_resources(root.clone(), store, status, test_resources(&root)).unwrap(),
    );
    (temp, shared)
}
fn policy() -> ipc::PeerPolicy {
    ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
}

async fn wait_core_result(shared: &Shared, id: Uuid) -> CoreRequestStatus {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(status) = shared.core.request_status(id).unwrap()
                && !matches!(status, CoreRequestStatus::Pending { .. })
            {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn long_installs_do_not_starve_status_cancellation_or_owner_idle_exit() {
    use app_proxy_core::core_control::CoreOutcome;
    let (_temp, shared) = snapshot();
    shared
        .core
        .hold_installer(Arc::new(tokio::sync::Notify::new()));
    let id = shared.identity.store_id;
    let listener = ipc::Listener::bind(id, policy()).unwrap();
    let owner = shared.clone();
    let server = tokio::spawn(serve_connections(
        listener,
        owner,
        Duration::from_millis(100),
    ));
    let mut installs = Vec::new();
    for _ in 0..MAX_CLIENTS {
        let request_id = Uuid::new_v4();
        installs.push(request_id);
        let reply = rpc(
            id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation: Operation::ControlCore {
                    action: CoreAction::Install {},
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            reply,
            Reply::CoreRequestStatus {
                status: Some(CoreRequestStatus::Pending { .. })
            }
        ));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(shared.jobs.load(Ordering::SeqCst), MAX_CLIENTS);
    assert!(!server.is_finished());
    query(id, &policy(), Duration::from_secs(1)).await.unwrap();
    for request_id in &installs {
        rpc(
            id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id: Uuid::new_v4(),
                operation: Operation::ControlCore {
                    action: CoreAction::CancelInstall {
                        request_id: *request_id,
                    },
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            wait_core_result(&shared, *request_id).await,
            CoreRequestStatus::Complete {
                outcome: CoreOutcome::Cancelled {},
                ..
            }
        ));
    }
    tokio::time::timeout(Duration::from_secs(3), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(shared.jobs.load(Ordering::SeqCst), 0);
    assert!(shared.idle_allowed().unwrap());
}

fn add_request(path: PathBuf) -> Request {
    use app_proxy_core::model::*;
    Request {
        protocol_major: PROTOCOL_MAJOR,
        request_id: Uuid::new_v4(),
        operation: Operation::Configure {
            expected_revision: 1,
            action: ConfigAction::AddApplication {
                application: Application {
                    id: Uuid::new_v4(),
                    revision: 1,
                    name: "fixture".into(),
                    locator: ApplicationLocator::Exe { path },
                    template_ref: Template::Codex,
                },
            },
        },
    }
}

#[tokio::test]
async fn core_lost_ack_still_completes_and_replay_cannot_change_action() {
    use app_proxy_core::core_control::CoreOutcome;
    let (_temp, shared) = snapshot();
    let id = shared.identity.store_id;
    let request_id = Uuid::new_v4();
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    let owner = shared.clone();
    let server = tokio::spawn(async move {
        let _ = handle(listener.accept().await.unwrap(), owner.clone()).await;
        (listener, owner)
    });
    let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    client
        .send(&hello(id, shared.identity.session_id, None))
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
            operation: Operation::ControlCore {
                action: CoreAction::Stop {},
            },
        })
        .await
        .unwrap();
    drop(client);
    let (mut listener, owner) = server.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), shared.job_finished.notified())
        .await
        .unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..3 {
            handle(listener.accept().await.unwrap(), owner.clone())
                .await
                .unwrap();
        }
    });
    let response = rpc(
        id,
        &policy(),
        Duration::from_secs(1),
        Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::CoreRequestStatus { request_id },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        response,
        Reply::CoreRequestStatus {
            status: Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Stopped {},
                ..
            })
        }
    ));
    let response = rpc(
        id,
        &policy(),
        Duration::from_secs(1),
        Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id,
            operation: Operation::ControlCore {
                action: CoreAction::Stop {},
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        response,
        Reply::CoreRequestStatus {
            status: Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Stopped {},
                ..
            })
        }
    ));
    let profile = Uuid::new_v4();
    let changed = rpc(
        id,
        &policy(),
        Duration::from_secs(1),
        Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id,
            operation: Operation::ControlCore {
                action: CoreAction::Start {
                    profiles: vec![profile],
                    required: profile,
                },
            },
        },
    )
    .await;
    assert!(matches!(
        changed,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn concurrent_core_requests_receive_one_durable_result() {
    let (_temp, shared) = snapshot();
    let id = shared.identity.store_id;
    let request_id = Uuid::new_v4();
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    let owner = shared.clone();
    let server = tokio::spawn(async move {
        let mut handlers = tokio::task::JoinSet::new();
        for _ in 0..6 {
            handlers.spawn(handle(listener.accept().await.unwrap(), owner.clone()));
        }
        while let Some(result) = handlers.join_next().await {
            result.unwrap().unwrap();
        }
    });
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..6 {
        clients.spawn(async move {
            rpc(
                id,
                &policy(),
                Duration::from_secs(1),
                Request {
                    protocol_major: PROTOCOL_MAJOR,
                    request_id,
                    operation: Operation::ControlCore {
                        action: CoreAction::Stop {},
                    },
                },
            )
            .await
            .unwrap()
        });
    }
    while let Some(response) = clients.join_next().await {
        assert!(matches!(
            response.unwrap(),
            Reply::CoreRequestStatus {
                status: Some(
                    CoreRequestStatus::Pending { .. } | CoreRequestStatus::Complete { .. }
                )
            }
        ));
    }
    server.await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), shared.job_finished.notified())
        .await
        .unwrap();
    assert!(matches!(
        shared.core.request_status(request_id).unwrap(),
        Some(CoreRequestStatus::Complete {
            outcome: app_proxy_core::core_control::CoreOutcome::Stopped {},
            ..
        })
    ));
    assert!(shared.idle_allowed().unwrap());
}

#[test]
fn delayed_status_snapshots_cannot_change_current_guard_idle_policy() {
    use app_proxy_core::{model::*, registry::*};
    let (temp, shared) = snapshot();
    let path = temp.path().join("fixture.exe");
    std::fs::write(&path, b"not executed").unwrap();
    shared.execute(add_request(path)).unwrap();
    let mut manifest = shared.configuration.snapshot().unwrap();
    let app_id = manifest.applications[0].id;
    let mut example: Manifest =
        serde_json::from_str(include_str!("../../../../examples/manifest.json")).unwrap();
    let profile = example.profiles.pop().unwrap();
    let profile_id = profile.id;
    let change = |revision, action| ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: revision,
        action,
    };
    assert!(matches!(
        shared
            .configuration
            .apply(&change(2, ConfigAction::AddProfile { profile }))
            .unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    let instance_id = Uuid::new_v4();
    assert!(matches!(
        shared
            .configuration
            .apply(&change(
                3,
                ConfigAction::CreateInstance {
                    instance: NewInstance {
                        id: instance_id,
                        application_id: app_id,
                        name: "guarded".into(),
                        data: NewData::Original {},
                        network: NetworkBinding::Profile { profile_id },
                        guard: None,
                        args: vec![],
                        env: SavedEnvironment::default(),
                        cwd: WorkingDirectory::Application {}
                    }
                }
            ))
            .unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    shared.update(&manifest); // Delayed snapshot from before enabling Guard.
    assert!(!shared.idle_allowed().unwrap());
    manifest = shared.configuration.snapshot().unwrap();
    assert!(matches!(
        shared
            .configuration
            .apply(&change(
                4,
                ConfigAction::BindInstance {
                    instance_id,
                    network: NetworkBinding::Direct {},
                    guard: None
                }
            ))
            .unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    shared.update(&manifest); // Delayed snapshot from before disabling Guard.
    assert!(shared.idle_allowed().unwrap());
}

#[tokio::test]
async fn concurrent_pipe_replays_commit_once_and_status_reflects_the_edit() {
    let (temp, shared) = snapshot();
    let path = temp.path().join("fixture.exe");
    std::fs::write(&path, b"not executed").unwrap();
    let id = shared.identity.store_id;
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    let owner = shared.clone();
    let server = tokio::spawn(async move {
        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..7 {
            let pipe = listener.accept().await.unwrap();
            clients.spawn(handle(pipe, owner.clone()));
        }
        while let Some(result) = clients.join_next().await {
            result.unwrap().unwrap();
        }
    });
    let request = add_request(path);
    let bytes = serde_json::to_vec(&request).unwrap();
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..6 {
        let request = serde_json::from_slice(&bytes).unwrap();
        clients.spawn(async move {
            rpc(id, &policy(), Duration::from_secs(1), request)
                .await
                .unwrap()
        });
    }
    let mut entity = None;
    while let Some(result) = clients.join_next().await {
        let Reply::Configured {
            outcome: ConfigOutcome::Applied { receipt },
        } = result.unwrap()
        else {
            panic!("request rejected")
        };
        assert_eq!(receipt.revision, 2);
        if let Some(previous) = entity {
            assert_eq!(receipt.entity_id, previous);
        }
        entity = Some(receipt.entity_id);
    }
    let status = query(id, &policy(), Duration::from_secs(1)).await.unwrap();
    assert_eq!(status.revision, 2);
    assert_eq!(status.applications, 1);
    server.await.unwrap();
}

#[tokio::test]
async fn lost_response_can_be_queried_and_changed_payload_cannot_reuse_id() {
    let (temp, shared) = snapshot();
    let path = temp.path().join("fixture.exe");
    std::fs::write(&path, b"not executed").unwrap();
    let id = shared.identity.store_id;
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    let owner = shared.clone();
    let request = add_request(path.clone());
    let request_id = request.request_id;
    let server = tokio::spawn(async move {
        let _ = handle(listener.accept().await.unwrap(), owner.clone()).await;
        (listener, owner)
    });
    let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    client
        .send(&hello(id, shared.identity.session_id, None))
        .await
        .unwrap();
    assert!(matches!(
        client.receive::<Welcome>().await.unwrap(),
        Welcome::Ready { .. }
    ));
    client.send(&request).await.unwrap();
    // Intentionally discard the response. Closing a client does not undo its
    // accepted write; the durable receipt is the authority on reconnect.
    drop(client);
    let (mut listener, owner) = server.await.unwrap();
    let server = tokio::spawn(async move {
        for _ in 0..2 {
            handle(listener.accept().await.unwrap(), owner.clone())
                .await
                .unwrap();
        }
    });
    let lookup = Request {
        protocol_major: PROTOCOL_MAJOR,
        request_id: Uuid::new_v4(),
        operation: Operation::RequestStatus { request_id },
    };
    assert!(matches!(
        rpc(id, &policy(), Duration::from_secs(1), lookup)
            .await
            .unwrap(),
        Reply::RequestStatus {
            status: Some(ConfigRequestStatus::Complete {
                outcome: ConfigOutcome::Applied { .. }
            })
        }
    ));
    let mut changed = add_request(path);
    changed.request_id = request_id;
    assert!(matches!(
        rpc(id, &policy(), Duration::from_secs(1), changed).await,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    server.await.unwrap();
    assert_eq!(shared.configuration.snapshot().unwrap().revision, 2);
}

#[test]
fn remote_errors_round_trip_without_a_client_side_code_list() {
    // Both codes were raised by the server but missing from the old list.
    for code in ["INVALID_LAUNCH_REQUEST", "CORE_OPERATION_LIMIT"] {
        let sent = safe_error(Error::Invalid(code));
        assert!(matches!(remote_error(&sent), Error::Invalid(found) if found == code));
    }
    let sent = safe_error(Error::Windows {
        operation: "ShortcutCom",
        code: 5,
    });
    assert!(matches!(
        remote_error(&sent),
        Error::Windows {
            operation: "ShortcutCom",
            code: 5
        }
    ));
    for text in ["", "not a code", "SHORTCUT_COM_ERROR:x", "C:\\secret\\path"] {
        assert!(matches!(
            remote_error(text),
            Error::Invalid("COORDINATOR_OPERATION_FAILED")
        ));
    }
}

#[tokio::test]
async fn handshake_rejects_major_store_session_and_client_epoch() {
    for expected in [
        "PROTOCOL_VERSION_MISMATCH",
        "STORE_ID_MISMATCH",
        "STORE_SESSION_CONFLICT",
        "INVALID_CLIENT_HELLO",
    ] {
        let (_temp, status) = snapshot();
        let id = status.identity.store_id;
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let mut request = hello(id, status.identity.session_id, None);
        match expected {
            "PROTOCOL_VERSION_MISMATCH" => request.protocol_major = 2,
            "STORE_ID_MISMATCH" => request.store_id = Uuid::new_v4(),
            "STORE_SESSION_CONFLICT" => request.session_id += 1,
            _ => request.epoch = Some(Uuid::new_v4()),
        }
        let task = tokio::spawn(async move {
            handle(listener.accept().await.unwrap(), status)
                .await
                .unwrap()
        });
        let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
            .await
            .unwrap();
        client.send(&request).await.unwrap();
        match client.receive::<Welcome>().await.unwrap() {
            Welcome::Rejected { code } => assert_eq!(code, expected),
            _ => panic!("invalid hello accepted"),
        }
        task.await.unwrap();
    }
}

#[tokio::test]
async fn stalled_client_does_not_block_another_status_request() {
    let (_temp, status) = snapshot();
    let id = status.identity.store_id;
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    let server = tokio::spawn(async move {
        let stalled = listener.accept().await.unwrap();
        let stalled_task = tokio::spawn(handle(stalled, status.clone()));
        handle(listener.accept().await.unwrap(), status)
            .await
            .unwrap();
        stalled_task.abort();
        let _ = stalled_task.await;
    });
    let _stalled = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        query(id, &policy(), Duration::from_secs(1)),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.store_id, id);
    server.await.unwrap();
}
