#![cfg(windows)]
use app_proxy_app::{configuration::CatalogPage, coordinator::Status};
use app_proxy_core::{ProcessIdentity, model::*};
use app_proxy_windows::{identity, process, store::Store};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use uuid::Uuid;

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}
fn ok(root: &Path, args: &[&str]) -> Value {
    let output = cli(root, args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}
struct Owner(Option<ProcessIdentity>);
impl Owner {
    fn capture(root: &Path) -> Self {
        let status: Status = serde_json::from_value(ok(root, &["status", "--json"])).unwrap();
        let observed = identity::inspect(status.coordinator_pid).unwrap();
        assert_eq!(
            observed.image_file,
            identity::file_identity(Path::new(env!("CARGO_BIN_EXE_app-proxy-host"))).unwrap()
        );
        Self(Some(observed))
    }
    fn stop(&mut self) {
        if let Some(identity) = self.0.take() {
            process::terminate_exact(&identity).unwrap();
        }
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(identity) = self.0.take() {
            let _ = process::terminate_exact(&identity);
        }
    }
}
fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("配置 store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only, never executed").unwrap();
    (temp, root, exe)
}

#[test]
fn core_cli_stop_receipt_survives_owner_restart_and_missing_profile_fails_safely() {
    let (_temp, root, _) = setup();
    let initial = ok(&root, &["core", "status", "--json"]);
    let mut owner = Owner::capture(&root);
    assert_eq!(initial["observed"], "stopped");
    let stopped = ok(&root, &["core", "stop", "--json"]);
    assert_eq!(stopped["result"]["outcome"]["outcome"], "stopped");
    let request_id = stopped["request_id"].as_str().unwrap();
    owner.stop();
    let replay = ok(&root, &["core", "request", request_id, "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(stopped, replay);
    let profile = Uuid::new_v4().to_string();
    let failed = cli(&root, &["core", "start", &profile, "--json"]);
    assert_eq!(failed.status.code(), Some(3));
    let failed: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(failed["result"]["outcome"]["outcome"], "failed");
    assert_eq!(
        ok(&root, &["core", "status", "--json"])["observed"],
        "stopped"
    );
    owner.stop();
}

#[test]
fn core_cli_interrupted_request_stays_unknown_across_restart_without_execution() {
    use app_proxy_core::core_control::CoreAction;
    let (_temp, root, _) = setup();
    let mut store = Store::create(&root).unwrap();
    let id = Uuid::new_v4();
    store
        .begin_core_request(id, Uuid::new_v4(), &CoreAction::Stop {})
        .unwrap();
    drop(store);
    let queried = cli(&root, &["core", "request", &id.to_string(), "--json"]);
    let mut owner = Owner::capture(&root);
    assert_eq!(queried.status.code(), Some(6));
    let first: Value = serde_json::from_slice(&queried.stdout).unwrap();
    assert_eq!(first["result"]["status"], "indeterminate");
    assert_eq!(
        ok(&root, &["core", "status", "--json"])["observed"],
        "stopped"
    );
    owner.stop();
    let queried = cli(&root, &["core", "request", &id.to_string(), "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(queried.status.code(), Some(6));
    assert_eq!(
        first,
        serde_json::from_slice::<Value>(&queried.stdout).unwrap()
    );
    owner.stop();
}
fn create(root: &Path, exe: &Path, extra: &[&str]) -> Value {
    let mut args = vec![
        "instance",
        "create",
        "--exe",
        exe.to_str().unwrap(),
        "--adapter",
        "codex",
        "--direct",
        "--json",
    ];
    args.extend_from_slice(extra);
    ok(root, &args)
}

#[test]
fn real_cli_lifecycle_keeps_identity_and_data_without_launching_an_application() {
    let (_temp, root, exe) = setup();
    let first = create(&root, &exe, &["--name", "原版"]);
    let mut owner = Owner::capture(&root);
    let original = first["receipt"]["entity_id"].as_str().unwrap();
    assert_eq!(first["application_started"], false);
    let request = first["request_id"].as_str().unwrap();
    let queried = ok(&root, &["instance", "request", request, "--json"]);
    assert_eq!(
        queried["result"]["outcome"]["receipt"]["entity_id"],
        original
    );
    let cloned = ok(
        &root,
        &["instance", "clone", original, "--name", "分身", "--json"],
    );
    let clone = cloned["receipt"]["entity_id"].as_str().unwrap();
    assert_ne!(original, clone);
    let listed: CatalogPage =
        serde_json::from_value(ok(&root, &["instance", "list", "--json"])).unwrap();
    assert_eq!(listed.applications.len(), 1);
    assert_eq!(listed.instances.len(), 2);
    assert!(
        !listed
            .instances
            .iter()
            .find(|i| i.id.to_string() == original)
            .unwrap()
            .isolated
    );
    assert!(
        listed
            .instances
            .iter()
            .find(|i| i.id.to_string() == clone)
            .unwrap()
            .isolated
    );
    // Populate only our fixture's owned instance directory between owner runs.
    owner.stop();
    let store = Store::open(&root).unwrap();
    let data = store
        .prepare_instance_data(Uuid::parse_str(clone).unwrap(), None)
        .unwrap()
        .unwrap();
    let saved_file = data.paths.app_home.join("keep.txt");
    fs::write(&saved_file, "retained fixture data").unwrap();
    drop(data);
    drop(store);
    let renamed = ok(&root, &["instance", "rename", clone, "新名称", "--json"]);
    owner = Owner::capture(&root);
    assert_eq!(renamed["receipt"]["entity_id"], clone);
    ok(&root, &["instance", "bind", clone, "--direct", "--json"]);
    let after: CatalogPage =
        serde_json::from_value(ok(&root, &["instance", "list", "--json"])).unwrap();
    assert_eq!(
        after
            .instances
            .iter()
            .find(|i| i.id.to_string() == clone)
            .unwrap()
            .name,
        "新名称"
    );
    ok(&root, &["instance", "remove", clone, "--json"]);
    ok(&root, &["instance", "remove", original, "--json"]);
    owner.stop();
    let store = Store::open(&root).unwrap();
    let manifest = store.load().unwrap();
    assert!(manifest.instances.is_empty());
    assert_eq!(manifest.applications.len(), 1);
    assert!(manifest.integrations.ifeo.is_empty());
    assert_eq!(
        fs::read_to_string(saved_file).unwrap(),
        "retained fixture data"
    );
    assert_eq!(fs::read(exe).unwrap(), b"fixture only, never executed");
}

#[test]
fn clone_only_creation_never_registers_original_and_alias_does_not_duplicate_application() {
    let (temp, root, exe) = setup();
    create(&root, &exe, &["--data", "isolated"]);
    let mut owner = Owner::capture(&root);
    let alias = temp.path().join("alias.exe");
    fs::hard_link(&exe, &alias).unwrap();
    create(&root, &alias, &["--data", "isolated", "--name", "second"]);
    owner.stop();
    let store = Store::open(&root).unwrap();
    let manifest = store.load().unwrap();
    assert_eq!(manifest.instances.len(), 2);
    assert_eq!(manifest.applications.len(), 1);
    assert!(
        manifest
            .instances
            .iter()
            .all(|i| matches!(i.data, InstanceData::Isolated { .. }))
    );
    assert!(manifest.integrations.ifeo.is_empty());
    assert!(!root.join("instances").exists()); // Data allocation remains deferred to launch.
}

#[test]
fn duplicate_original_is_rejected_and_cli_requires_explicit_network_choice() {
    let (temp, root, exe) = setup();
    let missing = cli(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "codex",
        ],
    );
    assert_eq!(missing.status.code(), Some(2));
    assert!(!root.exists());
    create(&root, &exe, &[]);
    let mut owner = Owner::capture(&root);
    let alias = temp.path().join("alias.exe");
    fs::hard_link(exe, &alias).unwrap();
    let duplicate = cli(
        &root,
        &[
            "instance",
            "create",
            "--exe",
            alias.to_str().unwrap(),
            "--adapter",
            "codex",
            "--direct",
            "--json",
        ],
    );
    assert_eq!(duplicate.status.code(), Some(4));
    let rejected: Value = serde_json::from_slice(&duplicate.stdout).unwrap();
    assert_eq!(rejected["outcome"]["code"], "DUPLICATE_ORIGINAL");
    let lookup = cli(
        &root,
        &[
            "instance",
            "request",
            rejected["request_id"].as_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(lookup.status.code(), Some(4));
    owner.stop();
    let manifest = Store::open(&root).unwrap().load().unwrap();
    assert_eq!(manifest.applications.len(), 1);
    assert_eq!(manifest.instances.len(), 1);
}

#[test]
fn list_pages_return_every_instance_without_arguments_or_environment_values() {
    let (_temp, root, exe) = setup();
    let mut store = Store::create(&root).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe { path: exe },
        template_ref: Template::Codex,
    });
    for n in 0..37 {
        let id = Uuid::new_v4();
        let mut env = SavedEnvironment::default();
        env.set.insert(
            "PRIVATE_TOKEN".into(),
            EnvValue::Literal {
                value: "never-display-this-secret".into(),
            },
        );
        manifest.instances.push(Instance {
            id,
            application_id: app,
            name: format!("fixture {n}"),
            revision: 1,
            data: InstanceData::Isolated {
                location: StorageLocation::Store {
                    relative_path: PathBuf::from("instances").join(id.to_string()),
                },
            },
            args: vec!["--private-argument=hidden-argument-value".into()],
            env,
            cwd: WorkingDirectory::Application {},
            network: NetworkBinding::Direct {},
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
    }
    store.commit(1, manifest).unwrap();
    drop(store);
    let output = cli(&root, &["instance", "list", "--json"]);
    let mut owner = Owner::capture(&root);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for forbidden in [
        "PRIVATE_TOKEN",
        "never-display-this-secret",
        "hidden-argument-value",
    ] {
        assert!(!text.contains(forbidden));
    }
    let catalog: CatalogPage = serde_json::from_str(&text).unwrap();
    assert_eq!(catalog.revision, 2);
    assert_eq!(catalog.instances.len(), 37);
    assert_eq!(catalog.applications.len(), 1);
    let ids: std::collections::HashSet<_> = catalog.instances.iter().map(|i| i.id).collect();
    assert_eq!(ids.len(), 37);
    owner.stop();
}
