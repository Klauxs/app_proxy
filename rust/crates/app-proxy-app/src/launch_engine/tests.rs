use super::*;
use app_proxy_windows::store::Store;
use std::{
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::Instant,
};

#[tokio::test]
async fn advanced_edit_preserves_running_session_and_blocks_stale_dispatch() {
    use app_proxy_core::registry::{
        ConfigAction, ConfigRequest, EnvironmentAssignment, EnvironmentEdit, InstanceEdit,
    };
    use app_proxy_windows::config_transaction::ConfigOutcome;
    let fixture = Fixture::new(true);
    let old_cwd = fixture._root.path().join("old working directory");
    std::fs::create_dir(&old_cwd).unwrap();
    fixture.edit(|m| {
        m.instances[0].cwd = WorkingDirectory::Explicit {
            path: old_cwd.clone(),
        }
    });
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let running = confirmed(fixture.result(request.request_id).await);
    fixture.events(1).await;
    let old_event: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.events.join(format!("{}.json", running.pid))).unwrap(),
    )
    .unwrap();
    assert_eq!(old_event["cwd"], old_cwd.to_str().unwrap());
    let journal_path = fixture._root.path().join("store/state/launch.json");
    let journal = std::fs::read(&journal_path).unwrap();
    let apply = |edit| {
        let revision = fixture.engine.configuration.snapshot().unwrap().revision;
        let result = fixture
            .engine
            .configuration
            .apply(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: revision,
                action: ConfigAction::EditInstance {
                    instance_id: fixture.instance,
                    edit,
                },
            })
            .unwrap();
        assert!(matches!(result, ConfigOutcome::Applied { .. }));
    };
    apply(InstanceEdit {
        env: Some(EnvironmentEdit {
            set: vec![EnvironmentAssignment {
                name: "APP_PROXY_ENGINE_VALUE".into(),
                secret_id: Uuid::new_v4(),
                value: "next-launch-only".into(),
            }],
            ..Default::default()
        }),
        ..Default::default()
    });
    let status = fixture
        .engine
        .observe_instance(fixture.instance, true)
        .await
        .unwrap();
    assert!(matches!(status.observation, InstanceObservation::Session {
        process, configuration_changed: Some(true), .. } if process == running));
    assert!(process::is_running_exact(&running).unwrap());
    assert_eq!(std::fs::read(&journal_path).unwrap(), journal);
    process::terminate_exact(&running).unwrap();

    let gate = Arc::new(tokio::sync::Notify::new());
    fixture.engine.hold_dispatch(gate.clone());
    let next = fixture.request();
    let revision = fixture.engine.configuration.snapshot().unwrap().revision;
    fixture
        .engine
        .submit_at_revision(next.clone(), Some(revision))
        .await
        .unwrap();
    fixture.ready(next.request_id).await;
    apply(InstanceEdit {
        cwd: Some(WorkingDirectory::Application {}),
        ..Default::default()
    });
    gate.notify_one();
    let failed = fixture.result(next.request_id).await;
    assert!(
        matches!(failed.phase, LaunchPhase::Failed { code } if code == "LAUNCH_CONFIG_CHANGED")
    );
    assert!(failed.dispatch_id.is_none());
    fixture.events(1).await;
    *fixture.engine.before_dispatch.lock().unwrap() = None;
    let fresh = fixture.request();
    fixture.engine.submit(fresh.clone()).await.unwrap();
    let updated = confirmed(fixture.result(fresh.request_id).await);
    fixture.events(2).await;
    let event: serde_json::Value = serde_json::from_slice(
        &std::fs::read(fixture.events.join(format!("{}.json", updated.pid))).unwrap(),
    )
    .unwrap();
    assert_eq!(event["value"], "next-launch-only");
    assert_eq!(
        event["cwd"],
        fixture.exe.parent().unwrap().to_str().unwrap()
    );
}

#[test]
fn subscription_dependency_tracks_selected_secret_and_preserves_legacy_manual_digest() {
    use app_proxy_core::subscription::{self, saved::SavedNode};
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../../examples/manifest.json")).unwrap();
    let id = manifest.instances[0].id;
    let ProxySource::Manual { nodes } = &mut manifest.profiles[0].source else {
        panic!()
    };
    nodes[0].credentials = Some(app_proxy_core::model::Credentials {
        username: "fixture-user".into(),
        password_secret_id: Uuid::new_v4(),
    });
    let (app, instance) = entries(&manifest, id).unwrap();
    let profile = &manifest.profiles[0];
    let ProxySource::Manual { nodes } = &profile.source else {
        panic!()
    };
    let node = &nodes[0];
    // This is the exact pre-subscription serialization, not a JSON Value.
    let legacy: [u8; 32] = Sha256::digest(
        serde_json::to_vec(&(
            &app.locator,
            app.template_ref,
            instance.application_id,
            &instance.data,
            &instance.args,
            &instance.env,
            &instance.cwd,
            &instance.network,
            Some((
                &profile.endpoint,
                node.id,
                &node.protocol,
                &node.host,
                node.port,
                &node.credentials,
            )),
            &manifest.settings.test_url,
            &manifest.settings.health_policy,
        ))
        .unwrap(),
    )
    .into();
    assert_eq!(dependency_digest(&manifest, id).unwrap(), legacy);

    let parsed = subscription::parse(include_str!(
        "../../../app-proxy-core/tests/fixtures/subscription.yaml"
    ))
    .unwrap();
    let nodes: Vec<_> = parsed
        .nodes
        .iter()
        .map(|node| {
            SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node)
                .unwrap()
                .0
        })
        .collect();
    manifest.profiles[0].selected_node_id = nodes[0].id;
    manifest.profiles[0].source = ProxySource::Subscription {
        url_secret_id: Uuid::new_v4(),
        revision: 1,
        nodes,
        auto_test_node_ids: vec![],
    };
    let before = dependency_digest(&manifest, id).unwrap();
    manifest.profiles[0].name = "display only".into();
    let ProxySource::Subscription {
        url_secret_id,
        revision,
        nodes,
        ..
    } = &mut manifest.profiles[0].source
    else {
        panic!()
    };
    *url_secret_id = Uuid::new_v4();
    *revision += 1;
    nodes[1].secret_id = Uuid::new_v4();
    assert_eq!(dependency_digest(&manifest, id).unwrap(), before);
    let ProxySource::Subscription { nodes, .. } = &mut manifest.profiles[0].source else {
        panic!()
    };
    nodes[0].secret_id = Uuid::new_v4();
    assert_ne!(dependency_digest(&manifest, id).unwrap(), before);
    let ProxySource::Subscription {
        nodes,
        auto_test_node_ids,
        ..
    } = &mut manifest.profiles[0].source
    else {
        panic!()
    };
    *auto_test_node_ids = vec![nodes[0].id, nodes[1].id];
    let group = dependency_digest(&manifest, id).unwrap();
    let ProxySource::Subscription { nodes, .. } = &mut manifest.profiles[0].source else {
        panic!()
    };
    nodes[2].secret_id = Uuid::new_v4();
    assert_eq!(dependency_digest(&manifest, id).unwrap(), group);
    let ProxySource::Subscription { nodes, .. } = &mut manifest.profiles[0].source else {
        panic!()
    };
    nodes[1].secret_id = Uuid::new_v4();
    assert_ne!(dependency_digest(&manifest, id).unwrap(), group);
}

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
        Err(Error::Invalid(
            app_proxy_core::error_code::INSTANCE_RESOURCE_BUSY
        ))
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

pub(crate) struct Fixture {
    engine: Arc<LaunchEngine>,
    instance: Uuid,
    exe: PathBuf,
    events: PathBuf,
    _root: tempfile::TempDir,
}
impl Fixture {
    pub(crate) fn monitor(&self) -> Arc<crate::guard_monitor::Monitor> {
        crate::guard_monitor::Monitor::new(self.engine.configuration.clone(), self.engine.clone())
    }
    pub(crate) fn guarded() -> Self {
        let fixture = Self::new(true);
        fixture.edit(|m| {
            let profile = Uuid::new_v4();
            let node = Uuid::new_v4();
            m.profiles.push(ProxyProfile {
                id: profile,
                name: "guard offline".into(),
                revision: 1,
                kind: ProxyKind::Managed,
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 44193,
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
            m.applications[0].template_ref = Template::Codex;
            m.instances[0].data = InstanceData::Isolated {
                location: StorageLocation::Store {
                    relative_path: format!("instances/{}", m.instances[0].id).into(),
                },
            };
            m.instances[0].guard.desired = Desired::Enabled;
            m.instances[0].network = NetworkBinding::Profile {
                profile_id: profile,
            };
        });
        fixture
    }
    pub(crate) fn external_guard_target(&self, isolated: bool, matching: bool) -> GuardChild {
        self.external_guard_tree(isolated, matching, false)
    }
    fn external_guard_tree(&self, isolated: bool, matching: bool, auxiliary: bool) -> GuardChild {
        let mut environment = app_proxy_core::EnvPatch::default();
        if auxiliary {
            environment
                .set
                .insert("APP_PROXY_ENGINE_AUXILIARY".into(), "1".into());
        }
        environment.set.insert(
            "APP_PROXY_ENGINE_EVENTS".into(),
            self.events.to_str().unwrap().into(),
        );
        let mut args: Vec<std::ffi::OsString> =
            ["--ignored", "--exact", "launch_engine::tests::engine_child"]
                .map(Into::into)
                .to_vec();
        if isolated {
            let data = self
                .engine
                .configuration
                .lock()
                .unwrap()
                .prepare_instance_data(self.instance, None)
                .unwrap()
                .unwrap();
            args.extend([
                "--skip".into(),
                format!("--user-data-dir={}", data.paths.user_data.display()).into(),
            ]);
        }
        if matching {
            args.extend([
                "--skip".into(),
                "--proxy-server=http://127.0.0.1:44193".into(),
            ]);
        }
        GuardChild(
            process::spawn(SpawnSpec {
                exe: self.exe.clone(),
                args,
                cwd: self._root.path().into(),
                environment,
            })
            .unwrap(),
        )
    }
    async fn correct(&self, child: &GuardChild) -> LaunchAttempt {
        let mut request = self.request();
        request.origin = LaunchOrigin::Guard;
        let revision = self.engine.configuration.snapshot().unwrap().revision;
        self.engine
            .submit_guard(
                request.clone(),
                revision,
                GuardTarget {
                    process: child.0.identity.clone(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: 44193,
                    },
                },
            )
            .unwrap();
        self.result(request.request_id).await
    }
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
    pub(crate) async fn events(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while std::fs::read_dir(&self.events).unwrap().count() != count {
            assert!(Instant::now() < deadline, "unexpected creation count");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}
pub(crate) struct GuardChild(pub process::StartedProcess);
impl Drop for GuardChild {
    fn drop(&mut self) {
        let _ = self.0.terminate();
    }
}

#[tokio::test]
async fn guard_event_binds_creation_and_preserves_original_and_compliant_processes() {
    use app_proxy_windows::etw::ProcessStartHint;
    let fixture = Fixture::guarded();
    fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .prepare_instance_data(fixture.instance, None)
        .unwrap();
    for (isolated, matching) in [(false, false), (true, true), (true, false)] {
        let child = fixture.external_guard_target(isolated, matching);
        let mut hint = ProcessStartHint {
            pid: child.0.identity.pid,
            creation_time: child.0.identity.creation_time + 1,
            event_time: child.0.identity.creation_time as i64,
            image_name: "untrusted.exe".into(),
        };
        assert!(
            fixture
                .engine
                .observe_guard_event(&hint)
                .await
                .unwrap()
                .is_none()
        );
        hint.creation_time = child.0.identity.creation_time;
        let observed = fixture.engine.observe_guard_event(&hint).await.unwrap();
        assert_eq!(observed.is_some(), isolated && !matching);
        if let Some((scan, pinned)) = observed {
            let GuardObservation::Correction { target } = scan.observation else {
                panic!("not a correction");
            };
            assert_eq!(target.process, child.0.identity);
            assert_eq!(pinned.identity(), &child.0.identity);
        }
        assert!(process::is_running_exact(&child.0.identity).unwrap());
    }
}

#[tokio::test]
async fn guard_event_keeps_pin_through_submission_and_rejects_changed_configuration() {
    use app_proxy_windows::etw::ProcessStartHint;
    for edited in [false, true] {
        let fixture = Fixture::guarded();
        let child = fixture.external_guard_target(true, false);
        let hint = ProcessStartHint {
            pid: child.0.identity.pid,
            creation_time: child.0.identity.creation_time,
            event_time: child.0.identity.creation_time as i64,
            image_name: "hint.exe".into(),
        };
        let (scan, pinned) = fixture
            .engine
            .observe_guard_event(&hint)
            .await
            .unwrap()
            .unwrap();
        let GuardObservation::Correction { target } = scan.observation else {
            panic!("not a correction");
        };
        if edited {
            fixture.edit(|m| m.instances[0].guard.desired = Desired::Disabled);
        }
        let mut request = fixture.request();
        request.origin = LaunchOrigin::Guard;
        let admitted = fixture
            .engine
            .submit_guard_event(request.clone(), scan.revision, target, pinned)
            .await;
        if edited {
            assert!(admitted.is_err());
            assert!(process::is_running_exact(&child.0.identity).unwrap());
        } else {
            admitted.unwrap();
            let result = fixture.result(request.request_id).await;
            assert!(result.guard_correction.unwrap().stop_confirmed);
            assert!(
                matches!(result.phase, LaunchPhase::Failed { code } if code == "GUARD_STOPPED_PROXY_UNAVAILABLE")
            );
            assert!(!process::is_running_exact(&child.0.identity).unwrap());
        }
    }
}

#[tokio::test]
async fn guard_event_stops_before_journal_and_preserves_config_stop_barrier() {
    use app_proxy_windows::etw::ProcessStartHint;
    for disable in [false, true] {
        let fixture = Fixture::guarded();
        let child = fixture.external_guard_target(true, false);
        let hint = ProcessStartHint {
            pid: child.0.identity.pid,
            creation_time: child.0.identity.creation_time,
            event_time: child.0.identity.creation_time as i64,
            image_name: "hint.exe".into(),
        };
        let (scan, pinned) = fixture
            .engine
            .observe_guard_event(&hint)
            .await
            .unwrap()
            .unwrap();
        let GuardObservation::Correction { target } = scan.observation else {
            panic!("not correction")
        };
        let mut request = fixture.request();
        request.origin = LaunchOrigin::Guard;
        let id = request.request_id;
        let configuration = fixture.engine.configuration.clone();
        let process_identity = child.0.identity.clone();
        *fixture.engine.before_guard_stop.lock().unwrap() = Some(Arc::new(move || {
            let mut store = configuration.lock().unwrap();
            assert!(store.launch_request(id).unwrap().is_none());
            assert!(process::is_running_exact(&process_identity).unwrap());
            if disable {
                let mut manifest = store.load().unwrap();
                manifest.instances[0].guard.desired = Desired::Disabled;
                store.commit(manifest.revision, manifest).unwrap();
            }
        }));
        let admitted = fixture
            .engine
            .submit_guard_event(request, scan.revision, target, pinned)
            .await;
        if disable {
            assert!(admitted.is_err());
            assert!(process::is_running_exact(&child.0.identity).unwrap());
            assert!(
                fixture
                    .engine
                    .configuration
                    .lock()
                    .unwrap()
                    .launch_request(id)
                    .unwrap()
                    .is_none()
            );
        } else {
            let admitted = admitted.unwrap();
            assert!(admitted.guard_correction.unwrap().stop_confirmed);
            assert!(!process::is_running_exact(&child.0.identity).unwrap());
            let result = fixture.result(id).await;
            assert!(matches!(result.phase, LaunchPhase::Failed { .. }));
        }
    }
}

#[tokio::test]
async fn guard_event_stops_new_main_while_an_earlier_restart_is_pending() {
    use app_proxy_windows::etw::ProcessStartHint;
    let fixture = Fixture::guarded();
    let first = fixture.external_guard_target(true, false);
    let second = fixture.external_guard_target(true, false);
    let mut pending = fixture.request();
    pending.origin = LaunchOrigin::Guard;
    let revision = fixture.engine.configuration.snapshot().unwrap().revision;
    fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .begin_guard_launch(
            &pending,
            fixture.engine.epoch,
            revision,
            GuardTarget {
                process: first.0.identity.clone(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 44193,
                },
            },
        )
        .unwrap();
    fixture
        .engine
        .active
        .lock()
        .unwrap()
        .insert(pending.request_id);
    let hint = ProcessStartHint {
        pid: second.0.identity.pid,
        creation_time: second.0.identity.creation_time,
        event_time: 1,
        image_name: "hint.exe".into(),
    };
    let (scan, pinned) = fixture
        .engine
        .observe_guard_event(&hint)
        .await
        .unwrap()
        .expect("pending restart must not suppress a new unproxied main");
    let GuardObservation::Correction { target } = scan.observation else {
        panic!("not correction")
    };
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    let result = fixture
        .engine
        .submit_guard_event(request, revision, target, pinned)
        .await
        .unwrap();
    assert_eq!(result.id, pending.request_id);
    assert!(!process::is_running_exact(&second.0.identity).unwrap());
    assert!(process::is_running_exact(&first.0.identity).unwrap());
    assert_eq!(
        fixture
            .engine
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap()
            .len(),
        1
    );
    fixture
        .engine
        .active
        .lock()
        .unwrap()
        .remove(&pending.request_id);
}

#[tokio::test]
async fn guard_event_stops_even_when_post_stop_journal_write_is_unavailable() {
    use app_proxy_windows::etw::ProcessStartHint;
    use std::os::windows::fs::OpenOptionsExt;
    let fixture = Fixture::guarded();
    let prior = fixture.request();
    {
        let mut store = fixture.engine.configuration.lock().unwrap();
        store.begin_launch(&prior, fixture.engine.epoch).unwrap();
        store
            .advance_launch(
                prior.request_id,
                fixture.engine.epoch,
                &LaunchPhase::Accepted {},
                LaunchPhase::Cancelled {},
            )
            .unwrap();
    }
    let child = fixture.external_guard_target(true, false);
    let hint = ProcessStartHint {
        pid: child.0.identity.pid,
        creation_time: child.0.identity.creation_time,
        event_time: child.0.identity.creation_time as i64,
        image_name: "hint.exe".into(),
    };
    let (scan, pinned) = fixture
        .engine
        .observe_guard_event(&hint)
        .await
        .unwrap()
        .unwrap();
    let GuardObservation::Correction { target } = scan.observation else {
        panic!("not correction")
    };
    let path = fixture._root.path().join("store/state/launch.json");
    let before = std::fs::read(&path).unwrap();
    // Simulate a blocked journal replacement, without blocking configuration
    // reads or the exact native process handle used for the urgent stop.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    assert!(
        fixture
            .engine
            .submit_guard_event(request, scan.revision, target, pinned)
            .await
            .is_err()
    );
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(fixture.engine.active.lock().unwrap().is_empty());
    drop(held);
    // No saved request means recovery must not fabricate or replay a restart.
    fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .recover_launches(Uuid::new_v4())
        .unwrap();
    let attempts = fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .launch_attempts()
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].id, prior.request_id);
}

#[tokio::test]
async fn guard_event_stops_before_waiting_for_restart_resource() {
    use app_proxy_windows::etw::ProcessStartHint;
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_target(true, false);
    let hint = ProcessStartHint {
        pid: child.0.identity.pid,
        creation_time: child.0.identity.creation_time,
        event_time: child.0.identity.creation_time as i64,
        image_name: "hint.exe".into(),
    };
    let (scan, pinned) = fixture
        .engine
        .observe_guard_event(&hint)
        .await
        .unwrap()
        .unwrap();
    let resource = InstanceResource::resolve(&pinned.application, pinned.data.as_ref()).unwrap();
    let held = fixture.engine.resources.acquire(resource).unwrap();
    let GuardObservation::Correction { target } = scan.observation else {
        panic!("not correction")
    };
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    fixture
        .engine
        .submit_guard_event(request.clone(), scan.revision, target, pinned)
        .await
        .unwrap();
    let result = fixture.result(request.request_id).await;
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    assert!(result.guard_correction.unwrap().stop_confirmed);
    assert!(
        matches!(result.phase, LaunchPhase::Failed { code } if code == "INSTANCE_RESOURCE_BUSY")
    );
    assert!(result.dispatch_id.is_none());
    drop(held);
}

#[tokio::test]
async fn original_guard_scan_keeps_exact_main_when_helpers_have_explicit_default_data() {
    for process_type in ["renderer", "unexpected-role"] {
        let fixture = Fixture::guarded();
        fixture.edit(|m| m.instances[0].data = InstanceData::Original {});
        let main = fixture.external_guard_target(false, false);
        let directory = fixture._root.path().join("app-default-data");
        std::fs::create_dir(&directory).unwrap();
        let mut environment = app_proxy_core::EnvPatch::default();
        environment.set.insert(
            "APP_PROXY_ENGINE_EVENTS".into(),
            fixture.events.to_str().unwrap().into(),
        );
        let helper = GuardChild(
            process::spawn(SpawnSpec {
                exe: fixture.exe.clone(),
                args: vec![
                    "--ignored".into(),
                    "--exact".into(),
                    "launch_engine::tests::engine_child".into(),
                    "--skip".into(),
                    format!("--type={process_type}").into(),
                    "--skip".into(),
                    format!("--user-data-dir={}", directory.display()).into(),
                ],
                cwd: fixture._root.path().into(),
                environment,
            })
            .unwrap(),
        );
        let scan = fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap();
        if process_type == "renderer" {
            assert!(
                matches!(scan.observation, GuardObservation::Correction { target }
                if target.process == main.0.identity)
            );
            assert!(process::is_running_exact(&main.0.identity).unwrap());
            assert!(process::is_running_exact(&helper.0.identity).unwrap());
            drop(main);
            assert!(
                matches!(fixture.engine.observe_guard(fixture.instance).await.unwrap().observation,
                GuardObservation::Blocked { code } if code == "GUARD_AUXILIARY_STILL_RUNNING")
            );
        } else {
            assert!(
                matches!(scan.observation, GuardObservation::Blocked { code }
                if code == "INSTANCE_PROCESS_UNKNOWN")
            );
        }
    }
}

#[tokio::test]
async fn guard_scan_distinguishes_original_compliant_and_misconfigured_without_stopping() {
    for (isolated, matching) in [(false, false), (true, true), (true, false)] {
        let fixture = Fixture::guarded();
        fixture
            .engine
            .configuration
            .lock()
            .unwrap()
            .prepare_instance_data(fixture.instance, None)
            .unwrap();
        let child = fixture.external_guard_target(isolated, matching);
        fixture.events(1).await;
        let scan = fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap();
        match (isolated, matching, scan.observation) {
            (false, _, GuardObservation::Absent {}) => {}
            (true, true, GuardObservation::Compliant { process }) => {
                assert_eq!(process, child.0.identity)
            }
            (true, false, GuardObservation::Correction { target }) => {
                assert_eq!(target.process, child.0.identity);
                assert_eq!(target.endpoint.port, 44193);
                assert_eq!(
                    scan.revision,
                    fixture.engine.configuration.snapshot().unwrap().revision
                );
            }
            _ => panic!("incorrect guard classification"),
        }
        assert!(process::is_running_exact(&child.0.identity).unwrap());
        assert!(
            fixture
                .engine
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
async fn runtime_observation_keeps_disabled_guard_session_and_history_read_only() {
    let fixture = Fixture::new(true);
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let running = confirmed(fixture.result(request.request_id).await);
    let home = fixture._root.path().join("store");
    let journal = std::fs::read(home.join("state/launch.json")).unwrap();
    let manifest = std::fs::read(home.join("manifest.json")).unwrap();
    let status = fixture
        .engine
        .observe_instance(fixture.instance, true)
        .await
        .unwrap();
    assert!(
        matches!(status.observation, InstanceObservation::Session { process, network: LaunchNetwork::Direct {}, configuration_changed: Some(false) } if process == running)
    );
    assert_eq!(
        std::fs::read(home.join("state/launch.json")).unwrap(),
        journal
    );
    assert_eq!(std::fs::read(home.join("manifest.json")).unwrap(), manifest);
    fixture.edit(|m| {
        m.applications[0].locator = ApplicationLocator::Exe {
            path: fixture._root.path().join("no-longer-installed.exe"),
        }
    });
    let status = fixture
        .engine
        .observe_instance(fixture.instance, true)
        .await
        .unwrap();
    assert!(
        matches!(status.observation, InstanceObservation::Session { process, network: LaunchNetwork::Direct {}, configuration_changed: Some(true) } if process == running)
    );
    assert_eq!(
        std::fs::read(home.join("state/launch.json")).unwrap(),
        journal
    );
    assert!(process::is_running_exact(&running).unwrap());
}

#[tokio::test]
async fn runtime_observation_checks_external_process_after_recorded_session_exits() {
    let fixture = Fixture::new(true);
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let running = confirmed(fixture.result(request.request_id).await);
    process::terminate_exact(&running).unwrap();
    let external = fixture.external_guard_target(false, false);
    fixture.events(2).await;
    let path = fixture._root.path().join("store/state/launch.json");
    let before = std::fs::read(&path).unwrap();
    let status = fixture
        .engine
        .observe_instance(fixture.instance, true)
        .await
        .unwrap();
    assert!(
        matches!(status.observation, InstanceObservation::Observed { process } if process == external.0.identity)
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(process::is_running_exact(&external.0.identity).unwrap());
    drop(external);
    assert!(matches!(
        fixture
            .engine
            .observe_instance(fixture.instance, true)
            .await
            .unwrap()
            .observation,
        InstanceObservation::Absent {}
    ));
    assert_eq!(std::fs::read(path).unwrap(), before);
}

#[tokio::test]
async fn runtime_observation_never_creates_data_or_adopts_external_clones() {
    let fixture = Fixture::guarded();
    fixture.edit(|m| {
        m.instances[0].guard.desired = Desired::Disabled;
        m.instances[0].network = NetworkBinding::Direct {};
    });
    let data = fixture._root.path().join("store/instances");
    assert!(matches!(
        fixture
            .engine
            .observe_instance(fixture.instance, true)
            .await
            .unwrap()
            .observation,
        InstanceObservation::Absent {}
    ));
    assert!(!data.exists());
    let original = fixture.external_guard_target(false, false);
    assert!(matches!(
        fixture
            .engine
            .observe_instance(fixture.instance, true)
            .await
            .unwrap()
            .observation,
        InstanceObservation::Unknown { .. }
    ));
    assert!(!data.exists());
    let clone = fixture.external_guard_target(true, false);
    let status = fixture
        .engine
        .observe_instance(fixture.instance, true)
        .await
        .unwrap();
    assert!(
        matches!(status.observation, InstanceObservation::Observed { process } if process == clone.0.identity)
    );
    let other = fixture.external_guard_target(true, false);
    assert!(
        matches!(fixture.engine.observe_instance(fixture.instance, true).await.unwrap().observation, InstanceObservation::Unknown { code } if code == "INSTANCE_MULTIPLE_MAIN_PROCESSES")
    );
    for child in [&original, &clone, &other] {
        assert!(process::is_running_exact(&child.0.identity).unwrap());
    }
    assert!(
        fixture
            .engine
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn runtime_observation_rechecks_edits_and_new_pending_work() {
    for edit in [false, true] {
        let fixture = Fixture::new(true);
        let configuration = fixture.engine.configuration.clone();
        let request = fixture.request();
        let epoch = fixture.engine.epoch;
        *fixture.engine.after_guard_scan.lock().unwrap() = Some(Arc::new(move || {
            let mut store = configuration.lock().unwrap();
            if edit {
                let mut manifest = store.load().unwrap();
                manifest.instances[0].name = "changed".into();
                store.commit(manifest.revision, manifest).unwrap();
            } else {
                store.begin_launch(&request, epoch).unwrap();
            }
        }));
        let observation = fixture
            .engine
            .observe_instance(fixture.instance, true)
            .await
            .unwrap()
            .observation;
        if edit {
            assert!(
                matches!(observation, InstanceObservation::Unknown { code } if code == "LAUNCH_CONFIG_CHANGED")
            );
        } else {
            assert!(matches!(observation, InstanceObservation::Pending { .. }));
        }
    }
}

#[tokio::test]
async fn runtime_observation_timeout_retains_native_resolution_permit() {
    let fixture = Fixture::new(true);
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    *fixture.engine.before_guard_resolution.lock().unwrap() = Some(Arc::new(move || {
        wait.lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    }));
    assert!(
        matches!(fixture.engine.observe_instance(fixture.instance, true).await.unwrap().observation, InstanceObservation::Unknown { code } if code == "INSTANCE_OBSERVATION_TIMEOUT")
    );
    assert!(
        matches!(fixture.engine.observe_instance(fixture.instance, true).await.unwrap().observation, InstanceObservation::Unknown { code } if code == "GUARD_RESOLUTION_BUSY")
    );
    *fixture.engine.before_guard_resolution.lock().unwrap() = None;
    release.send(()).unwrap();
    let _permit = tokio::time::timeout(
        Duration::from_secs(3),
        fixture.engine.guard_resolution.acquire(),
    )
    .await
    .unwrap()
    .unwrap();
}

#[tokio::test]
async fn guard_scan_does_not_prepare_missing_data_and_pending_launches_are_deferred() {
    let fixture = Fixture::guarded();
    let data = fixture._root.path().join("store/instances");
    assert!(!data.exists());
    assert!(matches!(
        fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap()
            .observation,
        GuardObservation::Absent {}
    ));
    assert!(!data.exists());
    let request = fixture.request();
    fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .begin_launch(&request, fixture.engine.epoch)
        .unwrap();
    assert!(
        matches!(fixture.engine.observe_guard(fixture.instance).await.unwrap().observation,
        GuardObservation::Pending { attempt_id } if attempt_id == request.request_id)
    );
    assert!(!data.exists());
}

#[tokio::test]
async fn guard_scan_preserves_confirmed_session_binding_after_configuration_changes() {
    let fixture = Fixture::guarded();
    fixture.edit(|m| {
        m.applications[0].template_ref = Template::Environment;
        m.instances[0].data = InstanceData::Original {};
        m.instances[0].network = NetworkBinding::Direct {};
        m.instances[0].guard.desired = Desired::Disabled;
    });
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let process = confirmed(fixture.result(request.request_id).await);
    fixture.events(1).await;
    fixture.edit(|m| {
        m.applications[0].template_ref = Template::Chromium;
        m.instances[0].network = NetworkBinding::Profile {
            profile_id: m.profiles[0].id,
        };
        m.instances[0].guard.desired = Desired::Enabled;
    });
    let scan = fixture
        .engine
        .observe_guard(fixture.instance)
        .await
        .unwrap();
    assert!(
        matches!(scan.observation, GuardObservation::Session { process: observed, network: LaunchNetwork::Direct {} } if observed == process)
    );
    assert!(process::is_running_exact(&process).unwrap());
    fixture.events(1).await;
}

#[tokio::test]
async fn guard_scan_refuses_ambiguous_multiple_mains_and_disabled_instances() {
    let fixture = Fixture::guarded();
    let first = fixture.external_guard_target(true, false);
    let second = fixture.external_guard_target(true, false);
    fixture.events(2).await;
    assert!(matches!(
        fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap()
            .observation,
        GuardObservation::Blocked { code } if code == "GUARD_MULTIPLE_MAIN_PROCESSES"
    ));
    fixture.edit(|m| m.instances[0].guard.desired = Desired::Disabled);
    assert!(matches!(
        fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap()
            .observation,
        GuardObservation::Disabled {}
    ));
    assert!(process::is_running_exact(&first.0.identity).unwrap());
    assert!(process::is_running_exact(&second.0.identity).unwrap());
}

#[tokio::test]
async fn guard_scan_rechecks_edits_and_new_pending_work_before_returning_a_correction() {
    for edit in [false, true] {
        let fixture = Fixture::guarded();
        let child = fixture.external_guard_target(true, false);
        fixture.events(1).await;
        let configuration = fixture.engine.configuration.clone();
        let request = fixture.request();
        let epoch = fixture.engine.epoch;
        *fixture.engine.after_guard_scan.lock().unwrap() = Some(Arc::new(move || {
            let mut store = configuration.lock().unwrap();
            if edit {
                let mut manifest = store.load().unwrap();
                manifest.instances[0].guard.desired = Desired::Disabled;
                store.commit(manifest.revision, manifest).unwrap();
            } else {
                store.begin_launch(&request, epoch).unwrap();
            }
        }));
        let scan = fixture
            .engine
            .observe_guard(fixture.instance)
            .await
            .unwrap();
        if edit {
            assert!(matches!(
                scan.observation,
                GuardObservation::Blocked { code } if code == "LAUNCH_CONFIG_CHANGED"
            ));
        } else {
            assert!(matches!(scan.observation, GuardObservation::Pending { .. }));
        }
        assert!(process::is_running_exact(&child.0.identity).unwrap());
    }
}

#[tokio::test]
async fn guard_scan_timeout_cannot_accumulate_detached_resolvers() {
    let fixture = Fixture::guarded();
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *fixture.engine.before_guard_resolution.lock().unwrap() = Some(Arc::new(move || {
        entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(12))
            .unwrap();
    }));
    let engine = fixture.engine.clone();
    let id = fixture.instance;
    let scan = tokio::spawn(async move { engine.observe_guard(id).await.unwrap() });
    tokio::time::timeout(Duration::from_secs(3), entered_rx)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        fixture.engine.observe_guard(id).await.unwrap().observation,
        GuardObservation::Blocked { code } if code == "GUARD_RESOLUTION_BUSY"
    ));
    assert!(matches!(
        scan.await.unwrap().observation,
        GuardObservation::Blocked { code } if code == "GUARD_SCAN_TIMEOUT"
    ));
    assert!(matches!(
        fixture.engine.observe_guard(id).await.unwrap().observation,
        GuardObservation::Blocked { code } if code == "GUARD_RESOLUTION_BUSY"
    ));
    *fixture.engine.before_guard_resolution.lock().unwrap() = None;
    release_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while fixture.engine.guard_resolution.available_permits() == 0 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(matches!(
        fixture.engine.observe_guard(id).await.unwrap().observation,
        GuardObservation::Absent {}
    ));
}

#[tokio::test]
async fn guard_stops_exact_misconfigured_clone_before_proxy_failure_and_never_falls_back() {
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_target(true, false);
    fixture.events(1).await;
    let result = fixture.correct(&child).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { code } if code == "GUARD_STOPPED_PROXY_UNAVAILABLE")
    );
    let correction = result.guard_correction.unwrap();
    assert!(correction.stop_nonce.is_some() && correction.stop_confirmed);
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    assert!(result.dispatch_id.is_none());
    fixture.events(1).await;
}

#[tokio::test]
async fn guard_ignores_unrelated_candidate_exit_and_preserves_original() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture = Fixture::guarded();
    let original = fixture.external_guard_target(false, false);
    let transient = fixture.external_guard_target(false, false);
    let child = fixture.external_guard_target(true, false);
    fixture.events(3).await;
    let identity = transient.0.identity.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    *fixture.engine.after_guard_target_read.lock().unwrap() = Some(Arc::new(move || {
        if counted.fetch_add(1, Ordering::SeqCst) == 0 {
            app_proxy_windows::process_stop::stop_exact(&identity, true).unwrap();
        }
    }));
    let result = fixture.correct(&child).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { ref code } if code == "GUARD_STOPPED_PROXY_UNAVAILABLE")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(result.guard_correction.unwrap().stop_confirmed);
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    assert!(process::is_running_exact(&original.0.identity).unwrap());
}

#[tokio::test]
async fn guard_target_exit_during_snapshot_never_dispatches_stop_or_relaunch() {
    let fixture = Fixture::guarded();
    let original = fixture.external_guard_target(false, false);
    let child = fixture.external_guard_target(true, false);
    fixture.events(2).await;
    let identity = child.0.identity.clone();
    *fixture.engine.after_guard_target_read.lock().unwrap() = Some(Arc::new(move || {
        app_proxy_windows::process_stop::stop_exact(&identity, true).unwrap();
    }));
    let result = fixture.correct(&child).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { ref code } if code == "GUARD_TARGET_EXITED_BEFORE_STOP")
    );
    let correction = result.guard_correction.unwrap();
    assert!(correction.stop_started_at.is_none() && correction.stop_nonce.is_none());
    assert!(!correction.stop_confirmed && result.dispatch_id.is_none());
    assert!(process::is_running_exact(&original.0.identity).unwrap());
}

#[tokio::test]
async fn guard_does_not_repeat_target_observation_for_unrelated_churn() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let fixture = Fixture::guarded();
    let transient: Vec<_> = (0..3)
        .map(|_| fixture.external_guard_target(false, false))
        .collect();
    let child = fixture.external_guard_target(true, false);
    fixture.events(4).await;
    let identities: Vec<_> = transient.iter().map(|p| p.0.identity.clone()).collect();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    *fixture.engine.after_guard_target_read.lock().unwrap() = Some(Arc::new(move || {
        let identity = &identities[counted.fetch_add(1, Ordering::SeqCst)];
        let watch = process::watch_exit(identity).unwrap();
        process::terminate_exact(identity).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while watch.is_running().unwrap() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
    }));
    let result = fixture.correct(&child).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { ref code } if code == "GUARD_STOPPED_PROXY_UNAVAILABLE")
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let correction = result.guard_correction.unwrap();
    assert!(correction.stop_started_at.is_some() && correction.stop_nonce.is_some());
    assert!(correction.stop_confirmed && result.dispatch_id.is_none());
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    for other in &transient[1..] {
        assert!(process::is_running_exact(&other.0.identity).unwrap());
    }
}

#[tokio::test]
async fn guard_rechecks_configuration_and_cancellation_around_the_stop() {
    for after_stop in [false, true] {
        for cancel in [false, true] {
            let fixture = Fixture::guarded();
            let child = fixture.external_guard_target(true, false);
            fixture.events(1).await;
            let configuration = fixture.engine.configuration.clone();
            let hook: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                let mut store = configuration.lock().unwrap();
                if cancel {
                    let attempt = store
                        .launch_attempts()
                        .unwrap()
                        .into_iter()
                        .find(|a| matches!(a.phase, LaunchPhase::CheckingInstance {}))
                        .unwrap();
                    store.request_launch_cancel(attempt.id).unwrap();
                } else {
                    let mut manifest = store.load().unwrap();
                    manifest.instances[0].guard.desired = Desired::Disabled;
                    store.commit(manifest.revision, manifest).unwrap();
                }
            });
            if after_stop {
                *fixture.engine.after_guard_stop.lock().unwrap() = Some(hook);
            } else {
                *fixture.engine.before_guard_stop.lock().unwrap() = Some(hook);
            }
            let result = fixture.correct(&child).await;
            let correction = result.guard_correction.unwrap();
            assert_eq!(correction.stop_confirmed, after_stop);
            assert_eq!(correction.stop_nonce.is_some(), after_stop);
            assert_eq!(
                process::is_running_exact(&child.0.identity).unwrap(),
                !after_stop
            );
            if cancel && !after_stop {
                assert!(matches!(result.phase, LaunchPhase::Cancelled {}));
            } else {
                let expected = if cancel {
                    "GUARD_CANCELLED_AFTER_STOP_REQUEST"
                } else {
                    "LAUNCH_CONFIG_CHANGED"
                };
                assert!(matches!(result.phase, LaunchPhase::Failed { code } if code == expected));
            }
            assert!(result.dispatch_id.is_none());
            fixture.events(1).await;
        }
    }
}

#[tokio::test]
async fn guard_checks_remaining_auxiliaries_after_stop_without_killing_them_or_relaunching() {
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_tree(true, false, true);
    fixture.events(2).await;
    let path = fixture._root.path().join("auxiliary.json");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !path.exists() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let auxiliary: app_proxy_core::ProcessIdentity =
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    struct Cleanup(app_proxy_core::ProcessIdentity);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = process::terminate_exact(&self.0);
        }
    }
    let auxiliary = Cleanup(auxiliary);
    let result = fixture.correct(&child).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { code } if code == "GUARD_RESTART_NOT_CLEAR")
    );
    assert!(result.guard_correction.unwrap().stop_confirmed);
    assert!(result.dispatch_id.is_none());
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    assert!(process::is_running_exact(&auxiliary.0).unwrap());
    fixture.events(2).await;
}

#[tokio::test]
async fn guard_late_stop_worker_retains_resource_after_timeout_and_cannot_relaunch() {
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_target(true, false);
    fixture.events(1).await;
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let entered_tx = Mutex::new(Some(entered_tx));
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = Mutex::new(release_rx);
    *fixture.engine.after_guard_stop.lock().unwrap() = Some(Arc::new(move || {
        entered_tx.lock().unwrap().take().unwrap().send(()).unwrap();
        release_rx
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .unwrap();
    }));
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    let revision = fixture.engine.configuration.snapshot().unwrap().revision;
    fixture
        .engine
        .submit_guard(
            request.clone(),
            revision,
            GuardTarget {
                process: child.0.identity.clone(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 44193,
                },
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), entered_rx)
        .await
        .unwrap()
        .unwrap();
    fixture
        .engine
        .end_failed(request.request_id, None, &Error::Invalid("LAUNCH_TIMEOUT"))
        .unwrap();
    let application = installation::resolve(&ApplicationLocator::Exe {
        path: fixture.exe.clone(),
    })
    .unwrap();
    let data = fixture
        .engine
        .configuration
        .lock()
        .unwrap()
        .prepare_instance_data(fixture.instance, None)
        .unwrap();
    assert!(matches!(
        fixture
            .engine
            .resources
            .acquire(InstanceResource::resolve(&application, data.as_ref()).unwrap()),
        Err(Error::Invalid(
            app_proxy_core::error_code::INSTANCE_RESOURCE_BUSY
        ))
    ));
    release_tx.send(()).unwrap();
    let result = fixture.result(request.request_id).await;
    assert!(matches!(result.phase, LaunchPhase::Failed { code } if code == "LAUNCH_TIMEOUT"));
    assert!(result.guard_correction.unwrap().stop_confirmed);
    assert!(result.dispatch_id.is_none());
    fixture.events(1).await;
}

#[tokio::test]
async fn guard_failed_stop_receipt_stays_unconfirmed_and_recovery_never_replays() {
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_target(true, false);
    fixture.events(1).await;
    let path = fixture._root.path().join("store/state/launch.json");
    let (sender, receiver) = std::sync::mpsc::channel();
    *fixture.engine.before_guard_receipt.lock().unwrap() = Some(Arc::new(move || {
        sender.send(hold_replacement(&path)).unwrap();
    }));
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    fixture
        .engine
        .submit_guard(
            request.clone(),
            fixture.engine.configuration.snapshot().unwrap().revision,
            GuardTarget {
                process: child.0.identity.clone(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 44193,
                },
            },
        )
        .unwrap();
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
    assert!(!process::is_running_exact(&child.0.identity).unwrap());
    {
        let mut store = fixture.engine.configuration.lock().unwrap();
        let before = store.launch_request(request.request_id).unwrap().unwrap();
        assert!(
            before
                .guard_correction
                .as_ref()
                .unwrap()
                .stop_nonce
                .is_some()
        );
        assert!(!before.guard_correction.unwrap().stop_confirmed);
        assert!(before.dispatch_id.is_none());
        drop(held);
        store.recover_launches(Uuid::new_v4()).unwrap();
        let recovered = store.launch_request(request.request_id).unwrap().unwrap();
        assert!(
            matches!(recovered.phase, LaunchPhase::Failed { code } if code == "GUARD_CORRECTION_INTERRUPTED")
        );
        assert!(!recovered.guard_correction.unwrap().stop_confirmed);
        assert!(
            store
                .dispatch_guard_stop(request.request_id, fixture.engine.epoch)
                .is_err()
        );
    }
    fixture.events(1).await;
}

#[tokio::test]
async fn guard_preserves_unmanaged_original_and_correct_proxy_during_network_failure() {
    for (isolated, matching) in [(false, false), (true, true)] {
        let fixture = Fixture::guarded();
        let child = fixture.external_guard_target(isolated, matching);
        fixture.events(1).await;
        let result = fixture.correct(&child).await;
        assert!(
            matches!(result.phase, LaunchPhase::Failed { code } if code == "GUARD_TARGET_NOT_UNPROXIED")
        );
        assert!(result.guard_correction.unwrap().stop_nonce.is_none());
        assert!(process::is_running_exact(&child.0.identity).unwrap());
    }
}

#[tokio::test]
async fn guard_origin_alone_cannot_close_an_external_instance() {
    let fixture = Fixture::guarded();
    let child = fixture.external_guard_target(true, false);
    let mut request = fixture.request();
    request.origin = LaunchOrigin::Guard;
    fixture.engine.submit(request.clone()).await.unwrap();
    let result = fixture.result(request.request_id).await;
    assert!(
        matches!(result.phase, LaunchPhase::Failed { code } if code == "INSTANCE_EXTERNALLY_RUNNING")
    );
    assert!(result.guard_correction.is_none());
    assert!(process::is_running_exact(&child.0.identity).unwrap());
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
async fn ordinary_launch_does_not_scan_or_adopt_an_external_process() {
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
    })
    .unwrap();
    let request = fixture.request();
    fixture.engine.submit(request.clone()).await.unwrap();
    let started = confirmed(fixture.result(request.request_id).await);
    assert_ne!(started, external.identity);
    assert!(process::is_running_exact(&started).unwrap());
    assert!(process::is_running_exact(&external.identity).unwrap());
    fixture.events(2).await;
    // A duplicate tool request still reuses its own confirmed session, never
    // the externally created same-image process and never a third launch.
    let reused = fixture.engine.submit(fixture.request()).await.unwrap();
    assert_eq!(reused.id, request.request_id);
    assert_eq!(confirmed(reused), started);
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
    if std::env::var_os("APP_PROXY_ENGINE_AUXILIARY").is_some() {
        let mut environment = app_proxy_core::EnvPatch::default();
        environment.unset.push("APP_PROXY_ENGINE_AUXILIARY".into());
        let child = process::spawn(SpawnSpec {
            exe: std::env::current_exe().unwrap(),
            cwd: std::env::current_dir().unwrap(),
            args: [
                "--ignored",
                "--exact",
                "launch_engine::tests::engine_child",
                "--skip",
                "--type=renderer",
            ]
            .map(Into::into)
            .to_vec(),
            environment,
        })
        .unwrap();
        std::fs::write(
            Path::new(&root).parent().unwrap().join("auxiliary.json"),
            serde_json::to_vec(&child.identity).unwrap(),
        )
        .unwrap();
    }
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
