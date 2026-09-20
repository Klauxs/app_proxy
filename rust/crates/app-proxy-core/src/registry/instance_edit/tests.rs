use super::*;
use crate::registry::{self, ConfigAction, ConfigRequest};

fn fixture() -> (Manifest, Uuid) {
    let mut manifest = Manifest::empty("S-1-5-21-fixture".into());
    let app = Uuid::new_v4();
    let id = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "app".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: r"C:\app\fixture.exe".into(),
        },
        template_ref: Template::Codex,
    });
    manifest.instances.push(Instance {
        id,
        application_id: app,
        name: "instance".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec!["old argument".into()],
        env: SavedEnvironment {
            set: [
                (
                    "Keep".into(),
                    EnvValue::Literal {
                        value: "keep value".into(),
                    },
                ),
                ("Change".into(), EnvValue::SecretRef { id: Uuid::new_v4() }),
                (
                    "Restore".into(),
                    EnvValue::Literal {
                        value: "remove override".into(),
                    },
                ),
            ]
            .into(),
            unset: vec!["OldUnset".into()],
        },
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    (manifest, id)
}
fn apply(edit: InstanceEdit) -> Result<Manifest, ValidationError> {
    let (manifest, instance_id) = fixture();
    registry::apply(
        manifest,
        &ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 1,
            action: ConfigAction::EditInstance { instance_id, edit },
        },
    )
    .map(|(m, _)| m)
}

#[test]
fn partial_edit_preserves_other_fields_and_env_changes_are_case_insensitive() {
    let secret = Uuid::new_v4();
    let manifest = apply(InstanceEdit {
        env: Some(EnvironmentEdit {
            set: vec![EnvironmentAssignment {
                name: "CHANGE".into(),
                secret_id: secret,
                value: "new secret".into(),
            }],
            unset: vec!["Absent".into()],
            inherit: vec!["RESTORE".into(), "oldunset".into()],
        }),
        ..InstanceEdit::default()
    })
    .unwrap();
    let instance = &manifest.instances[0];
    assert_eq!(instance.revision, 2);
    assert_eq!(instance.args, ["old argument"]);
    assert_eq!(instance.env.set.len(), 2);
    assert!(
        matches!(instance.env.set.get("Keep"), Some(EnvValue::Literal { value }) if value == "keep value")
    );
    assert!(
        matches!(instance.env.set.get("CHANGE"), Some(EnvValue::SecretRef { id }) if *id == secret)
    );
    assert_eq!(instance.env.unset, ["Absent"]);
    assert!(matches!(instance.network, NetworkBinding::Direct {}));
    assert!(matches!(instance.data, InstanceData::Original {}));
    assert!(
        !serde_json::to_string(&manifest)
            .unwrap()
            .contains("new secret")
    );
    let cleared = apply(InstanceEdit {
        args: Some(vec![]),
        cwd: Some(WorkingDirectory::Explicit {
            path: r"${app_dir}\work".into(),
        }),
        env: None,
    })
    .unwrap();
    assert!(cleared.instances[0].args.is_empty());
    assert_eq!(cleared.instances[0].env.set.len(), 3);
}

#[test]
fn invalid_edit_never_bypasses_managed_fields_or_instance_mode() {
    for edit in [
        InstanceEdit::default(),
        InstanceEdit {
            args: Some(vec!["--proxy-server=elsewhere".into()]),
            ..Default::default()
        },
        InstanceEdit {
            args: Some(vec!["${app_home}".into()]),
            ..Default::default()
        },
        InstanceEdit {
            cwd: Some(WorkingDirectory::Explicit {
                path: "relative".into(),
            }),
            ..Default::default()
        },
        InstanceEdit {
            args: Some(vec!["x".repeat(EDIT_LIMIT)]),
            ..Default::default()
        },
        InstanceEdit {
            env: Some(EnvironmentEdit {
                set: vec![EnvironmentAssignment {
                    name: "HTTP_PROXY".into(),
                    secret_id: Uuid::new_v4(),
                    value: "x".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
        InstanceEdit {
            env: Some(EnvironmentEdit {
                unset: vec!["NAME".into()],
                inherit: vec!["name".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        InstanceEdit {
            env: Some(EnvironmentEdit {
                set: vec![EnvironmentAssignment {
                    name: "N".into(),
                    secret_id: Uuid::nil(),
                    value: "x".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
        InstanceEdit {
            env: Some(EnvironmentEdit {
                set: vec![EnvironmentAssignment {
                    name: "N".into(),
                    secret_id: Uuid::new_v4(),
                    value: "nul\0value".into(),
                }],
                ..Default::default()
            }),
            ..Default::default()
        },
    ] {
        assert!(apply(edit).is_err());
    }
}
