#![cfg(windows)]
use app_proxy_core::{
    model::*,
    registry::{ConfigAction, ConfigRequest, SubscriptionEdit},
    subscription,
};
use app_proxy_windows::{
    Error, config_transaction::ConfigOutcome, core_state::CoreState, store::Store,
    subscription_stage::ImportRequest,
};
use uuid::Uuid;

fn body(a: &str, b: &str) -> subscription::Parsed {
    subscription::parse(&format!(
        "trojan://{a}@edge.invalid:443#A\ntrojan://{b}@edge.invalid:443#B"
    ))
    .unwrap()
}
fn input() -> ImportRequest {
    ImportRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        profile_id: Uuid::new_v4(),
        name: "subscription".into(),
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 18998,
        },
        url: "https://source.invalid/?token=fixture".into(),
        selected_name: "A".into(),
    }
}
fn apply(store: &mut Store, request: &ConfigRequest) {
    assert!(matches!(
        store.apply_config(request).unwrap(),
        ConfigOutcome::Applied { .. }
    ));
}
fn create() -> (tempfile::TempDir, Store, ImportRequest) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("store")).unwrap();
    let input = input();
    let staged = store
        .stage_subscription_import(&input, &body("first", "second"))
        .unwrap();
    apply(&mut store, &staged.request);
    (temp, store, input)
}
fn nodes(store: &Store) -> Vec<subscription::saved::SavedNode> {
    let manifest = store.load().unwrap();
    let ProxySource::Subscription { nodes, .. } = manifest.profiles[0].source.clone() else {
        panic!()
    };
    nodes
}
fn refresh(
    store: &mut Store,
    input: &ImportRequest,
    parsed: &subscription::Parsed,
) -> app_proxy_windows::subscription_stage::StagedSubscription {
    let manifest = store.load().unwrap();
    let ProxySource::Subscription {
        revision,
        url_secret_id,
        ..
    } = manifest.profiles[0].source
    else {
        panic!()
    };
    store
        .stage_subscription_refresh(
            Uuid::new_v4(),
            input.profile_id,
            revision,
            url_secret_id,
            parsed,
        )
        .unwrap()
}

#[test]
fn import_stages_only_secrets_and_replays_identical_inputs_but_rejects_changed_secrets() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("store")).unwrap();
    let mut input = input();
    let parsed = body("first", "second");
    let staged = store.stage_subscription_import(&input, &parsed).unwrap();
    assert_eq!(staged.changes.added, vec!["A", "B"]);
    assert!(store.load().unwrap().profiles.is_empty());
    let repeat = store.stage_subscription_import(&input, &parsed).unwrap();
    assert_eq!(
        serde_json::to_string(&staged.request).unwrap(),
        serde_json::to_string(&repeat.request).unwrap()
    );
    assert_eq!(
        std::fs::read_dir(temp.path().join("store").join("secrets"))
            .unwrap()
            .count(),
        3
    );
    apply(&mut store, &staged.request);
    let repeat = store.stage_subscription_import(&input, &parsed).unwrap();
    apply(&mut store, &repeat.request);
    assert_eq!(store.load().unwrap().revision, 2);
    input.url = "https://source.invalid/?token=changed".into();
    assert!(matches!(
        store.stage_subscription_import(&input, &parsed),
        Err(Error::Invalid("SECRET_ID_CONFLICT"))
    ));
    input.url = "https://source.invalid/?token=fixture".into();
    assert!(matches!(
        store.stage_subscription_import(&input, &body("changed", "second")),
        Err(Error::Invalid("SECRET_ID_CONFLICT"))
    ));
    assert_eq!(store.read_secret(input.request_id).unwrap(), input.url);
}

#[test]
fn refresh_preserves_identity_and_current_selection_and_reuses_unchanged_secrets() {
    let (_temp, mut store, input) = create();
    let before = nodes(&store);
    // An unrelated edit during download is allowed; stage captures the new global revision.
    apply(
        &mut store,
        &ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::RenameProfile {
                profile_id: input.profile_id,
                name: "renamed".into(),
            },
        },
    );
    let parsed = subscription::parse("trojan://third@edge.invalid:443#C\ntrojan://updated@edge.invalid:443#B\ntrojan://first@edge.invalid:443#A").unwrap();
    let staged = store
        .stage_subscription_refresh(
            Uuid::new_v4(),
            input.profile_id,
            1,
            input.request_id,
            &parsed,
        )
        .unwrap();
    assert_eq!(staged.request.expected_revision, 3);
    assert_eq!(staged.changes.added, vec!["C"]);
    assert!(staged.changes.removed.is_empty());
    apply(&mut store, &staged.request);
    let after = nodes(&store);
    assert_eq!(after[2].id, before[0].id);
    assert_eq!(after[2].secret_id, before[0].secret_id);
    assert_eq!(after[1].id, before[1].id);
    assert_ne!(after[1].secret_id, before[1].secret_id);
    assert_eq!(
        store.load().unwrap().profiles[0].selected_node_id,
        before[0].id
    );
    // A later selection during another download is the selection refreshed.
    apply(
        &mut store,
        &ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 4,
            action: ConfigAction::EditSubscriptionProfile {
                profile_id: input.profile_id,
                edit: SubscriptionEdit::Select {
                    expected_source_revision: 2,
                    node_id: before[1].id,
                },
            },
        },
    );
    let changed = refresh(&mut store, &input, &body("first", "updated"));
    assert_eq!(changed.changes.removed, vec!["C"]);
    apply(&mut store, &changed.request);
    assert_eq!(
        store.load().unwrap().profiles[0].selected_node_id,
        before[1].id
    );
    apply(&mut store, &changed.request); // Exact request survives a lost reply.
    assert_eq!(store.load().unwrap().revision, 6);
}

#[test]
fn stale_source_missing_selection_and_duplicate_inputs_leave_manifest_and_secrets_unchanged() {
    let (temp, mut store, input) = create();
    let original = std::fs::read(temp.path().join("store").join("manifest.json")).unwrap();
    let count = std::fs::read_dir(temp.path().join("store").join("secrets"))
        .unwrap()
        .count();
    assert!(matches!(
        store.stage_subscription_refresh(
            Uuid::new_v4(),
            input.profile_id,
            9,
            input.request_id,
            &body("a", "b")
        ),
        Err(Error::Invalid("STALE_SUBSCRIPTION_SOURCE"))
    ));
    assert!(matches!(
        store.stage_subscription_refresh(
            Uuid::new_v4(),
            input.profile_id,
            1,
            Uuid::new_v4(),
            &body("a", "b")
        ),
        Err(Error::Invalid("STALE_SUBSCRIPTION_SOURCE"))
    ));
    let removed = subscription::parse("trojan://second@edge.invalid:443#B").unwrap();
    assert!(matches!(
        store.stage_subscription_refresh(
            Uuid::new_v4(),
            input.profile_id,
            1,
            input.request_id,
            &removed
        ),
        Err(Error::Invalid("SUBSCRIPTION_SELECTED_NODE_REMOVED"))
    ));
    let mut duplicate = body("changed", "second");
    duplicate.nodes[1].name = "A".into();
    assert!(
        store
            .stage_subscription_refresh(
                Uuid::new_v4(),
                input.profile_id,
                1,
                input.request_id,
                &duplicate
            )
            .is_err()
    );
    assert_eq!(
        std::fs::read(temp.path().join("store").join("manifest.json")).unwrap(),
        original
    );
    assert_eq!(
        std::fs::read_dir(temp.path().join("store").join("secrets"))
            .unwrap()
            .count(),
        count
    );
    // A staged response cannot overwrite any edit committed after preparation.
    let staged = refresh(&mut store, &input, &body("changed", "second"));
    apply(
        &mut store,
        &ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::RenameProfile {
                profile_id: input.profile_id,
                name: "concurrent".into(),
            },
        },
    );
    assert!(
        matches!(store.apply_config(&staged.request).unwrap(), ConfigOutcome::Rejected { code, .. } if code == "STALE_MANIFEST_REVISION")
    );
}

#[test]
fn active_generation_allows_non_connection_edits_but_requires_confirmation_for_selected_changes() {
    let (_temp, mut store, input) = create();
    let generation = store
        .prepare_core_generation(&[input.profile_id])
        .unwrap()
        .id();
    let starting = CoreState::Starting { generation };
    store
        .transition_core_state(&CoreState::Stopped {}, starting.clone())
        .unwrap();
    let running = CoreState::Running {
        generation,
        process: app_proxy_windows::identity::current().unwrap(),
    };
    store
        .transition_core_state(&starting, running.clone())
        .unwrap();
    let unselected = refresh(&mut store, &input, &body("first", "changed"));
    apply(&mut store, &unselected.request);
    assert_eq!(store.core_state().unwrap(), running);
    assert!(
        store
            .core_generation_is_current(&store.open_core_generation(generation).unwrap())
            .unwrap()
    );
    let selected = refresh(&mut store, &input, &body("updated", "changed"));
    assert!(
        matches!(store.apply_config(&selected.request).unwrap(), ConfigOutcome::Rejected { code, .. } if code == "CORE_RECONFIGURATION_REQUIRED")
    );
    assert_eq!(store.load().unwrap().revision, 3);
    assert_eq!(store.core_state().unwrap(), running);
    // Equal connection parameters with a different node name do not need a restart.
    let equal = refresh(&mut store, &input, &body("first", "first"));
    apply(&mut store, &equal.request);
    let second = nodes(&store)[1].id;
    apply(
        &mut store,
        &ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 4,
            action: ConfigAction::EditSubscriptionProfile {
                profile_id: input.profile_id,
                edit: SubscriptionEdit::Select {
                    expected_source_revision: 3,
                    node_id: second,
                },
            },
        },
    );
    assert_eq!(store.core_state().unwrap(), running);
    assert!(
        store
            .core_generation_is_current(&store.open_core_generation(generation).unwrap())
            .unwrap()
    );
}

#[test]
fn subscription_update_journal_validates_exact_edit_and_recovers_receipts_after_commit() {
    use app_proxy_core::core_control::{CoreAction, CoreOutcome};
    use app_proxy_windows::core_requests::CoreRequestPhase;
    for selection in [false, true] {
        let (temp, mut store, input) = create();
        let generation = store
            .prepare_core_generation(&[input.profile_id])
            .unwrap()
            .id();
        let starting = CoreState::Starting { generation };
        store
            .transition_core_state(&CoreState::Stopped {}, starting.clone())
            .unwrap();
        let previous = CoreState::Running {
            generation,
            process: app_proxy_windows::identity::current().unwrap(),
        };
        store
            .transition_core_state(&starting, previous.clone())
            .unwrap();
        let request = if selection {
            ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 2,
                action: ConfigAction::EditSubscriptionProfile {
                    profile_id: input.profile_id,
                    edit: SubscriptionEdit::Select {
                        expected_source_revision: 1,
                        node_id: nodes(&store)[1].id,
                    },
                },
            }
        } else {
            refresh(&mut store, &input, &body("changed", "second")).request
        };
        let ConfigAction::EditSubscriptionProfile { edit, .. } = &request.action else {
            panic!()
        };
        let action = CoreAction::PrepareSubscription {
            expected_revision: 2,
            profile_id: input.profile_id,
            edit: edit.clone(),
        };
        let epoch = Uuid::new_v4();
        store
            .begin_core_request(request.request_id, epoch, &action)
            .unwrap();
        let mut plan = store.prepare_core_update(&request).unwrap();
        plan.after.profiles[0].name = "unrelated tampering".into();
        assert!(matches!(
            store.publish_core_update(plan),
            Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"))
        ));
        let plan = store.prepare_core_update(&request).unwrap();
        let candidate = plan.candidate;
        let impact = store.publish_core_update(plan).unwrap();
        assert_eq!(impact.affected_profiles, vec![input.profile_id]);
        assert_eq!(store.load().unwrap().revision, 2);
        assert_eq!(store.core_state().unwrap(), previous);
        store
            .resolve_core_update_request(request.request_id, None)
            .unwrap();
        assert!(matches!(
            store.core_request_status(request.request_id).unwrap(),
            Some(CoreRequestPhase::Complete {
                outcome: CoreOutcome::Prepared { .. },
                ..
            })
        ));
        let execution = Uuid::new_v4();
        store
            .begin_core_request(
                execution,
                epoch,
                &CoreAction::ApplyUpdate {
                    plan_id: request.request_id,
                },
            )
            .unwrap();
        store
            .start_core_update(request.request_id, execution)
            .unwrap();
        let down = CoreState::Down { generation };
        store
            .transition_core_update(request.request_id, &previous, down.clone())
            .unwrap();
        let starting = CoreState::Starting {
            generation: candidate,
        };
        store
            .transition_core_update(request.request_id, &down, starting.clone())
            .unwrap();
        store
            .transition_core_update(
                request.request_id,
                &starting,
                CoreState::Running {
                    generation: candidate,
                    process: app_proxy_windows::identity::current().unwrap(),
                },
            )
            .unwrap();
        store.commit_core_update(request.request_id).unwrap();
        // These are journal-only simulated identities: no process APIs are used.
        drop(store);
        let mut store = Store::open(&temp.path().join("store")).unwrap();
        store
            .resolve_core_update_request(request.request_id, None)
            .unwrap();
        assert!(matches!(
            store.core_request_status(execution).unwrap(),
            Some(CoreRequestPhase::Complete {
                outcome: CoreOutcome::Reconfigured { revision: 3, .. },
                ..
            })
        ));
        assert_eq!(store.load().unwrap().revision, 3);
        let ProxySource::Subscription { revision, .. } = store.load().unwrap().profiles[0].source
        else {
            panic!()
        };
        assert_eq!(revision, if selection { 1 } else { 2 });
    }
}
