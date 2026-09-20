#![cfg(windows)]
use app_proxy_app::coordinator::Status;
use app_proxy_core::{ProcessIdentity, model::*};
use app_proxy_windows::{identity, process, store::Store};
use std::{fs, path::Path, process::Command};

fn cli(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_app-proxy"));
    command.arg("--home").arg(root);
    command
}
struct Owner(ProcessIdentity);
impl Owner {
    fn capture(root: &Path) -> Self {
        let output = cli(root).args(["status", "--json"]).output().unwrap();
        assert!(output.status.success());
        let status: Status = serde_json::from_slice(&output.stdout).unwrap();
        let observed = identity::inspect(status.coordinator_pid).unwrap();
        assert_eq!(
            observed.image_file,
            identity::file_identity(Path::new(env!("CARGO_BIN_EXE_app-proxy-host"))).unwrap()
        );
        Self(observed)
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        let _ = process::terminate_exact(&self.0);
    }
}

#[test]
fn redirected_menu_requires_console_before_creating_a_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("not created");
    for explicit in [false, true] {
        let mut command = cli(&root);
        if explicit {
            command.arg("menu");
        }
        let output = command.output().unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&output.stderr).contains("菜单需要交互终端"));
        assert!(!root.exists());
    }
    assert!(cli(&root).arg("--help").output().unwrap().status.success());
    assert!(!root.exists());
}

#[test]
#[ignore = "interactive console: edit arguments and environment, then exit without launching"]
fn console_advanced_settings_save_hidden_values_without_launching() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("advanced menu store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only; cannot execute").unwrap();
    let owner = Owner::capture(&root);
    let output = cli(&root)
        .args([
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "environment",
            "--direct",
            "--name",
            "advanced fixture",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let created: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let revision = created["receipt"]["revision"].as_u64().unwrap();
    println!(
        "Manage / instance 1 / advanced 10 / arguments 1: [\"menu-private-argument\",\"\"] / save 1."
    );
    println!(
        "Repeat manage / advanced / environment 3: MENU_PRIVATE_TOKEN = menu-private-value / save 1; exit 0."
    );
    assert!(cli(&root).status().unwrap().success());
    // Interactive pauses may outlive the idle coordinator; capture the current
    // fixture owner instead of assuming the initial PID still owns the store.
    drop(Owner::capture(&root));
    drop(owner);
    let store = Store::open(&root).unwrap();
    let saved = store.load().unwrap();
    assert_eq!(saved.revision, revision + 2);
    assert_eq!(saved.instances[0].args, ["menu-private-argument", ""]);
    let EnvValue::SecretRef { id } = saved.instances[0].env.set["MENU_PRIVATE_TOKEN"] else {
        panic!()
    };
    assert_eq!(store.read_secret(id).unwrap(), "menu-private-value");
    assert!(
        !fs::read_to_string(root.join("manifest.json"))
            .unwrap()
            .contains("menu-private-value")
    );
    assert!(store.launch_attempts().unwrap().is_empty());
}

#[test]
#[ignore = "interactive console: add Environment original direct, decline launch, rename, reject clone, exit"]
fn console_original_save_return_rename_and_unsupported_clone() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("menu store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only; cannot execute").unwrap();
    let owner = Owner::capture(&root);
    println!("FIXTURE EXE: {}", exe.display());
    println!(
        "Add other / Environment / default original / menu-original / direct; save, decline launch. Rename to menu-renamed; attempt clone (must reject before any input or network); exit."
    );
    assert!(cli(&root).status().unwrap().success());
    drop(Owner::capture(&root));
    drop(owner);
    let store = Store::open(&root).unwrap();
    let saved = store.load().unwrap();
    assert_eq!(saved.instances.len(), 1);
    assert_eq!(saved.applications.len(), 1);
    assert_eq!(saved.instances[0].name, "menu-renamed");
    assert!(matches!(saved.instances[0].data, InstanceData::Original {}));
    assert!(matches!(
        saved.instances[0].network,
        NetworkBinding::Direct {}
    ));
    assert!(saved.instances[0].guard.desired == Desired::Disabled);
    assert!(saved.profiles.is_empty());
    assert!(store.launch_attempts().unwrap().is_empty());
}

#[test]
#[ignore = "interactive console: save authenticated manual proxy, add instance with it, Return at missing-core installation, exit"]
fn console_missing_core_return_preserves_proxy_without_creating_instance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("menu store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only; cannot execute").unwrap();
    let owner = Owner::capture(&root);
    println!("FIXTURE EXE: {}", exe.display());
    println!(
        "Create HTTP menu-proxy at 127.0.0.1:1; user menu-user / password menu-secret-value (hidden). Add Environment original using this proxy; confirm, then Return at missing sing-box; exit."
    );
    assert!(cli(&root).env("PATH", "").status().unwrap().success());
    drop(Owner::capture(&root));
    drop(owner);
    let store = Store::open(&root).unwrap();
    let saved = store.load().unwrap();
    assert_eq!(saved.profiles.len(), 1);
    assert_eq!(saved.profiles[0].name, "menu-proxy");
    assert!(saved.instances.is_empty());
    assert!(saved.applications.is_empty());
    assert!(store.launch_attempts().unwrap().is_empty());
    assert!(
        !fs::read_to_string(root.join("manifest.json"))
            .unwrap()
            .contains("menu-secret-value")
    );
}

#[test]
#[ignore = "interactive console: begin manual authenticated proxy; Ctrl+C at hidden password must exit entire menu"]
fn console_cancel_password_exits_without_saving() {
    // The fixture parent shares the console: absorb its own Ctrl+C while the
    // foreground child handles cancellation. Run this test binary directly,
    // since cargo itself does not install this handler.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            tokio::signal::ctrl_c(),
        )
        .await;
    });
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("menu store");
    let owner = Owner::capture(&root);
    println!("Add manual HTTP proxy, choose username/password, press Ctrl+C at password.");
    assert_eq!(cli(&root).status().unwrap().code(), Some(5));
    drop(Owner::capture(&root));
    drop(owner);
    let store = Store::open(&root).unwrap();
    let saved = store.load().unwrap();
    assert!(saved.profiles.is_empty());
    assert!(saved.instances.is_empty());
    assert!(store.launch_attempts().unwrap().is_empty());
}

#[test]
#[ignore = "interactive console: pause at rename confirmation; rename fixture externally to external-edit, then confirm old menu summary"]
fn console_stale_confirmation_does_not_overwrite_external_edit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("menu store");
    let exe = temp.path().join("fixture.exe");
    fs::write(&exe, b"fixture only; cannot execute").unwrap();
    let owner = Owner::capture(&root);
    let created = cli(&root)
        .args([
            "instance",
            "create",
            "--exe",
            exe.to_str().unwrap(),
            "--adapter",
            "environment",
            "--direct",
            "--name",
            "original",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(created.status.success());
    let created: serde_json::Value = serde_json::from_slice(&created.stdout).unwrap();
    let id = created["receipt"]["entity_id"].as_str().unwrap();
    let revision = created["receipt"]["revision"].as_u64().unwrap();
    println!("RACE ROOT: {}\nRACE INSTANCE: {id}", root.display());
    println!(
        "Manage / rename to menu-edit; pause at confirmation; external CLI rename to external-edit; confirm stale menu; exit."
    );
    assert!(cli(&root).status().unwrap().success());
    drop(Owner::capture(&root));
    drop(owner);
    let store = Store::open(&root).unwrap();
    let saved = store.load().unwrap();
    assert_eq!(saved.revision, revision + 1);
    assert_eq!(saved.instances[0].name, "external-edit");
    assert!(store.launch_attempts().unwrap().is_empty());
}
