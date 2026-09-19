use super::*;
use app_proxy_core::model::*;
use app_proxy_windows::{etw::ProcessStartHint, store::Store};

fn fixture() -> (tempfile::TempDir, Arc<Monitor>) {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::create(&root.path().join("store")).unwrap();
    let mut manifest = store.load().unwrap();
    let application = Uuid::new_v4();
    let profile = Uuid::new_v4();
    let node = Uuid::new_v4();
    manifest.applications.push(Application {
        id: application,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: root.path().join("never-executed.exe"),
        },
        template_ref: Template::Codex,
    });
    manifest.profiles.push(ProxyProfile {
        id: profile,
        name: "fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 19321,
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
    manifest.instances.push(Instance {
        id: Uuid::new_v4(),
        application_id: application,
        name: "fixture".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec![],
        env: SavedEnvironment::default(),
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Profile {
            profile_id: profile,
        },
        guard: GuardConfig {
            desired: Desired::Enabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    store.commit(manifest.revision, manifest).unwrap();
    let configuration = Arc::new(Configuration::new(store));
    let manager = Arc::new(crate::core_manager::CoreManager::new(
        root.path().join("store"),
        configuration.clone(),
    ));
    let resources = app_proxy_windows::instance_resource::ResourceRegistry::for_test_at(
        &root.path().join("resources"),
    )
    .unwrap();
    let launch =
        LaunchEngine::with_resources(configuration.clone(), manager, Uuid::new_v4(), resources)
            .unwrap();
    (root, Monitor::new(configuration, launch))
}

async fn until(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn missing_deployment_never_runs_or_authorizes_and_disable_stops_service() {
    let (_root, monitor) = fixture();
    let service = monitor.start().unwrap();
    assert!(monitor.start().is_err());
    until(|| monitor.snapshot().phase == Phase::NeedsAuthorization).await;
    let snapshot = monitor.snapshot();
    assert!(snapshot.generation.is_none());
    assert!(monitor.state.lock().unwrap().authorization.is_none());
    assert_eq!(
        snapshot.diagnostic.as_deref(),
        Some("GUARD_LISTENER_MISSING")
    );
    {
        let mut store = monitor.configuration.lock().unwrap();
        let mut manifest = store.load().unwrap();
        manifest.instances[0].guard.desired = Desired::Disabled;
        store.commit(manifest.revision, manifest).unwrap();
    }
    until(|| monitor.snapshot().phase == Phase::Disabled).await;
    drop(service);
    assert!(monitor.state.lock().unwrap().owner.is_none());
    assert!(
        monitor
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn stopped_service_revokes_snapshot_and_old_publish_cannot_change_successor() {
    let (_root, monitor) = fixture();
    let old = monitor.start().unwrap();
    let epoch = old.epoch;
    monitor.publish(epoch, Phase::Starting, None, None);
    drop(old);
    assert!(monitor.snapshot().phase == Phase::Disabled);
    let next = monitor.start().unwrap();
    monitor.publish(next.epoch, Phase::Starting, None, None);
    monitor.publish(epoch, Phase::Polling, None, Some("stale".into()));
    assert!(monitor.snapshot().phase == Phase::Starting);
    drop(next);
    monitor.publish(epoch, Phase::Etw, None, None);
    assert!(monitor.snapshot().phase == Phase::Disabled);
}

#[tokio::test]
async fn timed_out_or_cancelled_native_work_keeps_slot_until_actual_return() {
    let (_root, monitor) = fixture();
    for cancel in [false, true] {
        let (release, waiting) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let owner = monitor.clone();
        let task = tokio::spawn(async move {
            owner
                .native_work(
                    if cancel {
                        Duration::from_secs(3)
                    } else {
                        Duration::from_millis(20)
                    },
                    move || {
                        entered.send(()).unwrap();
                        let _ = waiting.recv();
                        Ok(())
                    },
                )
                .await
        });
        started.await.unwrap();
        if cancel {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert!(matches!(
                task.await.unwrap(),
                Err(Error::Invalid("GUARD_LISTENER_PREPARE_TIMEOUT"))
            ));
        }
        assert!(matches!(
            monitor.native_work(Duration::from_secs(1), || Ok(())).await,
            Err(Error::Invalid("GUARD_LISTENER_WORKER_BUSY"))
        ));
        release.send(()).unwrap();
        until(|| monitor.native.available_permits() == 1).await;
    }
}

#[test]
fn only_received_batches_mark_etw_and_loss_hints_end_request_rescan() {
    let mut snapshot = Snapshot::new(Phase::Starting, None);
    let mut batch = EventBatch {
        epoch: Uuid::new_v4(),
        sequence: 1,
        hints: vec![],
        full_scan_required: true,
        dropped: 0,
        decode_failures: 0,
        etw_events_lost: 0,
        etw_buffers_lost: 0,
        ended: None,
    };
    assert!(snapshot.observe(&batch));
    assert!(snapshot.phase == Phase::Etw);
    assert_eq!(snapshot.epoch, Some(batch.epoch));
    batch.sequence += 1;
    batch.full_scan_required = false;
    assert!(!snapshot.observe(&batch));
    batch.hints.push(ProcessStartHint {
        pid: 123,
        image_name: "untrusted-hint.exe".into(),
        event_time: 1,
    });
    assert!(snapshot.observe(&batch));
    batch.hints.clear();
    batch.ended = Some(5);
    let mut failed = Snapshot::new(Phase::Starting, None);
    assert!(failed.observe(&batch));
    assert!(failed.phase == Phase::Polling);
    let mut ended = snapshot.clone();
    assert!(ended.observe(&batch));
    assert!(ended.phase == Phase::Polling);
    assert_eq!(
        ended.diagnostic.as_deref(),
        Some("GUARD_EVENT_STREAM_ENDED: 5")
    );
    let (_root, monitor) = fixture();
    snapshot.received = Some(Instant::now() - Duration::from_secs(6));
    monitor.state.lock().unwrap().snapshot = snapshot;
    assert!(monitor.snapshot().phase == Phase::Polling);
}

#[tokio::test]
async fn revoked_preparation_never_dispatches_after_blocked_validation() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (_root, monitor) = fixture();
    // Timeout, async cancellation, and service replacement all revoke a worker
    // which has not reached the system-call boundary yet.
    for mode in 0..3 {
        let epoch = Uuid::new_v4();
        monitor.state.lock().unwrap().owner = Some(epoch);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let (release, waiting) = std::sync::mpsc::channel();
        let (entered, started) = tokio::sync::oneshot::channel();
        let owner = monitor.clone();
        let task = tokio::spawn(async move {
            let timeout = if mode == 0 {
                Duration::from_millis(30)
            } else {
                Duration::from_secs(3)
            };
            let preparation = Preparation::new(timeout);
            let pending = preparation.pending.clone();
            let deadline = preparation.deadline;
            let worker_owner = owner.clone();
            owner
                .native_work(timeout, move || {
                    entered.send(()).unwrap();
                    waiting.recv().unwrap();
                    worker_owner.claim_dispatch(epoch, &pending, deadline)?;
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
        });
        started.await.unwrap();
        match mode {
            0 => assert!(matches!(
                task.await.unwrap(),
                Err(Error::Invalid("GUARD_LISTENER_PREPARE_TIMEOUT"))
            )),
            1 => {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            }
            _ => {
                monitor.state.lock().unwrap().owner = Some(Uuid::new_v4());
                release.send(()).unwrap();
                assert!(matches!(
                    task.await.unwrap(),
                    Err(Error::Invalid("GUARD_LISTENER_PREPARATION_REVOKED"))
                ));
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                continue;
            }
        }
        release.send(()).unwrap();
        until(|| monitor.native.available_permits() == 1).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
    let epoch = Uuid::new_v4();
    monitor.state.lock().unwrap().owner = Some(epoch);
    let attempt = Preparation::new(Duration::from_secs(1));
    assert!(
        monitor
            .claim_dispatch(epoch, &attempt.pending, attempt.deadline)
            .is_ok()
    );
    assert!(
        monitor
            .claim_dispatch(epoch, &attempt.pending, attempt.deadline)
            .is_err()
    );
    // Check the absolute deadline even if the async timeout has not been polled.
    let expired = Preparation::new(Duration::ZERO);
    assert!(
        monitor
            .claim_dispatch(epoch, &expired.pending, expired.deadline)
            .is_err()
    );
}

#[tokio::test]
async fn owned_child_tasks_are_cancelled_instead_of_detached_on_scope_exit() {
    struct Dropped(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            let _ = self.0.take().unwrap().send(());
        }
    }
    let (sender, dropped) = tokio::sync::oneshot::channel();
    let (ready, started) = tokio::sync::oneshot::channel();
    let task = Task(tokio::spawn(async move {
        let _guard = Dropped(Some(sender));
        ready.send(()).unwrap();
        std::future::pending::<()>().await;
    }));
    started.await.unwrap();
    drop(task);
    tokio::time::timeout(Duration::from_secs(1), dropped)
        .await
        .unwrap()
        .unwrap();
}
