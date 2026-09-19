use app_proxy_core::{model::*, registry::*};
use uuid::Uuid;

fn example() -> Manifest {
    serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap()
}
fn request(action: ConfigAction) -> ConfigRequest {
    ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        action,
    }
}
fn create(manifest: &Manifest, data: NewData, network: NetworkBinding) -> ConfigRequest {
    request(ConfigAction::CreateInstance {
        instance: NewInstance {
            id: Uuid::new_v4(),
            application_id: manifest.applications[0].id,
            name: "new instance".into(),
            data,
            network,
            guard: None,
            args: vec![],
            env: SavedEnvironment::default(),
            cwd: WorkingDirectory::Application {},
        },
    })
}

#[test]
fn omitted_data_defaults_to_original_and_network_remains_required() {
    let id = Uuid::new_v4();
    let app = Uuid::new_v4();
    let value = serde_json::json!({"id":id,"application_id":app,"name":"original","network":{"kind":"direct"}});
    let instance: NewInstance = serde_json::from_value(value.clone()).unwrap();
    assert!(matches!(instance.data, NewData::Original {}));
    let mut value = value;
    value.as_object_mut().unwrap().remove("network");
    assert!(serde_json::from_value::<NewInstance>(value).is_err());
}

#[test]
fn create_only_isolated_does_not_register_original_or_ifeo() {
    let mut manifest = example();
    manifest.instances.clear();
    let profile = manifest.profiles[0].id;
    let change = create(
        &manifest,
        NewData::Isolated {
            storage: NewStorage::PackageLocalState,
        },
        NetworkBinding::Profile {
            profile_id: profile,
        },
    );
    let (updated, receipt) = apply(manifest, &change).unwrap();
    assert_eq!(updated.instances.len(), 1);
    assert_eq!(updated.instances[0].id, receipt.entity_id);
    assert_eq!(receipt.revision, 2);
    assert_eq!(updated.revision, 1);
    assert!(updated.instances[0].guard.desired == Desired::Enabled);
    assert!(updated.integrations.ifeo.is_empty());
    let InstanceData::Isolated {
        location:
            StorageLocation::PackageLocalState {
                namespace,
                relative_path,
                ..
            },
    } = &updated.instances[0].data
    else {
        panic!()
    };
    assert_eq!(namespace, &updated.store_id.to_string());
    assert_eq!(
        relative_path,
        &std::path::PathBuf::from("instances").join(receipt.entity_id.to_string())
    );
}

#[test]
fn original_uniqueness_and_explicit_guard_rules_are_enforced() {
    let manifest = example();
    let duplicate = create(&manifest, NewData::Original {}, NetworkBinding::Direct {});
    assert_eq!(
        apply(manifest, &duplicate).err().unwrap().0,
        "DUPLICATE_ORIGINAL"
    );
    let mut manifest = example();
    manifest.instances.clear();
    let mut change = create(&manifest, NewData::Original {}, NetworkBinding::Direct {});
    if let ConfigAction::CreateInstance { instance } = &mut change.action {
        instance.guard = Some(Desired::Enabled);
    }
    assert_eq!(
        apply(manifest, &change).err().unwrap().0,
        "GUARD_REQUIRES_SUPPORTED_PROXY"
    );
}

#[test]
fn clone_copies_configuration_and_secret_references_but_allocates_new_data() {
    let mut manifest = example();
    let secret = Uuid::new_v4();
    manifest.instances[0]
        .env
        .set
        .insert("TOKEN".into(), EnvValue::SecretRef { id: secret });
    manifest.instances[0]
        .args
        .push("--file=${user_data}/settings.json".into());
    let source_id = manifest.instances[0].id;
    let new_id = Uuid::new_v4();
    let change = request(ConfigAction::CloneInstance {
        source_id,
        instance_id: new_id,
        name: "blank clone".into(),
        storage: NewStorage::Store,
        network: None,
        guard: None,
    });
    let (updated, receipt) = apply(manifest, &change).unwrap();
    let copied = &updated.instances[2];
    assert_eq!(copied.id, new_id);
    assert_eq!(receipt.entity_id, new_id);
    assert_eq!(copied.revision, 1);
    assert_eq!(copied.args, updated.instances[0].args);
    assert!(matches!(copied.env.set["TOKEN"], EnvValue::SecretRef { id } if id == secret));
    assert!(matches!(
        updated.instances[0].data,
        InstanceData::Original {}
    ));
    let InstanceData::Isolated {
        location: StorageLocation::Store { relative_path },
    } = &copied.data
    else {
        panic!()
    };
    assert_eq!(
        relative_path,
        &std::path::PathBuf::from("instances").join(new_id.to_string())
    );
}

#[test]
fn rename_preserves_identity_and_bind_preserves_explicit_disabled_guard() {
    let manifest = example();
    let id = manifest.instances[1].id;
    let profile = manifest.profiles[0].id;
    let (updated, _) = apply(
        manifest,
        &request(ConfigAction::RenameInstance {
            instance_id: id,
            name: "renamed".into(),
        }),
    )
    .unwrap();
    assert_eq!(updated.instances[1].id, id);
    assert_eq!(updated.instances[1].revision, 2);
    let (updated, _) = apply(
        updated,
        &request(ConfigAction::BindInstance {
            instance_id: id,
            network: NetworkBinding::Profile {
                profile_id: profile,
            },
            guard: None,
        }),
    )
    .unwrap();
    assert!(updated.instances[1].guard.desired == Desired::Disabled);
    assert_eq!(updated.instances[1].revision, 3);
}

#[test]
fn remove_requires_integration_cleanup_and_keeps_application() {
    let mut manifest = example();
    let id = manifest.instances[1].id;
    manifest.integrations.shortcuts.push(Shortcut {
        instance_id: id,
        path: r"C:\Desktop\owned.lnk".into(),
        target: r"C:\Apps\host.exe".into(),
        args: vec![],
    });
    let change = request(ConfigAction::RemoveInstance { instance_id: id });
    assert_eq!(
        apply(manifest, &change).err().unwrap().0,
        "INTEGRATION_CLEANUP_REQUIRED"
    );
    let (updated, _) = apply(example(), &change).unwrap();
    assert_eq!(updated.instances.len(), 1);
    assert_eq!(updated.applications.len(), 1);
}

#[test]
fn stale_revision_nil_request_and_entity_revision_overflow_are_rejected() {
    let mut manifest = example();
    let id = manifest.instances[0].id;
    let mut change = request(ConfigAction::RenameInstance {
        instance_id: id,
        name: "next".into(),
    });
    change.expected_revision = 2;
    assert_eq!(
        apply(example(), &change).err().unwrap().0,
        "STALE_MANIFEST_REVISION"
    );
    change.expected_revision = 1;
    change.request_id = Uuid::nil();
    assert_eq!(
        apply(example(), &change).err().unwrap().0,
        "INVALID_REQUEST_ID"
    );
    change.request_id = Uuid::new_v4();
    manifest.instances[0].revision = u64::MAX;
    assert_eq!(
        apply(manifest, &change).err().unwrap().0,
        "REVISION_EXHAUSTED"
    );
}
