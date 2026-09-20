use app_proxy_core::{
    model::*,
    registry::{self, ConfigAction, ConfigRequest, ManualProxyInput},
    singbox,
    subscription::{self, saved::SavedNode},
};
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn fixture() -> (Manifest, HashMap<Uuid, String>) {
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let parsed = subscription::parse(include_str!("fixtures/subscription.yaml")).unwrap();
    let mut secrets = HashMap::new();
    let nodes: Vec<_> = parsed
        .nodes
        .iter()
        .map(|node| {
            let (saved, secret) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node).unwrap();
            secrets.insert(saved.secret_id, secret);
            saved
        })
        .collect();
    let url_secret_id = Uuid::new_v4();
    secrets.insert(
        url_secret_id,
        "https://subscription.invalid/?token=source-secret".into(),
    );
    manifest.profiles[0].selected_node_id = nodes[0].id;
    manifest.profiles[0].source = ProxySource::Subscription {
        url_secret_id,
        revision: 1,
        nodes,
        auto_test_node_ids: vec![],
    };
    (manifest, secrets)
}

#[test]
fn automatic_group_compiles_only_selected_members_and_rejects_corrupt_selections() {
    let (mut manifest, secrets) = fixture();
    let ProxySource::Subscription {
        nodes,
        auto_test_node_ids,
        ..
    } = &mut manifest.profiles[0].source
    else {
        panic!()
    };
    let chosen = vec![nodes[0].id, nodes[2].id];
    *auto_test_node_ids = chosen.clone();
    let config = singbox::compile(&manifest, &[manifest.profiles[0].id], |id| {
        Ok(secrets[&id].clone())
    })
    .unwrap();
    let config: Value = serde_json::from_slice(config.bytes()).unwrap();
    let outbounds = config["outbounds"].as_array().unwrap();
    assert_eq!(outbounds.len(), 3);
    assert_eq!(outbounds[2]["type"], "urltest");
    assert_eq!(
        outbounds[2]["outbounds"],
        json!([outbounds[0]["tag"], outbounds[1]["tag"]])
    );
    assert_eq!(config["route"]["rules"][0]["outbound"], outbounds[2]["tag"]);
    assert_eq!(outbounds[2]["url"], manifest.settings.test_url);
    assert_eq!(outbounds[2]["interval"], "3m");
    assert!(!outbounds.iter().any(|o| o["type"] == "direct"));
    assert_eq!(
        serde_json::from_str::<Manifest>(&serde_json::to_string(&manifest).unwrap())
            .unwrap()
            .profiles[0]
            .selected_node_ids(),
        chosen
    );
    for bad in [
        vec![chosen[0]],
        vec![chosen[0], chosen[0]],
        vec![chosen[0], Uuid::new_v4()],
        vec![chosen[1], chosen[0]],
    ] {
        let ProxySource::Subscription {
            auto_test_node_ids, ..
        } = &mut manifest.profiles[0].source
        else {
            panic!()
        };
        *auto_test_node_ids = bad;
        assert!(manifest.validate().is_err());
    }
}

#[test]
fn six_protocols_round_trip_through_secret_documents_without_manifest_credentials() {
    let (manifest, secrets) = fixture();
    manifest.validate().unwrap();
    assert_eq!(manifest.secret_ids(), secrets.keys().copied().collect());
    let encoded = serde_json::to_string(&manifest).unwrap();
    for secret in [
        "fixture%2F,secret",
        "12345678-1234-1234-1234-123456789abc",
        "source-secret",
    ] {
        assert!(!encoded.contains(secret));
    }
    let manifest: Manifest = serde_json::from_str(&encoded).unwrap();
    let ProxySource::Subscription { nodes, .. } = &manifest.profiles[0].source else {
        panic!()
    };
    let originals = subscription::parse(include_str!("fixtures/subscription.yaml")).unwrap();
    for (saved, original) in nodes.iter().zip(&originals.nodes) {
        assert!(saved.resolve(&secrets[&saved.secret_id]).unwrap() == *original);
    }
    let node = subscription::parse(
        "trojan://password-only-in-secret@edge.invalid:443?type=ws&path=%2Fsecret-path-token#label",
    )
    .unwrap()
    .nodes
    .remove(0);
    let (saved, document) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), &node).unwrap();
    assert!(document.contains("secret-path-token"));
    assert!(
        !serde_json::to_string(&saved)
            .unwrap()
            .contains("secret-path-token")
    );
    assert!(saved.resolve(&document).unwrap() == node);
}

#[test]
fn typed_secret_reader_rejects_unknown_fields_invalid_credentials_and_metadata_mismatch() {
    let node = subscription::parse("trojan://synthetic@edge.invalid:443?type=quic#label")
        .unwrap()
        .nodes
        .remove(0);
    let (saved, document) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), &node).unwrap();
    assert!(saved.resolve(&document).is_ok());
    let original: Value = serde_json::from_str(&document).unwrap();
    for (pointer, value) in [
        ("/version", json!(2)),
        ("/node/name", json!("other")),
        ("/node/server", json!("other.invalid")),
        ("/node/port", json!(1)),
        ("/node/protocol/password", json!("")),
        ("/node/tls", Value::Null),
    ] {
        let mut corrupt = original.clone();
        *corrupt.pointer_mut(pointer).unwrap() = value;
        assert!(saved.resolve(&corrupt.to_string()).is_err(), "{pointer}");
    }
    for pointer in [
        "",
        "/node",
        "/node/protocol",
        "/node/tls",
        "/node/transport",
    ] {
        let mut corrupt = original.clone();
        corrupt
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("unknown".into(), json!("token"));
        assert!(saved.resolve(&corrupt.to_string()).is_err(), "{pointer}");
    }
    assert!(
        saved
            .resolve(&document.replacen("\"version\":1", "\"version\":1,\"version\":1", 1))
            .is_err()
    );
    assert!(saved.resolve(&"x".repeat(65537)).is_err());
    assert!(SavedNode::capture(Uuid::nil(), Uuid::new_v4(), &node).is_err());
}

#[test]
fn shared_compiler_resolves_only_selected_node_and_preserves_explicit_routes() {
    let (mut manifest, secrets) = fixture();
    let profile_id = manifest.profiles[0].id;
    let ProxySource::Subscription { nodes, .. } = &manifest.profiles[0].source else {
        panic!()
    };
    let saved = nodes.clone();
    let mut manual: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    manual.profiles[0].id = Uuid::new_v4();
    manual.profiles[0].endpoint.port += 1;
    let manual_id = manual.profiles[0].id;
    manifest.profiles.push(manual.profiles.remove(0));
    for selected in &saved {
        manifest.profiles[0].selected_node_id = selected.id;
        let mut resolved = Vec::new();
        let config = singbox::compile(&manifest, &[profile_id, manual_id], |id| {
            resolved.push(id);
            Ok(secrets[&id].clone())
        })
        .unwrap();
        assert_eq!(resolved, vec![selected.secret_id]);
        let config: Value = serde_json::from_slice(config.bytes()).unwrap();
        let out = config["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["tag"] == format!("out-{profile_id}"))
            .unwrap();
        assert_eq!(
            *out,
            selected
                .resolve(&secrets[&selected.secret_id])
                .unwrap()
                .outbound(&format!("out-{profile_id}"))
                .unwrap()
        );
        assert_eq!(config["inbounds"].as_array().unwrap().len(), 2);
        assert_eq!(
            config["route"]["rules"].as_array().unwrap().last().unwrap()["action"],
            "reject"
        );
    }
    assert!(matches!(
        singbox::compile(&manifest, &[profile_id], |_| Err(ValidationError(
            "MISSING_SECRET"
        ))),
        Err(ValidationError("MISSING_SECRET"))
    ));
    assert!(matches!(
        singbox::compile(&manifest, &[profile_id], |_| Ok("{}".into())),
        Err(ValidationError("INVALID_SUBSCRIPTION_SECRET"))
    ));
}

#[test]
fn subscription_manifest_rejects_invalid_source_selection_duplicates_and_manual_replacement() {
    for mutation in 0..6 {
        let (mut manifest, _) = fixture();
        let profile = &mut manifest.profiles[0];
        let ProxySource::Subscription {
            url_secret_id,
            revision,
            nodes,
            ..
        } = &mut profile.source
        else {
            panic!()
        };
        match mutation {
            0 => *url_secret_id = Uuid::nil(),
            1 => *revision = 0,
            2 => nodes[1].name = nodes[0].name.clone(),
            3 => nodes[1].id = nodes[0].id,
            4 => nodes[0].secret_id = Uuid::nil(),
            _ => profile.selected_node_id = Uuid::new_v4(),
        }
        assert!(manifest.validate().is_err());
    }
    let (manifest, _) = fixture();
    let request = ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: manifest.revision,
        action: ConfigAction::UpdateManualProfile {
            profile_id: manifest.profiles[0].id,
            node: ManualProxyInput {
                protocol: ManualProtocol::Http,
                host: "edge.invalid".into(),
                port: 443,
                credentials: None,
            },
        },
    };
    assert!(matches!(
        registry::apply(manifest, &request),
        Err(ValidationError("MANUAL_PROFILE_REQUIRED"))
    ));
}

#[test]
fn source_url_rejects_ambiguous_credentials_and_unsafe_schemes_without_echoing_input() {
    assert!(subscription::source_url("https://source.invalid/sub?token=secret").is_ok());
    for value in [
        "file:///secret",
        "https://@source.invalid",
        "https://user:password@source.invalid",
        "https://source.invalid:0",
        "https://source.invalid/#secret",
        "https://source.invalid/\nsecret",
    ] {
        let error = subscription::source_url(value).unwrap_err();
        assert_eq!(error.to_string(), "SUBSCRIPTION_URL_INVALID");
    }
}
