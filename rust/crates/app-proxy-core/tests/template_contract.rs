use app_proxy_core::model::*;
use app_proxy_core::template::{self, TemplatePaths};
use std::path::Path;

fn example() -> Manifest {
    serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap()
}
fn paths(isolated: bool) -> TemplatePaths<'static> {
    TemplatePaths {
        executable: Path::new(r"C:\应用 Program\client.exe"),
        instance_root: isolated.then(|| Path::new(r"D:\store 空白\instances\example")),
    }
}
fn compile(manifest: &Manifest, index: usize) -> template::TemplateOutput {
    template::compile(
        manifest,
        manifest.instances[index].id,
        paths(index == 1),
        |_| panic!("no secret expected"),
    )
    .unwrap()
}

#[test]
fn original_proxy_clears_all_inherited_isolation_and_has_no_direct_fallback() {
    let manifest = example();
    let result = compile(&manifest, 0);
    assert!(result.data.is_none());
    assert_eq!(result.args, ["--proxy-server=http://127.0.0.1:18099"]);
    for key in [
        "CODEX_HOME",
        "CODEX_ELECTRON_USER_DATA_PATH",
        "CLAUDE_CONFIG_DIR",
    ] {
        assert!(result.environment.unset.iter().any(|name| name == key));
        assert!(!result.environment.set.contains_key(key));
    }
    for key in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
        assert_eq!(result.environment.set[key], "http://127.0.0.1:18099");
    }
    assert_eq!(result.environment.set["NO_PROXY"], "");
    result.environment.validate().unwrap();
}

#[test]
fn codex_and_claude_isolated_paths_are_explicit_and_separate() {
    for (template, key, other) in [
        (Template::Codex, "CODEX_HOME", "CLAUDE_CONFIG_DIR"),
        (Template::Claude, "CLAUDE_CONFIG_DIR", "CODEX_HOME"),
    ] {
        let mut manifest = example();
        manifest.applications[0].template_ref = template;
        let result = compile(&manifest, 1);
        let data = result.data.as_ref().unwrap();
        assert_eq!(
            data.user_data,
            paths(true).instance_root.unwrap().join("user-data")
        );
        assert_eq!(
            data.app_home,
            paths(true).instance_root.unwrap().join("app-home")
        );
        assert_eq!(result.environment.set[key], data.app_home.to_str().unwrap());
        assert!(result.environment.unset.iter().any(|name| name == other));
        assert_eq!(
            result.args[0],
            format!("--user-data-dir={}", data.user_data.display()).as_str()
        );
        assert_eq!(result.args[1], "--no-proxy-server");
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY"] {
            assert!(result.environment.unset.iter().any(|key| key == name));
        }
        result.environment.validate().unwrap();
    }
}

#[test]
fn environment_adapter_adds_no_chromium_flags_and_ipv6_is_bracketed() {
    let mut manifest = example();
    manifest.instances.truncate(1);
    manifest.applications[0].template_ref = Template::Environment;
    manifest.instances[0].guard.desired = Desired::Disabled;
    manifest.profiles[0].endpoint.host = "::1".parse().unwrap();
    let result = compile(&manifest, 0);
    assert!(result.args.is_empty());
    assert_eq!(result.environment.set["HTTP_PROXY"], "http://[::1]:18099");
    manifest.instances[0].network = NetworkBinding::Direct {};
    let result = compile(&manifest, 0);
    assert!(result.args.is_empty());
    assert!(result.environment.set.is_empty());
}

#[test]
fn substitution_preserves_single_arguments_and_does_not_expand_values_twice() {
    let mut manifest = example();
    manifest.instances[1].args = vec![
        "--file=${user_data}/a b.json".into(),
        "${app_dir}".into(),
        String::new(),
        "quotes \" and \\".into(),
    ];
    manifest.instances[1].cwd = WorkingDirectory::Explicit {
        path: "${app_home}".into(),
    };
    let result = compile(&manifest, 1);
    assert_eq!(result.args.len(), 6);
    assert_eq!(
        result.args[0],
        r"--file=D:\store 空白\instances\example\user-data/a b.json"
    );
    assert_eq!(result.args[1], r"C:\应用 Program");
    assert_eq!(result.args[2], "");
    assert_eq!(result.cwd, result.data.unwrap().app_home);
    manifest.instances[0].args = vec!["${app_dir}".into()];
    let with_dollar = TemplatePaths {
        executable: Path::new(r"C:\literal ${user_data}\client.exe"),
        instance_root: None,
    };
    let result = template::compile(
        &manifest,
        manifest.instances[0].id,
        with_dollar,
        |_| panic!(),
    )
    .unwrap();
    assert_eq!(result.args[0], r"C:\literal ${user_data}");
}

#[test]
fn original_cannot_reference_isolated_paths_and_unknown_variables_are_rejected() {
    for name in ["user_data", "app_home", "instance_root"] {
        let mut manifest = example();
        manifest.instances[0].args.push(format!("${{{name}}}"));
        let error = template::compile(
            &manifest,
            manifest.instances[0].id,
            paths(false),
            |_| panic!(),
        )
        .err()
        .unwrap();
        assert_eq!(error.0, "ISOLATED_PATH_VARIABLE_IN_ORIGINAL");
    }
    for arg in ["${unknown}", "${app_dir", "${}"] {
        let mut manifest = example();
        manifest.instances[0].args.push(arg.into());
        assert!(manifest.validate().is_err());
    }
}

#[test]
fn secrets_empty_and_unset_keep_distinct_semantics_and_limits() {
    let mut manifest = example();
    let secret = uuid::Uuid::new_v4();
    let env = &mut manifest.instances[0].env;
    env.set
        .insert("ACCESS_TOKEN".into(), EnvValue::SecretRef { id: secret });
    env.set.insert(
        "EMPTY".into(),
        EnvValue::Literal {
            value: String::new(),
        },
    );
    env.unset.push("REMOVE_ME".into());
    let result = template::compile(&manifest, manifest.instances[0].id, paths(false), |id| {
        assert_eq!(id, secret);
        Ok("private token".into())
    })
    .unwrap();
    assert_eq!(result.environment.set["ACCESS_TOKEN"], "private token");
    assert_eq!(result.environment.set["EMPTY"], "");
    assert!(
        result
            .environment
            .unset
            .iter()
            .any(|name| name == "REMOVE_ME")
    );
    for (value, code) in [
        ("nul\0inside".into(), "INVALID_COMPILED_ENVIRONMENT"),
        (
            "x".repeat(template::ENVIRONMENT_LIMIT),
            "ENVIRONMENT_TOO_LARGE",
        ),
    ] {
        let error = template::compile(&manifest, manifest.instances[0].id, paths(false), |_| {
            Ok(value.clone())
        })
        .err()
        .unwrap();
        assert_eq!(error.0, code);
    }
}

#[test]
fn directory_context_must_match_mode_and_be_absolute() {
    let manifest = example();
    assert_eq!(
        template::compile(
            &manifest,
            manifest.instances[0].id,
            paths(true),
            |_| panic!()
        )
        .err()
        .unwrap()
        .0,
        "INSTANCE_PATH_MODE_MISMATCH"
    );
    assert_eq!(
        template::compile(
            &manifest,
            manifest.instances[1].id,
            paths(false),
            |_| panic!()
        )
        .err()
        .unwrap()
        .0,
        "INSTANCE_PATH_MODE_MISMATCH"
    );
    assert_eq!(
        template::compile(
            &manifest,
            manifest.instances[1].id,
            TemplatePaths {
                executable: Path::new("client.exe"),
                instance_root: paths(true).instance_root
            },
            |_| panic!()
        )
        .err()
        .unwrap()
        .0,
        "ABSOLUTE_PATH_REQUIRED"
    );
}
