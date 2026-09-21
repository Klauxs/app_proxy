#![cfg(windows)]
use app_proxy_core::{
    model::*,
    registry::{ConfigAction, ConfigRequest},
    singbox,
    subscription::{self, saved::SavedNode},
};
use app_proxy_windows::{config_transaction::ConfigOutcome, store::Store};
use std::fs;
use uuid::Uuid;

fn stage(store: &Store) -> ProxyProfile {
    let parsed = subscription::parse(include_str!(
        "../../app-proxy-core/tests/fixtures/subscription.yaml"
    ))
    .unwrap();
    let nodes: Vec<_> = parsed
        .nodes
        .iter()
        .map(|node| {
            let (mut saved, document) =
                SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node).unwrap();
            saved.secret_id = store.put_secret(&document).unwrap();
            saved
        })
        .collect();
    ProxyProfile {
        id: Uuid::new_v4(),
        name: "fixture".into(),
        revision: 1,
        kind: ProxyKind::Managed,
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: 18999,
        },
        selected_node_id: nodes[0].id,
        source: ProxySource::Subscription {
            url_secret_id: store
                .put_secret("https://subscription.invalid/?token=source-secret")
                .unwrap(),
            revision: 1,
            nodes,
            auto_test_node_ids: vec![],
        },
    }
}

#[test]
fn durable_subscription_profile_reopens_and_compiles_without_secrets_in_manifest_or_journal() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let profile = stage(&store);
    let profile_id = profile.id;
    let request = ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        action: ConfigAction::AddProfile { profile },
    };
    assert!(matches!(
        store.apply_config(&request).unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    let first = store.load().unwrap();
    let secret_ids = first.secret_ids();
    assert_eq!(secret_ids.len(), 7);
    let config = singbox::compile(&first, &[profile_id], |id| {
        Ok(store.read_secret(id).unwrap())
    })
    .unwrap();
    assert!(
        std::str::from_utf8(config.bytes())
            .unwrap()
            .contains("fixture%2F,secret")
    );
    for folder in ["", "state/requests", "backups"] {
        let folder = root.join(folder);
        assert!(folder.is_dir());
        for file in fs::read_dir(folder).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|s| s == "json") {
                let contents = fs::read_to_string(path).unwrap();
                for forbidden in [
                    "fixture%2F,secret",
                    "source-secret",
                    "12345678-1234-1234-1234-123456789abc",
                ] {
                    assert!(!contents.contains(forbidden));
                }
            }
        }
    }
    drop(store);
    let mut reopened = Store::open(&root).unwrap();
    assert_eq!(reopened.load().unwrap().secret_ids(), secret_ids);
    assert!(matches!(
        reopened.apply_config(&request).unwrap(),
        ConfigOutcome::Applied { .. }
    ));
    assert_eq!(reopened.load().unwrap().revision, 2);
}

#[test]
fn bad_subscription_secrets_reject_commit_and_existing_corruption_is_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    for mutation in 0..4 {
        let mut profile = stage(&store);
        let ProxySource::Subscription {
            url_secret_id,
            nodes,
            ..
        } = &mut profile.source
        else {
            panic!()
        };
        match mutation {
            0 => {
                *url_secret_id = store
                    .put_secret("https://user:secret@subscription.invalid")
                    .unwrap()
            }
            1 => nodes[1].secret_id = Uuid::new_v4(),
            2 => nodes[1].secret_id = nodes[0].secret_id,
            _ => nodes[1].secret_id = store.put_secret("{broken-secret").unwrap(),
        }
        let mut manifest = store.load().unwrap();
        manifest.profiles.push(profile);
        assert!(store.commit(1, manifest).is_err());
        assert!(store.load().unwrap().profiles.is_empty());
    }
    let profile = stage(&store);
    let ProxySource::Subscription { nodes, .. } = &profile.source else {
        panic!()
    };
    let secret_id = nodes[1].secret_id; // Unselected nodes must also be valid.
    let mut manifest = store.load().unwrap();
    manifest.profiles.push(profile);
    store.commit(1, manifest).unwrap();
    let before = fs::read(root.join("manifest.json")).unwrap();
    let path = root.join(format!("secrets/{secret_id}.json"));
    // Live validated snapshots pin even unselected credentials against edits.
    assert!(fs::write(&path, br#"{"value":"{broken-secret"}"#).is_err());
    assert!(fs::remove_file(&path).is_err());
    assert!(store.load().is_ok());
    drop(store);
    fs::write(&path, br#"{"value":"{broken-secret"}"#).unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(fs::read(root.join("manifest.json")).unwrap(), before);
    assert_eq!(fs::read(path).unwrap(), br#"{"value":"{broken-secret"}"#);
}

#[test]
fn same_revision_edit_cannot_reuse_validated_subscription_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let profile = stage(&store);
    let mut manifest = store.load().unwrap();
    manifest.profiles.push(profile);
    store.commit(1, manifest).unwrap();
    store.load().unwrap();
    let path = root.join("manifest.json");
    let original = fs::read(&path).unwrap();
    let mut edited: Manifest = serde_json::from_slice(&original).unwrap();
    let ProxySource::Subscription { nodes, .. } = &mut edited.profiles[0].source else {
        panic!()
    };
    // Still structurally valid, but its credential belongs to a different node.
    nodes[1].secret_id = nodes[0].secret_id;
    fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
    assert!(store.load().is_err());
    fs::write(&path, original).unwrap();
    assert!(store.load().is_ok());
}

#[test]
fn replacing_subscription_releases_old_pins_and_validates_new_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let first = stage(&store);
    let mut manifest = store.load().unwrap();
    manifest.profiles.push(first);
    store.commit(1, manifest).unwrap();
    let old = store.load().unwrap().secret_ids();
    let next = stage(&store);
    let mut manifest = store.load().unwrap();
    manifest.profiles = vec![next];
    store.commit(2, manifest).unwrap();
    let current = store.load().unwrap();
    for id in old {
        assert!(!current.secret_ids().contains(&id));
        fs::remove_file(root.join(format!("secrets/{id}.json"))).unwrap();
    }
    for id in current.secret_ids() {
        assert!(fs::remove_file(root.join(format!("secrets/{id}.json"))).is_err());
    }
    assert!(store.load().is_ok());
}
