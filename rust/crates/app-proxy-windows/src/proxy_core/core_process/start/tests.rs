use super::*;
use app_proxy_core::{
    core_control::{CoreAction, CoreOutcome},
    model::*,
    registry::*,
};
use std::time::Duration;

fn configured() -> (tempfile::TempDir, Store, CoreGeneration, Uuid) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("store")).unwrap();
    let id = Uuid::new_v4();
    store
        .apply_config(&ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 1,
            action: ConfigAction::CreateManualProfile {
                profile_id: id,
                name: "fixture".into(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 29457,
                },
                node: ManualProxyInput {
                    protocol: ManualProtocol::Http,
                    host: "proxy.invalid".into(),
                    port: 80,
                    credentials: None,
                },
            },
        })
        .unwrap();
    let generation = store.prepare_core_generation(&[id]).unwrap();
    (temp, store, generation, id)
}

#[test]
#[ignore = "child process fixture"]
fn sleeping_child() {
    std::thread::sleep(Duration::from_secs(30));
}

fn child(prepared: PreparedCore, cwd: &Path) -> CoreProcess {
    prepared
        .spawn_words(
            &[
                OsStr::new("--exact"),
                OsStr::new("core_process::start::tests::sleeping_child"),
                OsStr::new("--ignored"),
                OsStr::new("--test-threads=1"),
            ],
            cwd,
        )
        .unwrap()
}

#[test]
fn witness_recovers_exact_live_child_after_all_creation_handles_close() {
    let (temp, mut store, generation, profile) = configured();
    let request = Uuid::new_v4();
    let action = CoreAction::Start {
        profiles: vec![profile],
        required: profile,
    };
    store
        .begin_core_request(request, Uuid::new_v4(), &action)
        .unwrap();
    let prepared = prepare(&mut store, &std::env::current_exe().unwrap(), &generation)
        .unwrap()
        .bind_request(&mut store, request, action)
        .unwrap();
    let starting = CoreState::Starting {
        generation: generation.id(),
    };
    store
        .transition_core_state(&CoreState::Stopped {}, starting.clone())
        .unwrap();
    let process = child(prepared, temp.path());
    let expected = process.identity().clone();
    drop(process);
    drop(store);
    let mut store = Store::open(&temp.path().join("store")).unwrap();
    let recovery = Uuid::new_v4();
    let unrelated = Uuid::new_v4();
    store
        .begin_core_request(
            recovery,
            Uuid::new_v4(),
            &CoreAction::RecoverStart {
                generation: generation.id(),
            },
        )
        .unwrap();
    store
        .begin_core_request(
            unrelated,
            Uuid::new_v4(),
            &CoreAction::RecoverStart {
                generation: Uuid::new_v4(),
            },
        )
        .unwrap();
    let recovered = store.inspect_core_start(generation.id()).unwrap().unwrap();
    assert_eq!(recovered.identity(), &expected);
    assert!(recovered.is_running().unwrap());
    store
        .transition_core_state(
            &starting,
            CoreState::Running {
                generation: generation.id(),
                process: expected.clone(),
            },
        )
        .unwrap();
    store
        .resolve_core_start_request(generation.id(), Some(expected.clone()))
        .unwrap();
    assert!(
        matches!(store.core_request_status(request).unwrap(), Some(crate::core_requests::CoreRequestPhase::Complete { outcome: CoreOutcome::Reconciled { process: Some(p), .. }, .. }) if p == expected)
    );
    store
        .resolve_core_start_recovery_receipts(
            generation.id(),
            CoreOutcome::Reconciled {
                generation: generation.id(),
                process: Some(expected.clone()),
            },
            Uuid::new_v4(),
        )
        .unwrap();
    assert!(matches!(
        store.core_request_status(recovery).unwrap(),
        Some(crate::core_requests::CoreRequestPhase::Complete {
            outcome: CoreOutcome::Reconciled { .. },
            ..
        })
    ));
    assert!(matches!(
        store.core_request_status(unrelated).unwrap(),
        Some(crate::core_requests::CoreRequestPhase::Pending { .. })
    ));
    recovered.stop().unwrap();
    assert!(store.inspect_core_start(generation.id()).unwrap().is_none());
}

#[test]
fn no_spawn_proves_absence_but_missing_or_changed_witness_never_adopts() {
    let (temp, mut store, generation, _) = configured();
    assert!(matches!(
        store.inspect_core_start(generation.id()),
        Err(Error::Invalid("CORE_START_WITNESS_MISSING"))
    ));
    let prepared = prepare(&mut store, &std::env::current_exe().unwrap(), &generation).unwrap();
    drop(prepared);
    assert!(store.inspect_core_start(generation.id()).unwrap().is_none());
    let prepared = prepare(&mut store, &std::env::current_exe().unwrap(), &generation).unwrap();
    let process = child(prepared, temp.path());
    let header = store.load().unwrap();
    let mut witness: Witness = store::decode(
        &store::read_protected(&store.root().join(PATH), &header.owner_sid, LIMIT).unwrap(),
    )
    .unwrap();
    witness.image.file_index ^= 1;
    store
        .replace_bounded(PATH, &store::encode(&witness, LIMIT).unwrap(), LIMIT)
        .unwrap();
    assert!(matches!(
        store.inspect_core_start(generation.id()),
        Err(Error::Invalid("CORE_JOB_PROCESS_UNCONFIRMED"))
    ));
    assert!(process.is_running().unwrap());
    process.stop().unwrap();
}

#[test]
#[ignore = "creator crash fixture"]
fn exiting_creator() {
    let root = PathBuf::from(std::env::var_os("APP_PROXY_START_FIXTURE_ROOT").unwrap());
    let id: Uuid = std::env::var("APP_PROXY_START_FIXTURE_GENERATION")
        .unwrap()
        .parse()
        .unwrap();
    let mut store = Store::open(&root).unwrap();
    let generation = store.open_core_generation(id).unwrap();
    let prepared = prepare(&mut store, &std::env::current_exe().unwrap(), &generation).unwrap();
    store
        .transition_core_state(
            &CoreState::Stopped {},
            CoreState::Starting { generation: id },
        )
        .unwrap();
    drop(child(prepared, root.parent().unwrap()));
    // Exit without writing Running. OS closes all creator handles.
    std::process::exit(0);
}

#[test]
fn named_job_remains_recoverable_after_creator_process_exits() {
    let (temp, store, generation, _) = configured();
    let id = generation.id();
    drop(store);
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "core_process::start::tests::exiting_creator",
            "--ignored",
            "--test-threads=1",
        ])
        .env("APP_PROXY_START_FIXTURE_ROOT", temp.path().join("store"))
        .env("APP_PROXY_START_FIXTURE_GENERATION", id.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    let store = Store::open(&temp.path().join("store")).unwrap();
    assert_eq!(
        store.core_state().unwrap(),
        CoreState::Starting { generation: id }
    );
    let recovered = store
        .inspect_core_start(id)
        .unwrap()
        .expect("orphaned core survives creator");
    assert!(recovered.is_running().unwrap());
    recovered.stop().unwrap();
}

#[test]
fn multiple_job_members_are_preserved_and_never_adopted_as_the_original() {
    let (temp, mut store, generation, _) = configured();
    let first = prepare(&mut store, &std::env::current_exe().unwrap(), &generation).unwrap();
    let witness: Witness = store::decode(&store::encode(&first.witness, LIMIT).unwrap()).unwrap();
    // SAFETY: duplicate our owned fixture handle; no external process is used.
    let job = unsafe {
        let mut duplicate = ptr::null_mut();
        assert_ne!(
            DuplicateHandle(
                GetCurrentProcess(),
                first.job.as_raw_handle(),
                GetCurrentProcess(),
                &mut duplicate,
                0,
                0,
                DUPLICATE_SAME_ACCESS
            ),
            0
        );
        OwnedHandle::from_raw_handle(duplicate)
    };
    let second = PreparedCore { job, witness };
    let first = child(first, temp.path());
    let second = child(second, temp.path());
    assert!(matches!(
        store.inspect_core_start(generation.id()),
        Err(Error::Invalid("CORE_JOB_MEMBERS_UNCONFIRMED"))
    ));
    assert!(first.is_running().unwrap());
    assert!(second.is_running().unwrap());
    first.stop().unwrap();
    second.stop().unwrap();
}
