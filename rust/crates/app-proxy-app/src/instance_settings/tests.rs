use super::*;
use app_proxy_core::model::*;

#[test]
fn private_input_keeps_empty_arguments_and_values_and_never_exposes_parse_contents() {
    let omitted = decode(br#"{"args":null,"cwd":null,"env":null}"#).unwrap();
    assert!(omitted.args.is_none() && omitted.cwd.is_none() && omitted.env.is_none());
    let edit = decode(br#"{"args":["two words",""],"cwd":{"kind":"application"},"env":{"set":[{"name":"TOKEN","value":"${app_home}"},{"name":"EMPTY","value":""}],"unset":["OLD"],"inherit":["RESTORED"]}}"#).unwrap();
    assert_eq!(edit.args.unwrap(), ["two words", ""]);
    let env = edit.env.unwrap();
    assert_eq!(env.set[0].value, "${app_home}");
    assert_eq!(env.set[1].value, "");
    assert!(!env.set[0].secret_id.is_nil());
    assert_ne!(env.set[0].secret_id, env.set[1].secret_id);
    for invalid in [
        br#"{"args":[private-sensitive-argument]}"#.as_slice(),
        br#"{"env":{"set":[{"name":"TOKEN","value":"private-sensitive-value","secret_id":"caller-id"}]}}"#.as_slice(),
        br#"{"args":[],"args":["private-sensitive-argument"]}"#.as_slice(),
    ] {
        assert!(matches!(decode(invalid), Err(Error::Invalid("INVALID_INSTANCE_EDIT_FILE"))));
    }
    assert!(matches!(
        decode(&vec![b' '; INPUT_LIMIT + 1]),
        Err(Error::Invalid("INSTANCE_EDIT_TOO_LARGE"))
    ));
}

#[test]
fn advanced_summary_is_bounded_and_omits_all_values_and_secret_identifiers() {
    let mut manifest = Manifest::empty("S-1-5-21-fixture".into());
    let id = Uuid::new_v4();
    let secret = Uuid::new_v4();
    let mut env = SavedEnvironment::default();
    for index in 0..128 {
        env.set.insert(
            format!("{}_{index}", "N".repeat(1024)),
            EnvValue::SecretRef { id: secret },
        );
        env.unset.push(format!("REMOVED_{index}"));
    }
    manifest.instances.push(Instance {
        id,
        application_id: Uuid::new_v4(),
        name: "fixture".into(),
        revision: 1,
        args: vec!["private-argument".into()],
        cwd: WorkingDirectory::Explicit {
            path: r"C:\private-working-directory".into(),
        },
        env,
        data: InstanceData::Original {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    let view = summary(&manifest, id).unwrap();
    assert_eq!(view.set_count, 128);
    assert_eq!(view.unset_count, 128);
    assert_eq!(view.set_names.len(), 64);
    assert!(view.names_truncated);
    assert!(!view.application_directory);
    let encoded = serde_json::to_string(&view).unwrap();
    assert!(encoded.len() < 32 * 1024);
    assert!(!encoded.contains("private-argument"));
    assert!(!encoded.contains("private-working-directory"));
    assert!(!encoded.contains(&secret.to_string()));
}
