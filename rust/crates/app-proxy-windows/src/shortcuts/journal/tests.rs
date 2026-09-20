use super::*;
use crate::config_transaction::ConfigOutcome;
use app_proxy_core::{model::*, registry::ConfigRequest};

fn fixture() -> (tempfile::TempDir, Store, Request, Plan) {
    let (temp, mut spec, path) = super::super::tests::setup();
    let mut store = Store::create(&spec.home).unwrap();
    let mut manifest = store.load().unwrap();
    let application_id = Uuid::new_v4();
    manifest.applications.push(Application {
        id: application_id,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: spec.host.clone(),
        },
        template_ref: Template::Codex,
    });
    manifest.instances.push(Instance {
        id: spec.instance_id,
        application_id,
        name: "fixture".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec![],
        env: SavedEnvironment::default(),
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    spec.store_id = manifest.store_id;
    let expected_revision = store.commit(manifest.revision, manifest).unwrap();
    let request = Request {
        id: Uuid::new_v4(),
        instance_id: spec.instance_id,
        expected_revision,
        action: Action::Create,
        expected_creation: None,
    };
    (temp, store, request, Plan { path, spec })
}
fn interrupt(at: Point) -> impl Fn(Point) -> Result<()> {
    move |point| {
        if point == at {
            Err(Error::Invalid("TEST_INTERRUPTION"))
        } else {
            Ok(())
        }
    }
}
fn reopen(store: Store) -> Store {
    let root = store.root().to_owned();
    drop(store);
    Store::open(&root).unwrap()
}
fn removal(store: &Store, request: &Request) -> Request {
    Request {
        id: Uuid::new_v4(),
        instance_id: request.instance_id,
        expected_revision: store.load().unwrap().revision,
        action: Action::Remove,
        expected_creation: None,
    }
}

fn repair_fixture() -> (tempfile::TempDir, Store, Request, Plan, Request) {
    let (temp, mut store, create, mut plan) = fixture();
    plan.spec.icon = crate::shortcuts::icons::cache_bytes(&store, b"fixture icon").unwrap();
    store.apply_shortcut(&create, Some(plan.clone())).unwrap();
    let repair = Request {
        id: Uuid::new_v4(),
        expected_revision: store.load().unwrap().revision,
        action: Action::Repair,
        expected_creation: Some(create.id),
        ..create.clone()
    };
    (temp, store, create, plan, repair)
}

#[test]
fn shortcut_check_distinguishes_missing_changed_and_pending_without_writes() {
    let (_temp, mut store, create, plan, repair) = repair_fixture();
    let journal = std::fs::read(store.root().join(PATH)).unwrap();
    assert_eq!(
        store.check_shortcut(create.instance_id).unwrap().state,
        CheckState::Verified
    );
    let bytes = std::fs::read(&plan.path).unwrap();
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    assert_eq!(
        store.check_shortcut(create.instance_id).unwrap().state,
        CheckState::Missing
    );
    assert_eq!(std::fs::read(store.root().join(PATH)).unwrap(), journal);
    std::fs::write(&plan.path, &bytes).unwrap();
    // Same bytes do not grant ownership of a different file.
    assert_eq!(
        store.check_shortcut(create.instance_id).unwrap().state,
        CheckState::Blocked
    );
    assert!(store.apply_shortcut(&repair, None).is_err());
    assert_eq!(std::fs::read(&plan.path).unwrap(), bytes);
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    assert!(
        store
            .apply_shortcut_with(&repair, None, interrupt(Point::Accepted))
            .is_err()
    );
    let view = store.check_shortcut(create.instance_id).unwrap();
    assert_eq!(view.state, CheckState::Pending);
    assert_eq!(view.request_id, Some(repair.id));
    assert!(!plan.path.exists());
}

#[test]
fn shortcut_repair_recovers_every_publication_boundary_and_keeps_creation_history() {
    for point in [
        Point::Accepted,
        Point::Staged,
        Point::StageRecorded,
        Point::Published,
        Point::Completed,
    ] {
        let (_temp, mut store, create, plan, repair) = repair_fixture();
        super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
        assert!(
            store
                .apply_shortcut_with(&repair, None, interrupt(point))
                .is_err()
        );
        let before = plan.path.exists();
        store = reopen(store);
        store.shortcut_request_status(repair.id).unwrap();
        assert_eq!(plan.path.exists(), before);
        assert!(matches!(
            store.apply_shortcut(&repair, None).unwrap(),
            Status::Repaired { .. }
        ));
        assert_eq!(
            store.check_shortcut(create.instance_id).unwrap().state,
            CheckState::Verified
        );
        assert!(matches!(
            store.shortcut_request_status(create.id).unwrap(),
            Some(Status::Created { revision: 3, .. })
        ));
        assert_eq!(store.load().unwrap().revision, 3);
        let journal = std::fs::read(store.root().join(PATH)).unwrap();
        super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
        store.resume_shortcut(repair.id).unwrap();
        store.resume_shortcut(create.id).unwrap();
        assert!(
            !plan.path.exists(),
            "historical replay must not recreate a link"
        );
        assert_eq!(std::fs::read(store.root().join(PATH)).unwrap(), journal);
        let next = Request {
            id: Uuid::new_v4(),
            ..repair.clone()
        };
        store.apply_shortcut(&next, None).unwrap();
        assert_eq!(
            store.check_shortcut(create.instance_id).unwrap().state,
            CheckState::Verified
        );
        store
            .apply_shortcut(&removal(&store, &create), None)
            .unwrap();
        assert!(!plan.path.exists());
    }
}

#[test]
fn repair_refuses_concurrent_replacement_and_resumes_only_the_original_request() {
    let (_temp, mut store, create, plan, repair) = repair_fixture();
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    assert!(
        store
            .apply_shortcut_with(&repair, None, interrupt(Point::StageRecorded))
            .is_err()
    );
    let pending = store
        .instance_shortcut(create.instance_id)
        .unwrap()
        .unwrap();
    assert_eq!(pending.0.id, repair.id);
    assert!(
        store
            .apply_shortcut(&removal(&store, &create), None)
            .is_err()
    );
    assert!(
        store
            .apply_shortcut(
                &Request {
                    id: Uuid::new_v4(),
                    ..repair.clone()
                },
                None
            )
            .is_err()
    );
    let foreign = b"user replacement must remain";
    std::fs::write(&plan.path, foreign).unwrap();
    assert!(store.resume_shortcut(repair.id).is_err());
    assert_eq!(std::fs::read(&plan.path).unwrap(), foreign);
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    store.resume_shortcut(repair.id).unwrap();
    assert_eq!(
        store.check_shortcut(create.instance_id).unwrap().state,
        CheckState::Verified
    );
    assert!(matches!(
        store.apply_shortcut(
            &Request {
                expected_revision: 2,
                ..repair.clone()
            },
            None
        ),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
}

#[test]
fn repair_checks_revision_registration_assets_and_preserves_intact_file_identity() {
    let (_temp, mut store, create, plan, repair) = repair_fixture();
    let file = identity::file_identity(&plan.path).unwrap();
    assert!(
        store
            .apply_shortcut(
                &Request {
                    expected_revision: 2,
                    ..repair.clone()
                },
                None
            )
            .is_err()
    );
    assert!(
        store
            .apply_shortcut(
                &Request {
                    expected_creation: Some(Uuid::new_v4()),
                    ..repair.clone()
                },
                None
            )
            .is_err()
    );
    store.apply_shortcut(&repair, None).unwrap();
    assert_eq!(identity::file_identity(&plan.path).unwrap(), file);
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    std::fs::remove_file(&plan.spec.host).unwrap();
    let next = Request {
        id: Uuid::new_v4(),
        ..repair
    };
    assert!(store.apply_shortcut(&next, None).is_err());
    assert!(store.shortcut_request_status(next.id).unwrap().is_none());
    assert_eq!(
        store.check_shortcut(create.instance_id).unwrap().state,
        CheckState::Blocked
    );
    assert!(!plan.path.exists());
}

#[test]
fn all_creation_boundaries_recover_explicitly_and_replay_once() {
    for point in [
        Point::Accepted,
        Point::Staged,
        Point::StageRecorded,
        Point::Published,
        Point::ManifestCommitted,
        Point::Completed,
    ] {
        let (_temp, mut store, request, plan) = fixture();
        assert!(
            store
                .apply_shortcut_with(&request, Some(plan.clone()), interrupt(point))
                .is_err()
        );
        let before_open = plan.path.exists();
        store = reopen(store);
        assert_eq!(
            plan.path.exists(),
            before_open,
            "open must not publish: {point:?}"
        );
        let before = store.load().unwrap().revision;
        store.shortcut_request_status(request.id).unwrap();
        assert_eq!(store.load().unwrap().revision, before);
        assert!(matches!(
            store.apply_shortcut(&request, None).unwrap(),
            Status::Created { .. }
        ));
        assert!(plan.path.exists());
        let manifest = store.load().unwrap();
        assert_eq!(manifest.integrations.shortcuts.len(), 1);
        assert_eq!(manifest.revision, 3, "{point:?}");
        let journal = store.read_shortcuts().unwrap();
        let receipt = &journal.entries[0].staged.as_ref().unwrap().receipt;
        super::super::verify(&plan.path, &plan.spec, receipt).unwrap();
        store.apply_shortcut(&request, Some(plan.clone())).unwrap();
        assert_eq!(store.load().unwrap().revision, 3);
        let orphans = std::fs::read_dir(plan.path.parent().unwrap())
            .unwrap()
            .filter_map(|e| {
                let path = e.unwrap().path();
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".app-proxy-stage-")
                    .then_some(path)
            })
            .collect::<Vec<_>>();
        assert_eq!(orphans.len(), usize::from(point == Point::Staged));
        for path in orphans {
            assert_eq!(path.extension().unwrap(), "tmp");
        }
    }
}

#[test]
fn all_removal_boundaries_preserve_historical_creation_and_allow_new_creation() {
    for point in [
        Point::Accepted,
        Point::Published,
        Point::ManifestCommitted,
        Point::Completed,
    ] {
        let (_temp, mut store, create, plan) = fixture();
        store.apply_shortcut(&create, Some(plan.clone())).unwrap();
        let remove = removal(&store, &create);
        assert!(
            store
                .apply_shortcut_with(&remove, None, interrupt(point))
                .is_err()
        );
        store = reopen(store);
        assert!(matches!(
            store.apply_shortcut(&remove, None).unwrap(),
            Status::Removed { .. }
        ));
        assert!(!plan.path.exists());
        assert!(store.load().unwrap().integrations.shortcuts.is_empty());
        assert_eq!(store.load().unwrap().revision, 4);
        assert!(matches!(
            store.apply_shortcut(&create, Some(plan.clone())).unwrap(),
            Status::Created { revision: 3, .. }
        ));
        assert!(!plan.path.exists(), "terminal replay cannot recreate");
        let next = Request {
            id: Uuid::new_v4(),
            expected_revision: 4,
            ..create
        };
        store.apply_shortcut(&next, Some(plan.clone())).unwrap();
        assert!(plan.path.exists());
    }
}

#[test]
fn pending_shortcut_reserves_instance_but_merges_unrelated_config_edits() {
    let (_temp, mut store, request, plan) = fixture();
    store.begin_shortcut(&request, Some(plan.clone())).unwrap();
    let remove = ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 2,
        action: ConfigAction::RemoveInstance {
            instance_id: request.instance_id,
        },
    };
    assert!(
        matches!(store.apply_config(&remove).unwrap(), ConfigOutcome::Rejected { code, .. } if code == "INTEGRATION_CLEANUP_REQUIRED")
    );
    let mut manifest = store.load().unwrap();
    manifest.instances.clear();
    assert!(matches!(
        store.commit(2, manifest),
        Err(Error::Invalid("INTEGRATION_CLEANUP_REQUIRED"))
    ));
    let rename = ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 2,
        action: ConfigAction::RenameInstance {
            instance_id: request.instance_id,
            name: "renamed".into(),
        },
    };
    assert!(matches!(
        store.apply_config(&rename).unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    store.resume_shortcut(request.id).unwrap();
    let manifest = store.load().unwrap();
    assert_eq!(manifest.instances[0].name, "renamed");
    assert_eq!(manifest.revision, 4);
    assert_eq!(manifest.integrations.shortcuts[0].path, plan.path);
}

#[test]
fn modified_or_replaced_owned_link_blocks_removal_without_losing_receipt() {
    let (temp, mut store, create, plan) = fixture();
    store.apply_shortcut(&create, Some(plan.clone())).unwrap();
    let original = std::fs::read(&plan.path).unwrap();
    let remove = removal(&store, &create);
    std::fs::write(&plan.path, b"user modified").unwrap();
    assert!(store.apply_shortcut(&remove, None).is_err());
    store = reopen(store);
    assert!(store.resume_shortcut(remove.id).is_err());
    assert_eq!(std::fs::read(&plan.path).unwrap(), b"user modified");
    assert!(matches!(
        store.shortcut_request_status(remove.id).unwrap(),
        Some(Status::Pending { .. })
    ));
    assert_eq!(store.load().unwrap().integrations.shortcuts.len(), 1);
    std::fs::write(&plan.path, &original).unwrap();
    let saved = temp.path().join("saved.lnk");
    super::super::tests::edit_fixture(|| std::fs::rename(&plan.path, &saved)).unwrap();
    std::fs::write(&plan.path, &original).unwrap();
    assert!(store.resume_shortcut(remove.id).is_err());
    super::super::tests::edit_fixture(|| std::fs::remove_file(&plan.path)).unwrap();
    super::super::tests::edit_fixture(|| std::fs::rename(&saved, &plan.path)).unwrap();
    store.resume_shortcut(remove.id).unwrap();
    assert!(!plan.path.exists());
}

#[test]
fn pending_creation_can_be_cancelled_and_never_removes_foreign_occupant() {
    for point in [Point::Accepted, Point::StageRecorded, Point::Published] {
        let (_temp, mut store, create, plan) = fixture();
        assert!(
            store
                .apply_shortcut_with(&create, Some(plan.clone()), interrupt(point))
                .is_err()
        );
        if point == Point::Accepted {
            std::fs::write(&plan.path, b"later foreign file").unwrap();
        }
        let remove = removal(&store, &create);
        store.begin_shortcut(&remove, None).unwrap();
        assert!(matches!(
            store.resume_shortcut(create.id),
            Err(Error::Invalid("SHORTCUT_REMOVAL_PENDING"))
        ));
        store.resume_shortcut(remove.id).unwrap();
        assert!(matches!(
            store.resume_shortcut(create.id).unwrap(),
            Status::Cancelled { .. }
        ));
        if point == Point::Accepted {
            assert_eq!(std::fs::read(&plan.path).unwrap(), b"later foreign file");
        } else {
            assert!(!plan.path.exists());
        }
        let mut manifest = store.load().unwrap();
        manifest.instances.clear();
        store.commit(manifest.revision, manifest).unwrap();
    }
}

#[test]
fn missing_staged_file_can_be_reprepared_but_changed_stage_is_retained() {
    let (_temp, mut store, create, plan) = fixture();
    assert!(
        store
            .apply_shortcut_with(&create, Some(plan.clone()), interrupt(Point::StageRecorded))
            .is_err()
    );
    let journal = store.read_shortcuts().unwrap();
    let staged = journal.entries[0].staged.as_ref().unwrap();
    std::fs::write(&staged.path, b"modified stage").unwrap();
    assert!(store.resume_shortcut(create.id).is_err());
    assert_eq!(std::fs::read(&staged.path).unwrap(), b"modified stage");
    std::fs::remove_file(&staged.path).unwrap();
    store.resume_shortcut(create.id).unwrap();
    assert!(plan.path.exists());
}

#[test]
fn malformed_or_foreign_journal_is_not_replaced_and_blocks_instance_deletion() {
    let (_temp, mut store, create, plan) = fixture();
    store.begin_shortcut(&create, Some(plan)).unwrap();
    let mut journal = store.read_shortcuts().unwrap();
    journal.store_id = Uuid::new_v4();
    let bytes = serde_json::to_vec(&journal).unwrap();
    store.replace_bounded(PATH, &bytes, JOURNAL_LIMIT).unwrap();
    assert!(store.shortcut_request_status(create.id).is_err());
    assert!(store.resume_shortcut(create.id).is_err());
    let mut manifest = store.load().unwrap();
    manifest.instances.clear();
    assert!(store.commit(manifest.revision, manifest).is_err());
    assert_eq!(std::fs::read(store.root().join(PATH)).unwrap(), bytes);
}

#[test]
fn shortcut_request_ids_conflict_in_both_directions_with_other_namespaces() {
    use app_proxy_core::{
        core_control::CoreAction,
        launch::{LaunchOrigin, LaunchRequest},
    };
    let (_temp, mut store, create, plan) = fixture();
    store.begin_shortcut(&create, Some(plan.clone())).unwrap();
    let config = |id| ConfigRequest {
        request_id: id,
        expected_revision: 2,
        action: ConfigAction::RenameInstance {
            instance_id: create.instance_id,
            name: "renamed".into(),
        },
    };
    let launch = |id| LaunchRequest {
        request_id: id,
        instance_id: create.instance_id,
        origin: LaunchOrigin::Interactive,
    };
    assert!(matches!(
        store.apply_config(&config(create.id)),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert!(matches!(
        store.replay_config(&config(create.id)),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert!(matches!(
        store.begin_core_request(create.id, Uuid::new_v4(), &CoreAction::Stop {}),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert!(matches!(
        store.begin_launch(&launch(create.id), Uuid::new_v4()),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    for kind in 0..3 {
        let id = Uuid::new_v4();
        match kind {
            0 => {
                store.apply_config(&config(id)).unwrap();
            }
            1 => {
                store
                    .begin_core_request(id, Uuid::new_v4(), &CoreAction::Stop {})
                    .unwrap();
            }
            _ => {
                store.begin_launch(&launch(id), Uuid::new_v4()).unwrap();
            }
        }
        let other = Request {
            id,
            expected_revision: store.load().unwrap().revision,
            ..create.clone()
        };
        assert!(matches!(
            store.apply_shortcut(&other, Some(plan.clone())),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
    }
    let changed = Request {
        action: Action::Remove,
        ..create
    };
    assert!(matches!(
        store.apply_shortcut(&changed, None),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
}

#[test]
fn ownership_does_not_expire_and_record_limit_never_prevents_cleanup() {
    let (_temp, mut store, create, plan) = fixture();
    store.apply_shortcut(&create, Some(plan.clone())).unwrap();
    let remove = removal(&store, &create);
    // Fill history with distinct, well-formed retained terminal requests.
    let mut journal = store.read_shortcuts().unwrap();
    let mut historical = journal.entries[0].clone();
    historical.removal = Some(remove.clone());
    historical.removed_revision = Some(3);
    historical.removed_at = Some(now().unwrap());
    for _ in 1..ENTRY_LIMIT {
        historical.create.id = Uuid::new_v4();
        historical.removal.as_mut().unwrap().id = Uuid::new_v4();
        journal.entries.push(historical.clone());
    }
    store.write_shortcuts(&journal).unwrap();
    store.apply_shortcut(&remove, None).unwrap();
    assert!(!plan.path.exists());
    // Active ownership survives arbitrarily old removed history being pruned.
    let mut journal = store.read_shortcuts().unwrap();
    for entry in &mut journal.entries {
        entry.removed_at = Some(0);
    }
    store.write_shortcuts(&journal).unwrap();
    let next = Request {
        id: Uuid::new_v4(),
        expected_revision: store.load().unwrap().revision,
        ..create
    };
    store.apply_shortcut(&next, Some(plan)).unwrap();
    let journal = store.read_shortcuts().unwrap();
    assert_eq!(journal.entries.len(), 1);
    assert_eq!(journal.entries[0].create.id, next.id);
    assert!(journal.entries[0].staged.is_some());
}

#[test]
fn impossible_historical_revisions_are_preserved_as_errors() {
    let (_temp, mut store, create, plan) = fixture();
    store.apply_shortcut(&create, Some(plan)).unwrap();
    let remove = removal(&store, &create);
    store.apply_shortcut(&remove, None).unwrap();
    let valid = serde_json::to_vec(&store.read_shortcuts().unwrap()).unwrap();
    for case in 0..4 {
        let mut journal: Journal = serde_json::from_slice(&valid).unwrap();
        let entry = &mut journal.entries[0];
        match case {
            0 => entry.created_revision = Some(5),
            1 => entry.removed_revision = Some(5),
            2 => entry.removed_revision = Some(2),
            _ => entry.create.expected_revision = 5,
        }
        let bytes = serde_json::to_vec(&journal).unwrap();
        store.replace_bounded(PATH, &bytes, JOURNAL_LIMIT).unwrap();
        assert!(store.shortcut_request_status(create.id).is_err());
        assert!(store.resume_shortcut(remove.id).is_err());
        assert_eq!(std::fs::read(store.root().join(PATH)).unwrap(), bytes);
    }
}

#[test]
fn byte_capacity_is_reserved_before_acceptance_for_completion_and_cleanup() {
    let (_temp, mut store, create, plan) = fixture();
    store.begin_shortcut(&create, Some(plan.clone())).unwrap();
    let mut journal = store.read_shortcuts().unwrap();
    let mut history = journal.entries.pop().unwrap();
    history.create.expected_revision = 1;
    history.removal = Some(Request {
        action: Action::Remove,
        ..create.clone()
    });
    history.removed_revision = Some(2);
    history.removed_at = Some(now().unwrap());
    // Historical cancelled entries have no file receipt to fabricate. Long
    // valid icon paths exercise the byte bound independently of entry count.
    history.plan.spec.icon = PathBuf::from(format!("C:\\{}.ico", "x".repeat(30000)));
    let base = serde_json::to_vec_pretty(&journal).unwrap().len();
    journal.entries.push(history.clone());
    let item_size = serde_json::to_vec_pretty(&journal).unwrap().len() - base;
    journal.entries.clear();
    let desired = JOURNAL_LIMIT - 1500;
    let count = (desired - base) / (item_size + 1);
    for _ in 0..count {
        history.create.id = Uuid::new_v4();
        history.removal.as_mut().unwrap().id = Uuid::new_v4();
        journal.entries.push(history.clone());
    }
    history.create.id = Uuid::new_v4();
    history.removal.as_mut().unwrap().id = Uuid::new_v4();
    history.plan.spec.icon = PathBuf::from("C:\\padding.ico");
    journal.entries.push(history);
    let current = serde_json::to_vec_pretty(&journal).unwrap().len();
    let extra = desired.checked_sub(current).unwrap();
    journal.entries.last_mut().unwrap().plan.spec.icon =
        PathBuf::from(format!("C:\\padding{}.ico", "x".repeat(extra)));
    store.write_shortcuts(&journal).unwrap();
    let before = std::fs::read(store.root().join(PATH)).unwrap();
    assert_eq!(before.len(), desired);
    assert!(matches!(
        store.apply_shortcut(&create, Some(plan.clone())),
        Err(Error::Invalid("SHORTCUT_RECORD_LIMIT"))
    ));
    assert_eq!(std::fs::read(store.root().join(PATH)).unwrap(), before);
    assert!(store.shortcut_request_status(create.id).unwrap().is_none());
    assert!(!plan.path.exists());
    journal.entries.remove(0);
    store.write_shortcuts(&journal).unwrap();
    store.apply_shortcut(&create, Some(plan.clone())).unwrap();
    let remove = removal(&store, &create);
    store.apply_shortcut(&remove, None).unwrap();
    assert!(!plan.path.exists());
}

#[test]
fn removal_binds_displayed_creation_even_when_pending_operations_share_revision() {
    let (_temp, mut store, first, plan) = fixture();
    store.begin_shortcut(&first, Some(plan.clone())).unwrap();
    let old_remove = Request {
        expected_creation: Some(first.id),
        ..removal(&store, &first)
    };
    let cancel = Request {
        id: Uuid::new_v4(),
        ..old_remove.clone()
    };
    store.apply_shortcut(&cancel, None).unwrap();
    assert_eq!(store.load().unwrap().revision, 2);
    let next = Request {
        id: Uuid::new_v4(),
        ..first
    };
    store.begin_shortcut(&next, Some(plan)).unwrap();
    assert!(matches!(
        store.apply_shortcut(&old_remove, None),
        Err(Error::Invalid("SHORTCUT_REGISTRATION_CHANGED"))
    ));
    let (active, _) = store.instance_shortcut(next.instance_id).unwrap().unwrap();
    assert_eq!(active.id, next.id);
    store.resume_shortcut(next.id).unwrap();
}
