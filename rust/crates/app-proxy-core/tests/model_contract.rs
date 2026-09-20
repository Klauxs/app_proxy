use app_proxy_core::model::*;
use uuid::Uuid;

fn example() -> Manifest {
    serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap()
}
fn invalid(manifest: &Manifest, code: &str) {
    assert_eq!(manifest.validate().unwrap_err().0, code);
}

#[test]
fn current_example_roundtrips_without_changing_identity() {
    let manifest = example();
    manifest.validate().unwrap();
    let encoded = serde_json::to_vec(&manifest).unwrap();
    let decoded: Manifest = serde_json::from_slice(&encoded).unwrap();
    decoded.validate().unwrap();
    assert_eq!(manifest.store_id, decoded.store_id);
    assert_eq!(manifest.instances[1].id, decoded.instances[1].id);
}

#[test]
fn empty_store_is_valid_but_unknown_format_and_schema_are_not() {
    let mut manifest = Manifest::empty("S-1-5-21-1000".into());
    manifest.validate().unwrap();
    manifest.schema_version = 2;
    invalid(&manifest, "UNSUPPORTED_FORMAT");
    manifest.schema_version = 1;
    manifest.format = "legacy".into();
    invalid(&manifest, "UNSUPPORTED_FORMAT");
}

#[test]
fn only_a_clone_does_not_require_or_create_an_original() {
    let mut manifest = example();
    manifest.instances.remove(0);
    manifest.validate().unwrap();
    assert_eq!(manifest.instances.len(), 1);
}

#[test]
fn dangling_reference_and_duplicate_identity_are_rejected() {
    let mut manifest = example();
    manifest.instances[0].application_id = Uuid::new_v4();
    invalid(&manifest, "APPLICATION_NOT_FOUND");
    let mut manifest = example();
    manifest.instances[1].id = manifest.instances[0].id;
    invalid(&manifest, "INVALID_OR_DUPLICATE_ENTITY");
    let mut manifest = example();
    manifest.instances[0].network = NetworkBinding::Profile {
        profile_id: Uuid::new_v4(),
    };
    invalid(&manifest, "PROFILE_NOT_FOUND");
}

#[test]
fn duplicate_original_and_managed_network_conflicts_are_rejected() {
    let mut manifest = example();
    manifest.instances[1].data = InstanceData::Original {};
    invalid(&manifest, "DUPLICATE_ORIGINAL");
    let mut manifest = example();
    manifest.instances[0].network = NetworkBinding::Direct {};
    invalid(&manifest, "GUARD_REQUIRES_SUPPORTED_PROXY");
    let mut manifest = example();
    manifest.instances[0].args = vec!["--proxy-server=http://other".into()];
    invalid(&manifest, "MANAGED_OR_INVALID_ARGUMENT");
    let mut manifest = example();
    manifest.instances[0].env.unset.push("http_proxy".into());
    invalid(&manifest, "MANAGED_ENV_CONFLICT");
}

#[test]
fn empty_set_and_unset_differ_but_case_duplicate_env_is_invalid() {
    let mut manifest = example();
    manifest.instances[0].env.set.insert(
        "TEST".into(),
        EnvValue::Literal {
            value: String::new(),
        },
    );
    manifest.validate().unwrap();
    manifest.instances[0].env.unset.push("test".into());
    invalid(&manifest, "INVALID_ENV_PATCH");
    assert!(serde_json::from_str::<SavedEnvironment>(r#"{"set":{"A":{"kind":"literal","value":"first"},"A":{"kind":"literal","value":"second"}},"unset":[]}"#).is_err());
}

#[test]
fn path_traversal_and_other_instance_directory_are_rejected() {
    for path in [
        "../escape",
        "C:\\escape",
        "instances/../escape",
        "instances/a:stream",
        "instances/a.",
        "instances/a ",
    ] {
        assert!(safe_relative(std::path::Path::new(path)).is_err(), "{path}");
    }
    let mut manifest = example();
    manifest.instances[1].data = InstanceData::Isolated {
        location: StorageLocation::Store {
            relative_path: "instances/someone-else".into(),
        },
    };
    invalid(&manifest, "INSTANCE_STORAGE_NOT_ASSIGNED");
}

#[test]
fn profile_requires_loopback_and_selected_node() {
    let mut manifest = example();
    manifest.profiles[0].endpoint.host = "0.0.0.0".parse().unwrap();
    invalid(&manifest, "INVALID_OR_DUPLICATE_ENDPOINT");
    let mut manifest = example();
    manifest.profiles[0].selected_node_id = Uuid::new_v4();
    invalid(&manifest, "SELECTED_NODE_NOT_FOUND");
}

#[test]
fn credentials_and_unknown_fields_are_not_silently_accepted() {
    let mut manifest = example();
    manifest.settings.test_url = "https://user:secret@example.com".into();
    invalid(&manifest, "INVALID_HEALTH_URL");
    assert!(
        serde_json::from_str::<NetworkBinding>(r#"{"kind":"direct","profile_id":"ignored"}"#)
            .is_err()
    );
    assert!(serde_json::from_str::<ProxyKind>(r#""existing_singbox""#).is_err());
    assert!(
        serde_json::from_str::<InstanceData>(r#"{"kind":"original","location":"ignored"}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<WorkingDirectory>(r#"{"kind":"application","path":"ignored"}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<app_proxy_core::FileIdentity>(
            r#"{"volume_serial":1,"file_index":2,"extra":3}"#
        )
        .is_err()
    );
}

#[test]
fn chromium_windows_switch_aliases_cannot_override_managed_fields() {
    for arg in [
        "-no-proxy-server",
        "/proxy-server=http://other",
        "--USER-DATA-DIR=C:\\other",
        " --proxy-server=http://other ",
        " -- ",
        "--single-argument",
        "/SINGLE-ARGUMENT",
    ] {
        assert!(
            validate_instance_input(&[arg.into()], &SavedEnvironment::default()).is_err(),
            "{arg}"
        );
    }
    validate_instance_input(
        &["--lang=zh-CN".into(), "https://example.com".into()],
        &SavedEnvironment::default(),
    )
    .unwrap();
}

#[test]
fn retired_ifeo_slot_preserves_empty_manifest_and_rejects_old_registrations() {
    let original: serde_json::Value =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let manifest: Manifest = serde_json::from_value(original.clone()).unwrap();
    assert_eq!(
        serde_json::to_string(&manifest.integrations).unwrap(),
        r#"{"shortcuts":[],"guard_login_task":null,"ifeo":[]}"#
    );
    // Existing transaction digest input remains unchanged, including the empty slot.
    assert_eq!(serde_json::to_value(manifest).unwrap(), original);
    let mut legacy = original;
    legacy["integrations"]["ifeo"] = serde_json::json!([{"id":"legacy-registration"}]);
    let error = serde_json::from_value::<Manifest>(legacy).err().unwrap();
    assert!(error.to_string().contains("LEGACY_IFEO_CLEANUP_REQUIRED"));
}
