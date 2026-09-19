#![cfg(windows)]
use app_proxy_core::model::*;
use app_proxy_windows::{core_state::CoreState, identity, store::Store};
use std::fs;
use uuid::Uuid;

fn configured() -> (tempfile::TempDir, Store, Uuid) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::create(&temp.path().join("owned")).unwrap();
    let mut manifest = store.load().unwrap();
    let node = ManualNode {
        id: Uuid::new_v4(),
        name: "fixture".into(),
        protocol: ManualProtocol::Http,
        host: "proxy.invalid".into(),
        port: 8080,
        credentials: None,
    };
    let id = Uuid::new_v4();
    manifest.profiles.push(ProxyProfile {
        id,
        name: "fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 48091,
        },
        selected_node_id: node.id,
        source: ProxySource::Manual { nodes: vec![node] },
    });
    store.commit(manifest.revision, manifest).unwrap();
    (temp, store, id)
}

#[test]
fn candidate_is_immutable_preserves_manifest_and_checks_current_configuration() {
    let (_temp, mut store, id) = configured();
    let before = store.load().unwrap().revision;
    let candidate = store.prepare_core_generation(&[id]).unwrap();
    assert_eq!(store.load().unwrap().revision, before);
    assert_eq!(store.core_state().unwrap(), CoreState::Stopped {});
    assert!(store.core_generation_is_current(&candidate).unwrap());
    assert!(fs::write(candidate.config_path(), "modified").is_err());
    assert!(
        fs::rename(
            candidate.config_path().parent().unwrap(),
            candidate
                .config_path()
                .parent()
                .unwrap()
                .with_extension("moved")
        )
        .is_err()
    );
    let mut manifest = store.load().unwrap();
    manifest.profiles[0].name = "rename only".into();
    manifest.profiles[0].revision += 1;
    store.commit(manifest.revision, manifest).unwrap();
    assert!(store.core_generation_is_current(&candidate).unwrap());
    let mut manifest = store.load().unwrap();
    manifest.profiles[0].endpoint.port += 1;
    store.commit(manifest.revision, manifest).unwrap();
    assert!(!store.core_generation_is_current(&candidate).unwrap());
    let path = candidate.config_path().to_owned();
    let generation = candidate.id();
    drop(candidate);
    fs::write(&path, "modified").unwrap();
    assert!(matches!(
        store.open_core_generation(generation),
        Err(app_proxy_windows::Error::Invalid(
            "CORE_CONFIG_DIGEST_MISMATCH"
        ))
    ));
    assert_eq!(fs::read(path).unwrap(), b"modified");
}

#[test]
fn journal_survives_reopen_rejects_stale_writes_and_does_not_replay_starting() {
    let (temp, mut store, id) = configured();
    let candidate = store.prepare_core_generation(&[id]).unwrap();
    let starting = CoreState::Starting {
        generation: candidate.id(),
    };
    store
        .transition_core_state(&CoreState::Stopped {}, starting.clone())
        .unwrap();
    assert!(
        store
            .transition_core_state(&CoreState::Stopped {}, starting.clone())
            .is_err()
    );
    drop(store);
    let mut store = Store::open(&temp.path().join("owned")).unwrap();
    assert_eq!(store.core_state().unwrap(), starting);
    let running = CoreState::Running {
        generation: candidate.id(),
        process: identity::current().unwrap(),
    };
    store
        .transition_core_state(&starting, running.clone())
        .unwrap();
    assert!(
        store
            .transition_core_state(
                &running,
                CoreState::Starting {
                    generation: candidate.id()
                }
            )
            .is_err()
    );
    store
        .transition_core_state(&running, CoreState::Stopped {})
        .unwrap();
    assert!(candidate.config_path().exists());
}

#[test]
fn corrupt_or_foreign_records_are_preserved_and_unknown_fields_fail_closed() {
    let (temp, mut store, id) = configured();
    let candidate = store.prepare_core_generation(&[id]).unwrap();
    store
        .transition_core_state(
            &CoreState::Stopped {},
            CoreState::Starting {
                generation: candidate.id(),
            },
        )
        .unwrap();
    let path = temp.path().join("owned/state/core/runtime.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    record["state"] = serde_json::json!({"phase":"stopped","process":{"pid":1}});
    let bad = serde_json::to_vec(&record).unwrap();
    fs::write(&path, &bad).unwrap();
    assert!(store.core_state().is_err());
    assert_eq!(fs::read(&path).unwrap(), bad);
    record["state"] = serde_json::json!({"phase":"stopped"});
    record["store_id"] = serde_json::json!(Uuid::new_v4());
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    assert!(matches!(
        store.core_state(),
        Err(app_proxy_windows::Error::Invalid(
            "CORE_STATE_OWNER_MISMATCH"
        ))
    ));
    assert!(serde_json::from_value::<CoreState>(serde_json::json!({"phase":"running","generation":candidate.id(),"process":{"unexpected":true}})).is_err());
}

#[test]
fn swapped_profile_ports_cannot_change_health_evidence_identity() {
    let (_temp, mut store, id) = configured();
    let mut manifest = store.load().unwrap();
    let mut second = manifest.profiles[0].clone();
    second.id = Uuid::new_v4();
    second.endpoint.port += 1;
    let second_id = second.id;
    manifest.profiles.push(second);
    store.commit(manifest.revision, manifest).unwrap();
    let generation = store.prepare_core_generation(&[id, second_id]).unwrap();
    let path = generation
        .config_path()
        .parent()
        .unwrap()
        .join("generation.json");
    let id = generation.id();
    drop(generation);
    let mut header: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let a = header["profiles"][0]["endpoint"].clone();
    let b = header["profiles"][1]["endpoint"].clone();
    header["profiles"][0]["endpoint"] = b;
    header["profiles"][1]["endpoint"] = a;
    fs::write(path, serde_json::to_vec(&header).unwrap()).unwrap();
    assert!(matches!(
        store.open_core_generation(id),
        Err(app_proxy_windows::Error::Invalid(
            "CORE_PROFILE_MAPPING_MISMATCH"
        ))
    ));
}
