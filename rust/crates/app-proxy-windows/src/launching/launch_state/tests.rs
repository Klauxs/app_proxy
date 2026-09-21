use super::*;
use crate::{core_state::CoreState, identity};
use app_proxy_core::{core_control::CoreAction, model::*, registry::*};
use std::{fs, os::windows::fs::OpenOptionsExt};

fn setup() -> (tempfile::TempDir, Store, Uuid, Uuid) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("store")).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    let instance = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: std::env::current_exe().unwrap(),
        },
        template_ref: Template::Environment,
    });
    manifest.instances.push(Instance {
        id: instance,
        application_id: app,
        name: "fixture".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec!["private-argument".into()],
        env: SavedEnvironment {
            set: [(
                "PRIVATE_VALUE".into(),
                EnvValue::Literal {
                    value: "private-value".into(),
                },
            )]
            .into(),
            unset: vec![],
        },
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    store.commit(manifest.revision, manifest).unwrap();
    (temp, store, instance, Uuid::new_v4())
}
fn new_request(instance: Uuid) -> LaunchRequest {
    LaunchRequest {
        request_id: Uuid::new_v4(),
        instance_id: instance,
        origin: LaunchOrigin::Interactive,
    }
}
fn binding() -> LaunchBinding {
    let identity = identity::current().unwrap();
    LaunchBinding {
        dependency_digest: [1; 32],
        resource_key: [2; 32],
        executable: identity.image_path,
        image: identity.image_file,
        session_id: identity.session_id,
        network: LaunchNetwork::Direct {},
    }
}
fn prepare(store: &mut Store, instance: Uuid, epoch: Uuid) -> LaunchRequest {
    let request = new_request(instance);
    assert!(store.begin_launch(&request, epoch).unwrap().is_new);
    let mut phase = LaunchPhase::Accepted {};
    for next in [
        LaunchPhase::Resolving {},
        LaunchPhase::CheckingInstance {},
        LaunchPhase::PreparingProxy {},
        LaunchPhase::PreparingData {},
    ] {
        store
            .advance_launch(request.request_id, epoch, &phase, next.clone())
            .unwrap();
        phase = next;
    }
    request
}

#[test]
fn duplicate_requests_share_one_attempt_and_cross_namespace_collisions_fail() {
    let (temp, mut store, instance, epoch) = setup();
    let first = new_request(instance);
    assert!(store.begin_launch(&first, epoch).unwrap().is_new);
    assert!(!store.begin_launch(&first, epoch).unwrap().is_new);
    let mut alias = new_request(instance);
    alias.origin = LaunchOrigin::Guard;
    let admitted = store.begin_launch(&alias, epoch).unwrap();
    assert!(!admitted.is_new);
    assert_eq!(admitted.attempt.id, first.request_id);
    let mut conflict = alias.clone();
    conflict.origin = LaunchOrigin::Shortcut;
    assert!(matches!(
        store.begin_launch(&conflict, epoch),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert!(matches!(
        store.begin_core_request(first.request_id, epoch, &CoreAction::Stop {}),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let rename = ConfigRequest {
        request_id: first.request_id,
        expected_revision: 2,
        action: ConfigAction::RenameInstance {
            instance_id: instance,
            name: "new".into(),
        },
    };
    assert!(matches!(
        store.apply_config(&rename),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let core = new_request(instance);
    store
        .begin_core_request(core.request_id, epoch, &CoreAction::Stop {})
        .unwrap();
    assert!(matches!(
        store.begin_launch(&core, epoch),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let config = new_request(instance);
    store
        .apply_config(&ConfigRequest {
            request_id: config.request_id,
            expected_revision: 2,
            action: ConfigAction::RenameInstance {
                instance_id: instance,
                name: "new".into(),
            },
        })
        .unwrap();
    assert!(matches!(
        store.begin_launch(&config, epoch),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let bytes = fs::read_to_string(store.root().join(PATH)).unwrap();
    assert!(!bytes.contains("private-argument") && !bytes.contains("private-value"));
    drop(store);
    let store = Store::open(&temp.path().join("store")).unwrap();
    assert_eq!(
        store.launch_request(alias.request_id).unwrap().unwrap().id,
        first.request_id
    );
    assert_eq!(store.launch_attempts().unwrap().len(), 1);
}

#[test]
fn terminal_retention_clamps_clock_rollback_and_capacity_preserves_replays() {
    let (_temp, mut store, instance, epoch) = setup();
    let first = new_request(instance);
    store.begin_launch(&first, epoch).unwrap();
    let mut journal = store.read_launch_journal().unwrap();
    journal.attempts[0].accepted_at = now().unwrap() + 60;
    store.write_launch_journal(&journal).unwrap();
    let cancelled = store
        .advance_launch(
            first.request_id,
            epoch,
            &LaunchPhase::Accepted {},
            LaunchPhase::Cancelled {},
        )
        .unwrap();
    assert_eq!(cancelled.finished_at, Some(cancelled.accepted_at));
    let mut journal = store.read_launch_journal().unwrap();
    journal.attempts[0].accepted_at = 1;
    journal.attempts[0].finished_at = Some(1);
    store.write_launch_journal(&journal).unwrap();
    let current = new_request(instance);
    store.begin_launch(&current, epoch).unwrap();
    assert!(store.launch_request(first.request_id).unwrap().is_none());
    let mut journal = store.read_launch_journal().unwrap();
    journal.attempts[0].accepted_at = 1; // Pending never expires by age.
    for _ in 1..REQUEST_LIMIT {
        journal.requests.push(RequestEntry {
            guard_target: None,
            request: new_request(instance),
            attempt_id: current.request_id,
        });
    }
    store.write_launch_journal(&journal).unwrap();
    let before = fs::read(store.root().join(PATH)).unwrap();
    assert!(matches!(
        store.begin_launch(&new_request(instance), epoch),
        Err(Error::Invalid("LAUNCH_REQUEST_LIMIT"))
    ));
    assert_eq!(fs::read(store.root().join(PATH)).unwrap(), before);
    assert!(!store.begin_launch(&current, epoch).unwrap().is_new);
    assert!(
        store
            .request_launch_cancel(current.request_id)
            .unwrap()
            .cancel_requested
    );
}

#[test]
fn cancel_before_spawn_blocks_dispatch_and_after_dispatch_is_only_intent() {
    let (_temp, mut store, instance, epoch) = setup();
    let first = prepare(&mut store, instance, epoch);
    store
        .ready_launch(first.request_id, epoch, binding())
        .unwrap();
    let cancelled = store.request_launch_cancel(first.request_id).unwrap();
    assert!(cancelled.cancel_requested && cancelled.finished_at.is_none());
    assert!(matches!(
        store.dispatch_launch(first.request_id, epoch),
        Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"))
    ));
    store
        .advance_launch(
            first.request_id,
            epoch,
            &LaunchPhase::ReadyToSpawn {},
            LaunchPhase::Cancelled {},
        )
        .unwrap();
    let second = prepare(&mut store, instance, epoch);
    store
        .ready_launch(second.request_id, epoch, binding())
        .unwrap();
    store.dispatch_launch(second.request_id, epoch).unwrap();
    store.request_launch_cancel(second.request_id).unwrap();
    assert!(
        store
            .advance_launch(
                second.request_id,
                epoch,
                &LaunchPhase::SpawnRequested {},
                LaunchPhase::Cancelled {}
            )
            .is_err()
    );
    assert!(
        store
            .advance_launch(
                second.request_id,
                epoch,
                &LaunchPhase::SpawnRequested {},
                LaunchPhase::Failed {
                    code: "UNKNOWN".into()
                }
            )
            .is_err()
    );
    let confirmed = store
        .confirm_launch(second.request_id, epoch, identity::current().unwrap())
        .unwrap();
    assert!(confirmed.cancel_requested && confirmed.reserves_instance());
    assert!(!store.observe_launch_exit(second.request_id).unwrap());
}

#[test]
fn unsynchronized_no_creation_receipt_survives_retention_until_resource_release() {
    let (_temp, mut store, instance, epoch) = setup();
    let request = prepare(&mut store, instance, epoch);
    store
        .ready_launch(request.request_id, epoch, binding())
        .unwrap();
    let dispatch = store.dispatch_launch(request.request_id, epoch).unwrap();
    let process::SpawnFailure::NotCreated { evidence, .. } =
        process::not_dispatched(dispatch, Error::Invalid("FIXTURE"))
    else {
        panic!("expected proof")
    };
    store
        .fail_launch_not_created(request.request_id, epoch, &evidence)
        .unwrap();
    let mut journal = store.read_launch_journal().unwrap();
    journal.attempts[0].accepted_at = 1;
    journal.attempts[0].finished_at = Some(1);
    store.write_launch_journal(&journal).unwrap();
    store.begin_launch(&new_request(instance), epoch).unwrap();
    assert!(
        store
            .launch_request(request.request_id)
            .unwrap()
            .unwrap()
            .resource_pending
    );
    store.finish_resource_sync(request.request_id).unwrap();
    store.begin_launch(&new_request(instance), epoch).unwrap();
    assert!(store.launch_request(request.request_id).unwrap().is_none());
}

#[test]
fn recovery_abandons_only_pre_dispatch_and_unknown_launches_never_expire() {
    for dispatched in [false, true] {
        let (temp, mut store, instance, epoch) = setup();
        let request = prepare(&mut store, instance, epoch);
        store
            .ready_launch(request.request_id, epoch, binding())
            .unwrap();
        if dispatched {
            store.dispatch_launch(request.request_id, epoch).unwrap();
        }
        let mut journal = store.read_launch_journal().unwrap();
        journal.attempts[0].accepted_at = 1;
        store.write_launch_journal(&journal).unwrap();
        drop(store);
        let mut store = Store::open(&temp.path().join("store")).unwrap();
        assert_eq!(
            store
                .launch_request(request.request_id)
                .unwrap()
                .unwrap()
                .phase,
            if dispatched {
                LaunchPhase::SpawnRequested {}
            } else {
                LaunchPhase::ReadyToSpawn {}
            }
        );
        store.recover_launches(Uuid::new_v4()).unwrap();
        let recovered = store.launch_request(request.request_id).unwrap().unwrap();
        if dispatched {
            assert_eq!(recovered.phase, LaunchPhase::Indeterminate {});
            assert!(
                !store
                    .begin_launch(&new_request(instance), Uuid::new_v4())
                    .unwrap()
                    .is_new
            );
            store
                .confirm_launch(request.request_id, epoch, identity::current().unwrap())
                .unwrap();
        } else {
            assert!(matches!(recovered.phase, LaunchPhase::Failed { .. }));
            assert!(
                store
                    .begin_launch(&new_request(instance), Uuid::new_v4())
                    .unwrap()
                    .is_new
            );
            assert!(store.dispatch_launch(request.request_id, epoch).is_err());
        }
    }
}

#[test]
fn failed_atomic_write_does_not_authorize_spawn_and_bad_records_are_preserved() {
    let (_temp, mut store, instance, epoch) = setup();
    let request = prepare(&mut store, instance, epoch);
    store
        .ready_launch(request.request_id, epoch, binding())
        .unwrap();
    let path = store.root().join(PATH);
    let before = fs::read(&path).unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(&path)
        .unwrap();
    assert!(store.dispatch_launch(request.request_id, epoch).is_err());
    assert_eq!(fs::read(&path).unwrap(), before);
    drop(held);
    let mut bad: serde_json::Value = serde_json::from_slice(&before).unwrap();
    bad["attempts"][0]["phase"] = serde_json::json!({"phase":"indeterminate"});
    bad["attempts"][0]["binding"] = serde_json::Value::Null;
    let bytes = serde_json::to_vec(&bad).unwrap();
    store.replace_bounded(PATH, &bytes, LIMIT).unwrap();
    assert!(store.launch_attempts().is_err());
    assert!(store.begin_launch(&new_request(instance), epoch).is_err());
    assert!(store.ensure_core_launch_idle().is_err());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn proxy_permission_validates_generation_and_blocks_destructive_core_transitions() {
    let (_temp, mut store, instance, epoch) = setup();
    let profile = Uuid::new_v4();
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: 29091,
    };
    store
        .apply_config(&ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::CreateManualProfile {
                profile_id: profile,
                name: "fixture".into(),
                endpoint: endpoint.clone(),
                node: ManualProxyInput {
                    protocol: ManualProtocol::Http,
                    host: "proxy.example".into(),
                    port: 8080,
                    credentials: None,
                },
            },
        })
        .unwrap();
    let generation = store.prepare_core_generation(&[profile]).unwrap().id();
    let starting = CoreState::Starting { generation };
    store
        .transition_core_state(&CoreState::Stopped {}, starting.clone())
        .unwrap();
    let running = CoreState::Running {
        generation,
        process: identity::current().unwrap(),
    };
    store
        .transition_core_state(&starting, running.clone())
        .unwrap();
    let request = prepare(&mut store, instance, epoch);
    let mut plan = binding();
    plan.network = LaunchNetwork::Profile {
        profile_id: profile,
        generation,
        endpoint: endpoint.clone(),
    };
    let mut wrong = plan.clone();
    wrong.network = LaunchNetwork::Profile {
        profile_id: profile,
        generation,
        endpoint: Endpoint {
            port: 29092,
            ..endpoint
        },
    };
    assert!(matches!(
        store.ready_launch(request.request_id, epoch, wrong),
        Err(Error::Invalid("LAUNCH_CORE_CHANGED"))
    ));
    store.ready_launch(request.request_id, epoch, plan).unwrap();
    assert!(matches!(
        store.ensure_core_launch_idle(),
        Err(Error::Invalid("CORE_LAUNCH_IN_PROGRESS"))
    ));
    assert!(
        store
            .transition_core_state(&running, CoreState::Stopped {})
            .is_err()
    );
    store.dispatch_launch(request.request_id, epoch).unwrap();
    store.recover_launches(Uuid::new_v4()).unwrap();
    assert!(store.ensure_core_launch_idle().is_err());
    store
        .confirm_launch(request.request_id, epoch, identity::current().unwrap())
        .unwrap();
    assert!(store.ensure_core_launch_idle().is_ok());
    assert!(
        store
            .launch_request(request.request_id)
            .unwrap()
            .unwrap()
            .reserves_instance()
    );
}

#[test]
#[ignore = "native subprocess fixture invoked by confirmed_process_exit_releases_reservation"]
fn holding_child() {
    assert_eq!(std::env::var("APP_PROXY_LAUNCH_CHILD").as_deref(), Ok("1"));
    std::thread::sleep(std::time::Duration::from_secs(30));
}

#[test]
fn confirmed_process_exit_releases_reservation_and_forged_identity_is_unknown() {
    let (temp, mut store, instance, epoch) = setup();
    let request = prepare(&mut store, instance, epoch);
    store
        .ready_launch(request.request_id, epoch, binding())
        .unwrap();
    store.dispatch_launch(request.request_id, epoch).unwrap();
    let mut child = process::spawn(process::SpawnSpec {
        exe: std::env::current_exe().unwrap(),
        args: [
            "--exact",
            "launching::launch_state::tests::holding_child",
            "--ignored",
        ]
        .map(Into::into)
        .to_vec(),
        cwd: temp.path().to_owned(),

        environment: app_proxy_core::EnvPatch {
            set: [("APP_PROXY_LAUNCH_CHILD".into(), "1".into())].into(),
            unset: vec![],
        },
    })
    .unwrap();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        assert!(process::is_running_exact(&child.identity).unwrap());
        let mut forged = child.identity.clone();
        forged.image_file.file_index ^= 1;
        assert!(process::is_running_exact(&forged).is_err());
        assert!(
            store
                .confirm_launch(request.request_id, epoch, forged)
                .is_err()
        );
        store
            .confirm_launch(request.request_id, epoch, child.identity.clone())
            .unwrap();
        let path = store.root().join("state/launch.json");
        let unchanged = std::fs::read(&path).unwrap();
        assert_eq!(
            store.inspect_launch_process(request.request_id).unwrap(),
            Some(child.identity.clone())
        );
        assert_eq!(std::fs::read(&path).unwrap(), unchanged);
        assert!(!store.observe_launch_exit(request.request_id).unwrap());
        let before = store.launch_request(request.request_id).unwrap().unwrap();
        assert!(matches!(before.phase, LaunchPhase::Confirmed { .. }));
        child.terminate().unwrap();
        assert!(!process::is_running_exact(&child.identity).unwrap());
        assert_eq!(
            store.inspect_launch_process(request.request_id).unwrap(),
            None
        );
        assert_eq!(std::fs::read(&path).unwrap(), unchanged);
        assert!(store.observe_launch_exit(request.request_id).unwrap());
        assert!(
            store
                .begin_launch(&new_request(instance), epoch)
                .unwrap()
                .is_new
        );
        assert!(matches!(
            store
                .launch_request(request.request_id)
                .unwrap()
                .unwrap()
                .phase,
            LaunchPhase::Confirmed { .. }
        ));
    }));
    let _ = child.terminate();
    result.unwrap();
}
