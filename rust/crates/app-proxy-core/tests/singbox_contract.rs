use app_proxy_core::{model::*, singbox};
use serde_json::Value;
use uuid::Uuid;

fn fixture() -> Manifest {
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let mut second = manifest.profiles[0].clone();
    second.id = Uuid::new_v4();
    second.name = "second".into();
    second.endpoint.host = "::1".parse().unwrap();
    second.endpoint.port += 1;
    let ProxySource::Manual { nodes } = &mut second.source else {
        panic!("manual fixture required")
    };
    nodes[0].protocol = ManualProtocol::Socks5;
    nodes[0].host = "2001:db8::2".into();
    manifest.profiles.push(second);
    manifest
}

#[test]
fn two_profiles_have_explicit_separate_routes_and_no_direct_or_implicit_fallback() {
    let manifest = fixture();
    let ids: Vec<_> = manifest.profiles.iter().map(|p| p.id).collect();
    let output = singbox::compile(&manifest, &ids, |_| panic!("no credentials")).unwrap();
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["inbounds"].as_array().unwrap().len(), 2);
    assert_eq!(value["outbounds"].as_array().unwrap().len(), 2);
    for profile in &manifest.profiles {
        let incoming = format!("in-{}", profile.id);
        let outgoing = format!("out-{}", profile.id);
        let inbound = value["inbounds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["tag"] == incoming)
            .unwrap();
        assert_eq!(inbound["listen"], profile.endpoint.host.to_string());
        assert_eq!(inbound["listen_port"], profile.endpoint.port);
        assert_eq!(inbound["set_system_proxy"], false);
        assert!(
            value["route"]["rules"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["inbound"][0] == incoming
                    && r["outbound"] == outgoing
                    && r["action"] == "route")
        );
    }
    assert_eq!(
        value["route"]["rules"].as_array().unwrap().last().unwrap()["action"],
        "reject"
    );
    assert!(
        value["outbounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|o| o["type"] != "direct" && o["type"] != "selector")
    );
}

#[test]
fn stable_profile_identity_ignores_names_revisions_order_and_duplicate_users() {
    let mut manifest = fixture();
    let a = manifest.profiles[0].id;
    let b = manifest.profiles[1].id;
    let first = singbox::compile(&manifest, &[a, b, a], |_| unreachable!()).unwrap();
    manifest.profiles.reverse();
    manifest.profiles[0].name = "renamed".into();
    manifest.profiles[0].revision += 1;
    manifest.revision += 1;
    let second = singbox::compile(&manifest, &[b, a], |_| unreachable!()).unwrap();
    assert_eq!(first.bytes(), second.bytes());
    assert_eq!(second.profiles().len(), 2);
    let one = singbox::compile(&manifest, &[a], |_| unreachable!()).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(one.bytes()).unwrap()["inbounds"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn resolves_only_selected_credentials_and_preserves_literal_secret_characters() {
    let mut manifest = fixture();
    let id = manifest.profiles[0].id;
    let secret_id = Uuid::new_v4();
    let ProxySource::Manual { nodes } = &mut manifest.profiles[0].source else {
        panic!("manual fixture required")
    };
    nodes[0].credentials = Some(Credentials {
        username: "user@example".into(),
        password_secret_id: secret_id,
    });
    let mut unused = nodes[0].clone();
    unused.id = Uuid::new_v4();
    unused.name = "unselected".into();
    unused.credentials.as_mut().unwrap().password_secret_id = Uuid::new_v4();
    nodes.push(unused);
    let password = "literal%25\"\\\n密码";
    let output = singbox::compile(&manifest, &[id], |actual| {
        assert_eq!(actual, secret_id);
        Ok(password.into())
    })
    .unwrap();
    let value: Value = serde_json::from_slice(output.bytes()).unwrap();
    assert_eq!(value["outbounds"][0]["password"], password);
    assert_eq!(
        singbox::compile(&manifest, &[id], |_| Err(ValidationError(
            "SECRET_UNAVAILABLE"
        )))
        .err()
        .unwrap()
        .0,
        "SECRET_UNAVAILABLE"
    );
    assert_eq!(
        singbox::compile(&manifest, &[id], |_| Ok("bad\0secret".into()))
            .err()
            .unwrap()
            .0,
        "INVALID_PROXY_CREDENTIALS"
    );
}

#[test]
fn rejects_missing_profiles_invalid_listeners_and_unrepresentable_socks_auth() {
    let mut manifest = fixture();
    assert_eq!(
        singbox::compile(&manifest, &[], |_| unreachable!())
            .err()
            .unwrap()
            .0,
        "NO_ACTIVE_PROFILES"
    );
    assert_eq!(
        singbox::compile(&manifest, &[Uuid::new_v4()], |_| unreachable!())
            .err()
            .unwrap()
            .0,
        "PROFILE_NOT_FOUND"
    );
    let id = manifest.profiles[1].id;
    let ProxySource::Manual { nodes } = &mut manifest.profiles[1].source else {
        panic!("manual fixture required")
    };
    nodes[0].credentials = Some(Credentials {
        username: "socks-user".into(),
        password_secret_id: Uuid::new_v4(),
    });
    assert_eq!(
        singbox::compile(&manifest, &[id], |_| Ok("x".repeat(256)))
            .err()
            .unwrap()
            .0,
        "INVALID_PROXY_CREDENTIALS"
    );
    manifest.profiles[1].endpoint.host = "0.0.0.0".parse().unwrap();
    assert!(singbox::compile(&manifest, &[id], |_| Ok("password".into())).is_err());
}
