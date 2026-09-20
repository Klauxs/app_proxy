use super::*;
use app_proxy_core::model::*;
use std::{cell::Cell, fs};

struct Fixture {
    store: Store,
    registration: Registration,
    _temp: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("store");
        let mut store = Store::create(&home).unwrap();
        let initial = store.load().unwrap();
        let mut model: Manifest = serde_json::from_slice(
            &fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/manifest.json"))
                .unwrap(),
        )
        .unwrap();
        model.store_id = initial.store_id;
        model.owner_sid = initial.owner_sid.clone();
        model.applications[0].locator = ApplicationLocator::Exe {
            path: temp.path().join("missing fixture.exe"),
        };
        store.commit(1, model).unwrap();
        let registration = Registration {
            store_id: initial.store_id,
            owner_sid: initial.owner_sid,
            home,
            host: temp.path().join("missing release/app-proxy-host.exe"),
        };
        Self {
            store,
            registration,
            _temp: temp,
        }
    }
    fn create(&self) -> Request {
        Request {
            id: Uuid::new_v4(),
            expected_revision: self.store.load().unwrap().revision,
            action: Action::Create,
            expected_creation: None,
        }
    }
    fn remove(&self, create: &Request) -> Request {
        Request {
            id: Uuid::new_v4(),
            expected_revision: self.store.load().unwrap().revision,
            action: Action::Remove,
            expected_creation: Some(create.id),
        }
    }
    fn begin(&mut self, create: &Request) -> Job {
        job(self
            .store
            .begin_login_record(create, Some(self.registration.clone()))
            .unwrap())
    }
    fn reopen(self) -> Self {
        let Self {
            store,
            registration,
            _temp,
        } = self;
        drop(store);
        let store = Store::open(&registration.home).unwrap();
        Self {
            store,
            registration,
            _temp,
        }
    }
}
fn job(preparation: Preparation) -> Job {
    match preparation {
        Preparation::Pending(job) => job,
        _ => panic!("expected pending"),
    }
}
fn execute(job: Job, present: &Cell<bool>, creates: &Cell<usize>) -> Completion {
    job.execute_with(
        |_| Ok(present.get()),
        |_| {
            present.set(true);
            creates.set(creates.get() + 1);
            Ok(())
        },
        |_| {
            present.set(false);
            Ok(())
        },
    )
    .unwrap()
}
fn interrupted() -> Result<()> {
    Err(Error::Invalid("FIXTURE_INTERRUPTED"))
}

#[test]
fn login_intent_native_and_manifest_boundaries_recover_without_recreating_after_removal() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    drop(fixture.begin(&create));
    let mut fixture = fixture.reopen();
    assert!(matches!(
        fixture.store.login_request_status(create.id).unwrap(),
        Some(Status::Pending {
            action: Action::Create
        })
    ));
    let present = Cell::new(false);
    let creates = Cell::new(0);
    // Native registration succeeded, but the worker died before manifest work.
    drop(execute(
        job(fixture.store.resume_login(create.id).unwrap()),
        &present,
        &creates,
    ));
    let mut fixture = fixture.reopen();
    let completion = execute(
        job(fixture.store.resume_login(create.id).unwrap()),
        &present,
        &creates,
    );
    assert!(
        fixture
            .store
            .complete_login_with(completion, interrupted)
            .is_err()
    );
    let mut fixture = fixture.reopen();
    let completion = execute(
        job(fixture.store.resume_login(create.id).unwrap()),
        &present,
        &creates,
    );
    assert!(matches!(
        fixture.store.complete_login(completion).unwrap(),
        Status::Created { revision: 3 }
    ));
    assert_eq!(creates.get(), 1);
    let remove = fixture.remove(&create);
    let completion = execute(
        job(fixture.store.begin_login(&remove, None).unwrap()),
        &present,
        &creates,
    );
    assert!(
        fixture
            .store
            .complete_login_with(completion, interrupted)
            .is_err()
    );
    assert!(!present.get());
    let mut fixture = fixture.reopen();
    let completion = execute(
        job(fixture.store.resume_login(remove.id).unwrap()),
        &present,
        &creates,
    );
    assert!(matches!(
        fixture.store.complete_login(completion).unwrap(),
        Status::Removed { revision: 4 }
    ));
    assert!(matches!(
        fixture.store.begin_login(&create, None).unwrap(),
        Preparation::Complete(Status::Created { .. })
    ));
    assert_eq!(creates.get(), 1);
    assert!(
        fixture
            .store
            .load()
            .unwrap()
            .integrations
            .guard_login_task
            .is_none()
    );
    assert!(fixture.store.login_registration().unwrap().is_none());
}

#[test]
fn login_completion_holds_operation_and_owner_leases_and_merges_unrelated_edits() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    let work = fixture.begin(&create);
    let remove = fixture.remove(&create);
    assert!(matches!(
        fixture.store.begin_login(&remove, None),
        Err(Error::Invalid("GUARD_LOGIN_BUSY"))
    ));
    let completion = execute(work, &Cell::new(false), &Cell::new(0));
    assert!(matches!(
        fixture.store.resume_login(create.id),
        Err(Error::Invalid("GUARD_LOGIN_BUSY"))
    ));
    let mut manifest = fixture.store.load().unwrap();
    manifest.instances[0].name = "unrelated edit".into();
    fixture.store.commit(manifest.revision, manifest).unwrap();
    let home = fixture.registration.home.clone();
    drop(fixture.store);
    assert!(matches!(
        Store::open(&home),
        Err(Error::Invalid("STORE_ALREADY_OWNED"))
    ));
    drop(completion);
    fixture.store = Store::open(&home).unwrap();
    let completion = execute(
        job(fixture.store.resume_login(create.id).unwrap()),
        &Cell::new(true),
        &Cell::new(0),
    );
    assert!(matches!(
        fixture.store.complete_login(completion).unwrap(),
        Status::Created { revision: 4 }
    ));
    assert_eq!(
        fixture.store.load().unwrap().instances[0].name,
        "unrelated edit"
    );
}

#[test]
fn cancelled_login_create_cannot_cross_a_new_registration_at_the_same_revision() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    drop(fixture.begin(&create));
    let remove = fixture.remove(&create);
    let completion = execute(
        job(fixture.store.begin_login(&remove, None).unwrap()),
        &Cell::new(false),
        &Cell::new(0),
    );
    fixture.store.complete_login(completion).unwrap();
    assert!(matches!(
        fixture.store.resume_login(create.id).unwrap(),
        Preparation::Complete(Status::Cancelled {})
    ));
    let next = fixture.create();
    assert_eq!(next.expected_revision, remove.expected_revision);
    drop(fixture.begin(&next));
    let mut stale = remove.clone();
    stale.id = Uuid::new_v4();
    assert!(matches!(
        fixture.store.begin_login(&stale, None),
        Err(Error::Invalid("GUARD_LOGIN_REGISTRATION_CHANGED"))
    ));
    assert_eq!(
        fixture.store.login_registration().unwrap().unwrap().0.id,
        next.id
    );
}

#[test]
fn disabled_guard_refuses_missing_task_recreation_but_allows_existing_task_accounting() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    drop(fixture.begin(&create));
    let mut manifest = fixture.store.load().unwrap();
    for instance in &mut manifest.instances {
        instance.guard.desired = Desired::Disabled;
    }
    fixture.store.commit(manifest.revision, manifest).unwrap();
    let work = job(fixture.store.resume_login(create.id).unwrap());
    assert!(matches!(
        work.execute_with(|_| Ok(false), |_| panic!("must not register"), |_| panic!()),
        Err(Error::Invalid("GUARD_LOGIN_NOT_NEEDED"))
    ));
    let work = job(fixture.store.resume_login(create.id).unwrap());
    let completion = work
        .execute_with(|_| Ok(true), |_| panic!("must not register"), |_| panic!())
        .unwrap();
    fixture.store.complete_login(completion).unwrap();
    assert!(
        fixture
            .store
            .load()
            .unwrap()
            .instances
            .iter()
            .all(|i| i.guard.desired == Desired::Disabled)
    );
}

#[test]
fn login_receipt_failure_and_foreign_metadata_remain_pending_without_overwrite() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    let work = fixture.begin(&create);
    let mut manifest = fixture.store.load().unwrap();
    let mut foreign = fixture.registration.metadata().unwrap();
    foreign.args.push("foreign".into());
    manifest.integrations.guard_login_task = Some(foreign);
    fixture.store.commit(manifest.revision, manifest).unwrap();
    let before = fs::read(fixture.registration.home.join("manifest.json")).unwrap();
    let completion = execute(work, &Cell::new(false), &Cell::new(0));
    assert!(matches!(
        fixture.store.complete_login(completion),
        Err(Error::Invalid("GUARD_LOGIN_METADATA_CONFLICT"))
    ));
    assert_eq!(
        fs::read(fixture.registration.home.join("manifest.json")).unwrap(),
        before
    );
    assert!(matches!(
        fixture.store.login_request_status(create.id).unwrap(),
        Some(Status::Pending { .. })
    ));
}

#[test]
fn malformed_login_history_is_rejected_and_queries_do_not_repair_it() {
    let mut fixture = Fixture::new();
    let create = fixture.create();
    drop(fixture.begin(&create));
    let original = fixture.store.read_login().unwrap();
    for change in 0..4 {
        let mut journal = original.clone();
        match change {
            0 => journal.entries[0].created_revision = Some(999),
            1 => journal.entries[0].registration.store_id = Uuid::new_v4(),
            2 => journal.entries.push(journal.entries[0].clone()),
            _ => journal.entries[0].removed_revision = Some(2),
        }
        let bytes = store::encode(&journal, LIMIT).unwrap();
        let path = fixture.registration.home.join(PATH);
        fs::write(&path, &bytes).unwrap();
        assert!(fixture.store.login_request_status(create.id).is_err());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn login_native_success_survives_manifest_and_receipt_write_failures() {
    for remove in [false, true] {
        for blocked in ["manifest.json", PATH] {
            let mut fixture = Fixture::new();
            let create = fixture.create();
            let present = Cell::new(false);
            let creates = Cell::new(0);
            let work = fixture.begin(&create);
            let (work, id) = if remove {
                let completion = execute(work, &present, &creates);
                fixture.store.complete_login(completion).unwrap();
                let remove = fixture.remove(&create);
                (
                    job(fixture.store.begin_login(&remove, None).unwrap()),
                    remove.id,
                )
            } else {
                (work, create.id)
            };
            let held = OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ)
                .open(fixture.registration.home.join(blocked))
                .unwrap();
            let completion = execute(work, &present, &creates);
            assert_eq!(present.get(), !remove);
            assert!(fixture.store.complete_login(completion).is_err());
            assert!(matches!(
                fixture.store.login_request_status(id).unwrap(),
                Some(Status::Pending { .. })
            ));
            drop(held);
            let mut fixture = fixture.reopen();
            let completion = execute(
                job(fixture.store.resume_login(id).unwrap()),
                &present,
                &creates,
            );
            fixture.store.complete_login(completion).unwrap();
            assert_eq!(creates.get(), 1);
            assert_eq!(
                fixture
                    .store
                    .load()
                    .unwrap()
                    .integrations
                    .guard_login_task
                    .is_some(),
                !remove
            );
        }
    }
}

#[test]
fn login_request_ids_conflict_with_all_other_mutation_namespaces_both_ways() {
    use crate::shortcuts::journal as shortcuts;
    use app_proxy_core::{
        core_control::CoreAction,
        launch::{LaunchOrigin, LaunchRequest},
        registry::{ConfigAction, ConfigRequest},
    };
    let config = |id, revision, instance_id| ConfigRequest {
        request_id: id,
        expected_revision: revision,
        action: ConfigAction::RenameInstance {
            instance_id,
            name: "changed".into(),
        },
    };
    for namespace in 0..4 {
        let mut fixture = Fixture::new();
        let create = fixture.create();
        let instance = fixture.store.load().unwrap().instances[0].id;
        let mut shortcut = shortcuts::Request {
            id: create.id,
            instance_id: instance,
            expected_revision: create.expected_revision,
            action: shortcuts::Action::Create,
            expected_creation: None,
        };
        drop(fixture.begin(&create));
        let result = match namespace {
            0 => fixture
                .store
                .apply_config(&config(create.id, create.expected_revision, instance))
                .map(|_| ()),
            1 => fixture
                .store
                .begin_core_request(create.id, Uuid::new_v4(), &CoreAction::Stop {})
                .map(|_| ()),
            2 => fixture
                .store
                .begin_launch(
                    &LaunchRequest {
                        request_id: create.id,
                        instance_id: instance,
                        origin: LaunchOrigin::Interactive,
                    },
                    Uuid::new_v4(),
                )
                .map(|_| ()),
            _ => fixture.store.apply_shortcut(&shortcut, None).map(|_| ()),
        };
        assert!(matches!(result, Err(Error::Invalid("REQUEST_ID_CONFLICT"))));
        let mut fixture = Fixture::new();
        let mut request = fixture.create();
        let instance = fixture.store.load().unwrap().instances[0].id;
        match namespace {
            0 => {
                fixture
                    .store
                    .apply_config(&config(request.id, request.expected_revision, instance))
                    .unwrap();
            }
            1 => {
                fixture
                    .store
                    .begin_core_request(request.id, Uuid::new_v4(), &CoreAction::Stop {})
                    .unwrap();
            }
            2 => {
                fixture
                    .store
                    .begin_launch(
                        &LaunchRequest {
                            request_id: request.id,
                            instance_id: instance,
                            origin: LaunchOrigin::Interactive,
                        },
                        Uuid::new_v4(),
                    )
                    .unwrap();
            }
            _ => {
                shortcut.id = request.id;
                shortcut.instance_id = instance;
                let icon = fixture.store.root().join("state/icon.ico");
                fs::write(&icon, b"fixture icon").unwrap();
                let plan = shortcuts::Plan {
                    path: fixture.store.root().join("fixture.lnk"),
                    spec: crate::shortcuts::Spec {
                        store_id: fixture.registration.store_id,
                        instance_id: instance,
                        home: fixture.registration.home.clone(),
                        host: fixture.registration.host.clone(),
                        icon,
                    },
                };
                // Native preparation may reject the deliberately absent host,
                // but acceptance is already durable and owns the request ID.
                let _ = fixture.store.apply_shortcut(&shortcut, Some(plan));
                assert!(
                    fixture
                        .store
                        .shortcut_request_status(request.id)
                        .unwrap()
                        .is_some()
                );
            }
        }
        request.expected_revision = fixture.store.load().unwrap().revision;
        assert!(matches!(
            fixture
                .store
                .begin_login_record(&request, Some(fixture.registration.clone())),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
    }
}

#[test]
fn accepted_configuration_is_recovered_before_login_admission() {
    use app_proxy_core::registry::{ConfigAction, ConfigRequest};
    let mut fixture = Fixture::new();
    let stale = fixture.create();
    let instance = fixture.store.load().unwrap().instances[0].id;
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(fixture.registration.home.join("manifest.json"))
        .unwrap();
    assert!(
        fixture
            .store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: stale.expected_revision,
                action: ConfigAction::RenameInstance {
                    instance_id: instance,
                    name: "recovered first".into()
                }
            })
            .is_err()
    );
    drop(held);
    assert!(matches!(
        fixture
            .store
            .begin_login_record(&stale, Some(fixture.registration.clone())),
        Err(Error::Invalid("STALE_MANIFEST_REVISION"))
    ));
    assert_eq!(
        fixture.store.load().unwrap().instances[0].name,
        "recovered first"
    );
    assert!(
        fixture
            .store
            .login_request_status(stale.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn completed_login_replay_is_read_only_while_another_job_and_pending_edit_exist() {
    use app_proxy_core::registry::{ConfigAction, ConfigRequest};
    let mut fixture = Fixture::new();
    let create = fixture.create();
    let completion = execute(fixture.begin(&create), &Cell::new(false), &Cell::new(0));
    fixture.store.complete_login(completion).unwrap();
    let remove = fixture.remove(&create);
    let _work = job(fixture.store.begin_login(&remove, None).unwrap());
    let manifest = fixture.store.load().unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(fixture.registration.home.join("manifest.json"))
        .unwrap();
    assert!(
        fixture
            .store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: manifest.revision,
                action: ConfigAction::RenameInstance {
                    instance_id: manifest.instances[0].id,
                    name: "pending other".into()
                }
            })
            .is_err()
    );
    let before = fs::read(fixture.registration.home.join("manifest.json")).unwrap();
    for _ in 0..2 {
        assert!(matches!(
            fixture.store.begin_login(&create, None).unwrap(),
            Preparation::Complete(Status::Created { revision: 3 })
        ));
        assert!(matches!(
            fixture.store.resume_login(create.id).unwrap(),
            Preparation::Complete(Status::Created { revision: 3 })
        ));
        assert_eq!(
            fs::read(fixture.registration.home.join("manifest.json")).unwrap(),
            before
        );
    }
    let mut conflict = create;
    conflict.expected_revision += 1;
    assert!(matches!(
        fixture.store.begin_login(&conflict, None),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    drop(held);
}

#[test]
fn full_login_history_reserves_completion_space_and_does_not_block_existing_removal() {
    let mut fixture = Fixture::new();
    let first = fixture.create();
    let completion = execute(fixture.begin(&first), &Cell::new(false), &Cell::new(0));
    fixture.store.complete_login(completion).unwrap();
    let remove = fixture.remove(&first);
    let completion = execute(
        job(fixture.store.begin_login(&remove, None).unwrap()),
        &Cell::new(true),
        &Cell::new(0),
    );
    fixture.store.complete_login(completion).unwrap();
    let mut journal = fixture.store.read_login().unwrap();
    let template = journal.entries[0].clone();
    journal.entries.clear();
    for _ in 0..ENTRIES - 1 {
        let mut entry = template.clone();
        entry.create.id = Uuid::new_v4();
        let removal = entry.removal.as_mut().unwrap();
        removal.id = Uuid::new_v4();
        removal.expected_creation = Some(entry.create.id);
        entry.registration.host = r"C:\app-proxy-host.exe".into();
        journal.entries.push(entry);
    }
    let current = fixture.create();
    journal.entries.push(Entry {
        create: current.clone(),
        registration: fixture.registration.clone(),
        created_revision: None,
        removal: None,
        removed_revision: None,
        removed_at: None,
    });
    let base = store::encode(&journal, LIMIT).unwrap().len();
    let padding = (LIMIT - base - 1024) / (ENTRIES - 1);
    for entry in journal.entries.iter_mut().take(ENTRIES - 1) {
        // Keep the fixed host basename while adding equally sized directory text.
        entry.registration.host = format!(
            "C:\\{}\\app-proxy-host.exe",
            "x".repeat(padding.saturating_sub(2))
        )
        .into();
    }
    reserve_capacity(&journal).unwrap();
    fixture.store.write_login(&journal).unwrap();
    assert!(store::encode(&journal, LIMIT).unwrap().len() > LIMIT - 2048);
    let completion = execute(
        job(fixture.store.resume_login(current.id).unwrap()),
        &Cell::new(false),
        &Cell::new(0),
    );
    fixture.store.complete_login(completion).unwrap();
    let remove = fixture.remove(&current);
    let completion = execute(
        job(fixture.store.begin_login(&remove, None).unwrap()),
        &Cell::new(true),
        &Cell::new(0),
    );
    fixture.store.complete_login(completion).unwrap();
    let next = fixture.create();
    assert!(matches!(
        fixture
            .store
            .begin_login_record(&next, Some(fixture.registration.clone())),
        Err(Error::Invalid("GUARD_LOGIN_RECORD_LIMIT"))
    ));
    assert!(
        fixture
            .store
            .load()
            .unwrap()
            .integrations
            .guard_login_task
            .is_none()
    );
}

#[test]
#[ignore = "creates and removes one owned UUID login task across journal interruptions; never runs it"]
fn native_login_journal_recovers_existing_registration_and_completed_removal() {
    let mut fixture = Fixture::new();
    fixture.registration.host = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("app-proxy-host.exe");
    assert!(fixture.registration.host.is_file());
    let record = fixture.registration.clone();
    println!("LOGIN JOURNAL FIXTURE: {}", record.spec().unwrap().name);
    struct Cleanup(Registration);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.remove_idle();
        }
    }
    let cleanup = Cleanup(record.clone());
    let create = fixture.create();
    // The installer admission is synthetic; the Task Scheduler operation,
    // recovery verification and removal below are the real Windows backend.
    let completion = fixture
        .begin(&create)
        .execute_with(
            Registration::exists_verified,
            |r| super::super::register_spec(&r.spec()?),
            Registration::remove_idle,
        )
        .unwrap();
    drop(completion);
    let mut fixture = fixture.reopen();
    assert!(record.exists_verified().unwrap());
    let completion = job(fixture.store.resume_login(create.id).unwrap())
        .execute(None)
        .unwrap();
    fixture.store.complete_login(completion).unwrap();
    let remove = fixture.remove(&create);
    let completion = job(fixture.store.begin_login(&remove, None).unwrap())
        .execute(None)
        .unwrap();
    assert!(
        fixture
            .store
            .complete_login_with(completion, interrupted)
            .is_err()
    );
    let mut fixture = fixture.reopen();
    let completion = job(fixture.store.resume_login(remove.id).unwrap())
        .execute(None)
        .unwrap();
    fixture.store.complete_login(completion).unwrap();
    assert!(!record.exists_verified().unwrap());
    assert!(fixture.store.login_registration().unwrap().is_none());
    drop(cleanup);
}
