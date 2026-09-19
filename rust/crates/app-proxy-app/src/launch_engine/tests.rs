use super::*;
use app_proxy_windows::store::Store;
use std::{
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

fn hold_replacement(path: &Path) -> std::fs::File {
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1 | 2)
        .open(path)
        .unwrap()
}

#[tokio::test]
async fn continuation_revision_is_fixed_at_admission_and_rechecked_at_dispatch() {
    let fixture = Fixture::new(true);
    let revision = fixture.engine.configuration.snapshot().unwrap().revision;
    let stale = fixture.request();
    assert!(matches!(
        fixture
            .engine
            .submit_at_revision(stale.clone(), Some(revision - 1))
            .await,
        Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"))
    ));
    assert!(fixture.engine.status(stale.request_id).unwrap().is_none());
    let gate = Arc::new(tokio::sync::Notify::new());
    fixture.engine.hold_dispatch(gate.clone());
    let guarded = fixture.request();
    fixture
        .engine
        .submit_at_revision(guarded.clone(), Some(revision))
        .await
        .unwrap();
    fixture.ready(guarded.request_id).await;
    assert_eq!(
        fixture
            .engine
            .submit(guarded.clone())
            .await
            .unwrap()
            .expected_revision,
        Some(revision)
    );
    // Even display-only edits, normally allowed by dependency_digest, invalidate
    // a foreground continuation's explicit revision precondition.
    fixture.edit(|m| m.instances[0].name = "changed during foreground repair".into());
    gate.notify_one();
    let failed = fixture.result(guarded.request_id).await;
    assert!(
        matches!(failed.phase, LaunchPhase::Failed { code } if code == "LAUNCH_CONFIG_CHANGED")
    );
    assert!(failed.dispatch_id.is_none());
    assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
    let plain = fixture.request();
    fixture.engine.submit(plain.clone()).await.unwrap();
    fixture.ready(plain.request_id).await;
    let guarded = fixture.request();
    let current = fixture.engine.configuration.snapshot().unwrap().revision;
    assert!(matches!(
        fixture
            .engine
            .submit_at_revision(guarded.clone(), Some(current))
            .await,
        Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"))
    ));
    assert!(fixture.engine.status(guarded.request_id).unwrap().is_none());
    fixture.engine.cancel(plain.request_id).unwrap();
    gate.notify_one();
    assert!(matches!(
        fixture.result(plain.request_id).await.phase,
        LaunchPhase::Cancelled {}
    ));
}

#[tokio::test]
async fn interrupted_reserved_preparation_is_reclaimed_after_its_local_history_expires() {
    for coordinator_restart in [true, false] {
        let fixture = Fixture::new(true);
        let request = fixture.request();
        {
            let mut store = fixture.engine.configuration.lock().unwrap();
            store.begin_launch(&request, fixture.engine.epoch).unwrap();
            store
                .advance_launch(
                    request.request_id,
                    fixture.engine.epoch,
                    &LaunchPhase::Accepted {},
                    LaunchPhase::Resolving {},
                )
                .unwrap();
            store
                .advance_launch(
                    request.request_id,
                    fixture.engine.epoch,
                    &LaunchPhase::Resolving {},
                    LaunchPhase::CheckingInstance {},
                )
                .unwrap();
            let application = installation::resolve(&ApplicationLocator::Exe {
                path: fixture.exe.clone(),
            })
            .unwrap();
            let mut reserved = fixture
                .engine
                .resources
                .acquire(InstanceResource::resolve(&application, None).unwrap())
                .unwrap();
            reserved
                .reserve(ResourceOwner {
                    store_id: store.load().unwrap().store_id,
                    attempt_id: request.request_id,
                    epoch: fixture.engine.epoch,
                })
                .unwrap();
            drop(reserved); // Model owner death before any global dispatch intent.
            if coordinator_restart {
                store.recover_launches(Uuid::new_v4()).unwrap();
            }
        }
        let result = fixture.engine.status(request.request_id).unwrap().unwrap();
        assert!(
            matches!(result.phase, LaunchPhase::Failed { code } if code == if coordinator_restart { "LAUNCH_INTERRUPTED_BEFORE_SPAWN" } else { "LAUNCH_EXECUTION_INTERRUPTED" })
        );
        let path = fixture._root.path().join("store/state/launch.json");
        let mut journal: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        journal["attempts"][0]["accepted_at"] = 1.into();
        journal["attempts"][0]["finished_at"] = 1.into();
        std::fs::write(&path, serde_json::to_vec(&journal).unwrap()).unwrap();
        let next = fixture.request();
        fixture.engine.submit(next.clone()).await.unwrap();
        confirmed(fixture.result(next.request_id).await);
        assert!(
            fixture
                .engine
                .configuration
                .lock()
                .unwrap()
                .launch_request(request.request_id)
                .unwrap()
                .is_none()
        );
        fixture.events(1).await;
    }
}

#[tokio::test]
async fn unacknowledged_confirmation_survives_exit_and_another_store_until_owner_recovery() {
    let mut first = Fixture::new(true);
    let mut second = Fixture::new(false);
    second.exe = first.exe.clone();
    second.edit(|m| {
        m.applications[0].locator = ApplicationLocator::Exe {
            path: first.exe.clone(),
        }
    });
    second.engine = LaunchEngine::with_resources(
        second.engine.configuration.clone(),
        second.engine.manager.clone(),
        Uuid::new_v4(),
        first.engine.resources.clone(),
    )
    .unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = first._root.path().join("store/state/launch.json");
    *first.engine.after_spawn.lock().unwrap() = Some(Arc::new(move || {
        sender.send(hold_replacement(&path)).unwrap();
    }));
    let request = first.request();
    first.engine.submit(request.clone()).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while first
        .engine
        .active
        .lock()
        .unwrap()
        .contains(&request.request_id)
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let held = receiver.try_recv().unwrap();
    let app = installation::resolve(&ApplicationLocator::Exe {
        path: first.exe.clone(),
    })
    .unwrap();
    let reservation = first
        .engine
        .resources
        .acquire(InstanceResource::resolve(&app, None).unwrap())
        .unwrap();
    let ResourcePhase::Confirmed { process } = &reservation.claim().unwrap().phase else {
        panic!("missing global receipt")
    };
    let process = process.clone();
    process::terminate_exact(&process).unwrap();
    drop(reservation);
    let conflict = second.request();
    second.engine.submit(conflict.clone()).await.unwrap();
    assert!(
        matches!(second.result(conflict.request_id).await.phase, LaunchPhase::Failed { code } if code == "INSTANCE_RESOURCE_RECOVERY_REQUIRED")
    );
    drop(held);
    // Reopen the protected store under a new coordinator epoch, without a live
    // in-memory engine or current process from which to infer the lost receipt.
    let registry = first.engine.resources.clone();
    let placeholder = Fixture::new(false);
    first.engine = placeholder.engine.clone();
    let home = first._root.path().join("store");
    let configuration = Arc::new(Configuration::new(Store::open(&home).unwrap()));
    let manager = Arc::new(CoreManager::new(home, configuration.clone()));
    first.engine =
        LaunchEngine::with_resources(configuration, manager, Uuid::new_v4(), registry).unwrap();
    let recovered = first.engine.status(request.request_id).unwrap().unwrap();
    assert!(recovered.session_exited && !recovered.resource_pending);
    assert_eq!(confirmed(recovered), process);
    let next = second.request();
    second.engine.submit(next.clone()).await.unwrap();
    let running = confirmed(second.result(next.request_id).await);
    process::terminate_exact(&running).unwrap();
}

#[tokio::test]
async fn failed_preparation_release_preserves_evidence_and_replays_during_unrelated_pending_edit() {
    use app_proxy_core::registry::{ConfigAction, ConfigRequest};
    let fixture = Fixture::new(true);
    let gate = Arc::new(tokio::sync::Notify::new());
    *fixture.engine.before_dispatch.lock().unwrap() = Some(gate.clone());
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    fixture.ready(request.request_id).await;
    let raw = fixture.engine.status(request.request_id).unwrap().unwrap();
    let name: String = raw
        .binding
        .unwrap()
        .resource_key
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let held = hold_replacement(
        &fixture
            ._root
            .path()
            .join("resources")
            .join(format!("{name}.json")),
    );
    fixture.engine.cancel(request.request_id).unwrap();
    gate.notify_one();
    let deadline = Instant::now() + Duration::from_secs(15);
    while fixture
        .engine
        .active
        .lock()
        .unwrap()
        .contains(&request.request_id)
    {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let failed = fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .launch_request(request.request_id)
        .unwrap()
        .unwrap();
    assert!(failed.resource_pending);
    assert!(
        matches!(failed.phase, LaunchPhase::Failed { code } if code == "INSTANCE_RESOURCE_RELEASE_FAILED")
    );
    let manifest_lock = hold_replacement(&fixture._root.path().join("store/manifest.json"));
    let change = ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: fixture.engine.configuration.snapshot().unwrap().revision,
        action: ConfigAction::RenameInstance {
            instance_id: fixture.instance,
            name: "display only".into(),
        },
    };
    assert!(fixture.engine.configuration.apply(&change).is_err());
    assert_eq!(
        fixture.engine.submit(request.clone()).await.unwrap().id,
        request.request_id
    );
    let mut conflict = request.clone();
    conflict.origin = LaunchOrigin::Guard;
    assert!(matches!(
        fixture.engine.submit(conflict).await,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    drop(manifest_lock);
    drop(held);
    assert!(
        !fixture
            .engine
            .status(request.request_id)
            .unwrap()
            .unwrap()
            .resource_pending
    );
    *fixture.engine.before_dispatch.lock().unwrap() = None;
    let next = fixture.request();
    fixture.engine.submit(next.clone()).await.unwrap();
    confirmed(fixture.result(next.request_id).await);
    fixture.events(1).await;
}

#[tokio::test]
async fn accepted_pending_configuration_is_recovered_before_dispatch_or_blocks_creation() {
    use app_proxy_core::registry::{ConfigAction, ConfigRequest};
    for release_before_dispatch in [true, false] {
        let fixture = Fixture::new(true);
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.engine.before_dispatch.lock().unwrap() = Some(gate.clone());
        let request = fixture.request();
        fixture.engine.submit(request.clone()).await.unwrap();
        fixture.ready(request.request_id).await;
        // Accepted removal also requires recovery before resolving the instance.
        let change = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: fixture.engine.configuration.snapshot().unwrap().revision,
            action: ConfigAction::RemoveInstance {
                instance_id: fixture.instance,
            },
        };
        let mut held = Some(hold_replacement(
            &fixture._root.path().join("store/manifest.json"),
        ));
        assert!(fixture.engine.configuration.apply(&change).is_err());
        if release_before_dispatch {
            drop(held.take());
        }
        gate.notify_one();
        let result = fixture.result(request.request_id).await;
        assert!(matches!(result.phase, LaunchPhase::Failed { .. }));
        assert!(result.dispatch_id.is_none());
        assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
        drop(held);
        fixture
            .engine
            .configuration
            .request_status(change.request_id)
            .unwrap();
        assert!(
            fixture
                .engine
                .configuration
                .snapshot()
                .unwrap()
                .instances
                .is_empty()
        );
    }
}

#[tokio::test]
async fn interrupted_completion_recovers_both_journals_without_replaying_creation() {
    for created in [true, false] {
        let fixture = Fixture::new(created);
        let (sender, receiver) = std::sync::mpsc::channel();
        let configuration = fixture.engine.configuration.clone();
        let root = fixture._root.path().to_owned();
        *fixture.engine.after_spawn.lock().unwrap() = Some(Arc::new(move || {
            let path = if created {
                root.join("store/state/launch.json")
            } else {
                let binding = configuration
                    .lock()
                    .unwrap()
                    .launch_attempts()
                    .unwrap()
                    .pop()
                    .unwrap()
                    .binding
                    .unwrap();
                let name: String = binding
                    .resource_key
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect();
                root.join("resources").join(format!("{name}.json"))
            };
            sender.send(hold_replacement(&path)).unwrap();
        }));
        let request = fixture.request();
        fixture.engine.submit(request.clone()).await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while fixture
            .engine
            .active
            .lock()
            .unwrap()
            .contains(&request.request_id)
        {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let held = receiver.try_recv().unwrap();
        let raw = fixture
            .engine
            .configuration
            .lock()
            .unwrap()
            .launch_request(request.request_id)
            .unwrap()
            .unwrap();
        assert!(raw.resource_pending);
        if created {
            assert!(matches!(raw.phase, LaunchPhase::SpawnRequested {}));
            fixture.events(1).await;
        } else {
            assert!(
                matches!(raw.phase, LaunchPhase::Failed {ref code} if code == "APPLICATION_NOT_CREATED")
            );
        }
        assert!(fixture.engine.status(request.request_id).is_err());
        drop(held);
        let recovered = fixture.engine.status(request.request_id).unwrap().unwrap();
        assert!(!recovered.resource_pending);
        if created {
            let running = confirmed(recovered);
            assert!(process::is_running_exact(&running).unwrap());
            assert_eq!(
                fixture.engine.submit(fixture.request()).await.unwrap().id,
                request.request_id
            );
            fixture.events(1).await;
            process::terminate_exact(&running).unwrap();
        } else {
            *fixture.engine.after_spawn.lock().unwrap() = None;
            let next = fixture.request();
            fixture.engine.submit(next.clone()).await.unwrap();
            assert!(
                matches!(fixture.result(next.request_id).await.phase, LaunchPhase::Failed { code } if code == "APPLICATION_NOT_CREATED")
            );
            assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
        }
    }
}

struct Fixture {
    engine: Arc<LaunchEngine>,
    instance: Uuid,
    exe: PathBuf,
    events: PathBuf,
    _root: tempfile::TempDir,
}
impl Fixture {
    fn new(valid: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let exe = root
            .path()
            .join(format!("launch-fixture-{}.exe", Uuid::new_v4()));
        if valid {
            std::fs::copy(std::env::current_exe().unwrap(), &exe).unwrap();
        } else {
            std::fs::write(&exe, b"invalid PE fixture").unwrap();
        }
        let events = root.path().join("events");
        std::fs::create_dir(&events).unwrap();
        let home = root.path().join("store");
        let mut store = Store::create(&home).unwrap();
        let mut manifest = store.load().unwrap();
        let app = Uuid::new_v4();
        let instance = Uuid::new_v4();
        manifest.applications.push(Application {
            id: app,
            name: "fixture".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path: exe.clone() },
            template_ref: Template::Environment,
        });
        manifest.instances.push(Instance {
            id: instance,
            application_id: app,
            name: "fixture".into(),
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
                            value: "private fixture value 引号".into(),
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
        let configuration = Arc::new(Configuration::new(store));
        let manager = Arc::new(CoreManager::new(home, configuration.clone()));
        let registry = ResourceRegistry::for_test_at(&root.path().join("resources")).unwrap();
        let engine =
            LaunchEngine::with_resources(configuration, manager, Uuid::new_v4(), registry).unwrap();
        Self {
            engine,
            instance,
            exe,
            events,
            _root: root,
        }
    }
    fn request(&self) -> LaunchRequest {
        LaunchRequest {
            request_id: Uuid::new_v4(),
            instance_id: self.instance,
            origin: LaunchOrigin::Interactive,
        }
    }
    fn edit(&self, change: impl FnOnce(&mut Manifest)) {
        let mut store = self.engine.configuration.lock().unwrap();
        let mut manifest = store.load().unwrap();
        change(&mut manifest);
        store.commit(manifest.revision, manifest).unwrap();
    }
    async fn result(&self, id: Uuid) -> LaunchAttempt {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let result = self.engine.status(id).unwrap().unwrap();
            if (result.finished_at.is_some()
                || matches!(result.phase, LaunchPhase::Indeterminate {}))
                && !self.engine.active.lock().unwrap().contains(&result.id)
            {
                return result;
            }
            assert!(
                Instant::now() < deadline,
                "launch did not finish: {:?}",
                result.phase
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn ready(&self, id: Uuid) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = self.engine.status(id).unwrap().unwrap();
            if matches!(status.phase, LaunchPhase::ReadyToSpawn {}) {
                return;
            }
            assert!(
                status.phase.before_spawn() && Instant::now() < deadline,
                "unexpected preparation: {:?}",
                status.phase
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
    async fn events(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::fs::read_dir(&self.events).unwrap().count() != count {
            assert!(Instant::now() < deadline, "unexpected creation count");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(store) = self.engine.configuration.lock() {
            for attempt in store.launch_attempts().unwrap_or_default() {
                if let LaunchPhase::Confirmed { process } = attempt.phase
                    && process.image_file == identity::file_identity(&self.exe).unwrap()
                {
                    let _ = process::terminate_exact(&process);
                }
            }
        }
    }
}
fn confirmed(attempt: LaunchAttempt) -> app_proxy_core::ProcessIdentity {
    match attempt.phase {
        LaunchPhase::Confirmed { process } => process,
        other => panic!("expected confirmation: {other:?}"),
    }
}

#[tokio::test]
async fn separate_stores_share_one_reservation_before_and_after_confirmation() {
    let first = Fixture::new(true);
    let mut second = Fixture::new(true);
    second.exe = first.exe.clone();
    second.edit(|m| {
        m.applications[0].locator = ApplicationLocator::Exe {
            path: first.exe.clone(),
        }
    });
    second.engine = LaunchEngine::with_resources(
        second.engine.configuration.clone(),
        second.engine.manager.clone(),
        Uuid::new_v4(),
        first.engine.resources.clone(),
    )
    .unwrap();
    let gate = Arc::new(tokio::sync::Notify::new());
    *first.engine.before_dispatch.lock().unwrap() = Some(gate.clone());
    let request = first.request();
    first.engine.submit(request.clone()).await.unwrap();
    first.ready(request.request_id).await;
    let conflict = second.request();
    second.engine.submit(conflict.clone()).await.unwrap();
    assert!(
        matches!(second.result(conflict.request_id).await.phase,LaunchPhase::Failed {code} if code=="INSTANCE_RESOURCE_BUSY")
    );
    gate.notify_one();
    let running = confirmed(first.result(request.request_id).await);
    let conflict = second.request();
    second.engine.submit(conflict.clone()).await.unwrap();
    assert!(
        matches!(second.result(conflict.request_id).await.phase,LaunchPhase::Failed {code} if code=="INSTANCE_STILL_RUNNING")
    );
    assert!(process::is_running_exact(&running).unwrap());
    process::terminate_exact(&running).unwrap();
    let next = second.request();
    second.engine.submit(next.clone()).await.unwrap();
    let running = confirmed(second.result(next.request_id).await);
    second.events(1).await;
    process::terminate_exact(&running).unwrap();
}

#[tokio::test]
async fn dropping_and_reopening_owner_preserves_application_and_historical_receipt() {
    let mut fixture = Fixture::new(true);
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let process = confirmed(fixture.result(request.request_id).await);
    let registry = fixture.engine.resources.clone();
    let weak = Arc::downgrade(&fixture.engine);
    let placeholder = Fixture::new(false);
    fixture.engine = placeholder.engine.clone();
    let deadline = Instant::now() + Duration::from_secs(2);
    while weak.upgrade().is_some() {
        assert!(Instant::now() < deadline);
        tokio::task::yield_now().await;
    }
    assert!(process::is_running_exact(&process).unwrap());
    let home = fixture._root.path().join("store");
    let configuration = Arc::new(Configuration::new(Store::open(&home).unwrap()));
    let manager = Arc::new(CoreManager::new(home, configuration.clone()));
    fixture.engine =
        LaunchEngine::with_resources(configuration, manager, Uuid::new_v4(), registry).unwrap();
    assert_eq!(
        confirmed(fixture.engine.status(request.request_id).unwrap().unwrap()),
        process
    );
    assert_eq!(
        fixture.engine.submit(fixture.request()).await.unwrap().id,
        request.request_id
    );
    assert!(process::is_running_exact(&process).unwrap());
    fixture.events(1).await;
}

#[tokio::test]
async fn accepted_work_deduplicates_confirms_and_reuses_only_after_exact_exit() {
    let fixture = Fixture::new(true);
    let request = fixture.request();
    let ack = fixture.engine.submit(request.clone()).await.unwrap();
    let mut aliases = Vec::new();
    for _ in 0..8 {
        let request = fixture.request();
        assert_eq!(
            fixture.engine.submit(request.clone()).await.unwrap().id,
            ack.id
        );
        aliases.push(request.request_id);
    }
    let process = confirmed(fixture.result(request.request_id).await);
    fixture.events(1).await;
    for id in aliases {
        assert_eq!(confirmed(fixture.result(id).await), process);
    }
    let value: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.events.join(format!("{}.json", process.pid))).unwrap(),
    )
    .unwrap();
    assert_eq!(value["value"], "private fixture value 引号");
    assert_eq!(value["cwd"], fixture._root.path().to_str().unwrap());
    assert!(value["http_proxy"].is_null());
    fixture.edit(|m| m.instances[0].name = "renamed".into());
    assert_eq!(
        fixture.engine.submit(fixture.request()).await.unwrap().id,
        request.request_id
    );
    fixture.edit(|m| m.instances[0].args.push("--nocapture".into()));
    assert!(matches!(
        fixture.engine.submit(fixture.request()).await,
        Err(Error::Invalid("INSTANCE_RUNNING_WITH_OTHER_CONFIG"))
    ));
    assert!(process::is_running_exact(&process).unwrap());
    process::terminate_exact(&process).unwrap();
    assert!(
        fixture
            .engine
            .status(request.request_id)
            .unwrap()
            .unwrap()
            .session_exited
    );
    let next = fixture.request();
    assert_ne!(
        fixture.engine.submit(next.clone()).await.unwrap().id,
        ack.id
    );
    let second = confirmed(fixture.result(next.request_id).await);
    assert_ne!(second.creation_time, process.creation_time);
    fixture.events(2).await;
    assert_eq!(
        confirmed(fixture.engine.submit(request.clone()).await.unwrap()),
        process
    );
    let mut conflict = request;
    conflict.origin = LaunchOrigin::Guard;
    assert!(matches!(
        fixture.engine.submit(conflict).await,
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
}

#[tokio::test]
async fn cancellation_and_dependency_changes_prevent_creation_but_rename_does_not() {
    let fixture = Fixture::new(true);
    for mode in [0, 1, 2] {
        let gate = Arc::new(tokio::sync::Notify::new());
        *fixture.engine.before_dispatch.lock().unwrap() = Some(gate.clone());
        let request = fixture.request();
        fixture.engine.submit(request.clone()).await.unwrap();
        fixture.ready(request.request_id).await;
        match mode {
            0 => {
                fixture.engine.cancel(request.request_id).unwrap();
            }
            1 => fixture.edit(|m| m.instances[0].args.push("--nocapture".into())),
            _ => fixture.edit(|m| m.instances[0].name = "display only".into()),
        }
        gate.notify_one();
        let result = fixture.result(request.request_id).await;
        match mode {
            0 => assert!(matches!(result.phase, LaunchPhase::Cancelled {})),
            1 => assert!(
                matches!(result.phase,LaunchPhase::Failed {ref code} if code=="LAUNCH_CONFIG_CHANGED")
            ),
            _ => {
                let process = confirmed(result);
                assert!(process::is_running_exact(&process).unwrap());
            }
        }
        if mode < 2 {
            assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
        }
    }
    fixture.events(1).await;
}

#[tokio::test]
async fn definite_creation_failure_releases_both_reservations_for_an_explicit_new_request() {
    let fixture = Fixture::new(false);
    let first = fixture.request();
    let second = fixture.request();
    for request in [&first, &second] {
        assert_eq!(
            fixture.engine.submit(request.clone()).await.unwrap().id,
            request.request_id
        );
        let result = fixture.result(request.request_id).await;
        assert!(
            matches!(result.phase,LaunchPhase::Failed {code} if code=="APPLICATION_NOT_CREATED")
        );
    }
    assert_eq!(
        fixture.engine.submit(first.clone()).await.unwrap().id,
        first.request_id
    );
    assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
}

#[tokio::test]
async fn externally_started_process_is_preserved_and_never_claimed_as_a_session() {
    let fixture = Fixture::new(true);
    let mut environment = app_proxy_core::EnvPatch::default();
    environment.set.insert(
        "APP_PROXY_ENGINE_EVENTS".into(),
        fixture.events.to_str().unwrap().into(),
    );
    let mut external = process::spawn(SpawnSpec {
        exe: fixture.exe.clone(),
        cwd: fixture._root.path().to_owned(),
        args: vec![
            "--ignored".into(),
            "--exact".into(),
            "launch_engine::tests::engine_child".into(),
        ],
        environment,
        mode: CreationMode::Normal,
    })
    .unwrap();
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let result = fixture.result(request.request_id).await;
    assert!(
        matches!(result.phase,LaunchPhase::Failed {code} if code=="INSTANCE_EXTERNALLY_RUNNING")
    );
    assert!(process::is_running_exact(&external.identity).unwrap());
    fixture.events(1).await;
    external.terminate().unwrap();
}

#[tokio::test]
async fn missing_proxy_dependency_never_falls_back_to_a_direct_application() {
    let fixture = Fixture::new(true);
    fixture.edit(|m| {
        let profile = Uuid::new_v4();
        let node = Uuid::new_v4();
        m.profiles.push(ProxyProfile {
            id: profile,
            name: "offline".into(),
            revision: 1,
            kind: ProxyKind::Managed,
            endpoint: Endpoint {
                host: "127.0.0.1".parse().unwrap(),
                port: 44191,
            },
            selected_node_id: node,
            source: ProxySource::Manual {
                nodes: vec![ManualNode {
                    id: node,
                    name: "offline".into(),
                    protocol: ManualProtocol::Http,
                    host: "127.0.0.1".into(),
                    port: 1,
                    credentials: None,
                }],
            },
        });
        m.instances[0].network = NetworkBinding::Profile {
            profile_id: profile,
        };
    });
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    assert!(matches!(
        fixture.result(request.request_id).await.phase,
        LaunchPhase::Failed { .. }
    ));
    assert_eq!(std::fs::read_dir(&fixture.events).unwrap().count(), 0);
}

#[test]
#[ignore = "native application fixture invoked by launch engine tests"]
fn engine_child() {
    let root = std::env::var_os("APP_PROXY_ENGINE_EVENTS").unwrap();
    let path = Path::new(&root).join(format!("{}.json", std::process::id()));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .unwrap();
    serde_json::to_writer(
        file,
        &serde_json::json!({"value":std::env::var("APP_PROXY_ENGINE_VALUE").ok(),
        "cwd":std::env::current_dir().unwrap(),"http_proxy":std::env::var("HTTP_PROXY").ok()}),
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
