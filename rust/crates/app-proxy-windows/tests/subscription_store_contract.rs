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
    fs::write(&path, br#"{"value":"{broken-secret"}"#).unwrap();
    assert!(store.load().is_err());
    drop(store);
    assert!(Store::open(&root).is_err());
    assert_eq!(fs::read(root.join("manifest.json")).unwrap(), before);
    assert_eq!(fs::read(path).unwrap(), br#"{"value":"{broken-secret"}"#);
}
