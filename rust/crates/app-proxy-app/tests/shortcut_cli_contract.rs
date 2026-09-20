#![cfg(windows)]
use app_proxy_app::coordinator::Status;
use app_proxy_core::{ProcessIdentity, model::*};
use app_proxy_windows::{
    identity, process,
    shortcuts::{
        self, Spec,
        journal::{Action, Plan, Request},
    },
    store::Store,
};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};
use uuid::Uuid;

struct Fixture {
    temp: Option<tempfile::TempDir>,
    root: PathBuf,
    instance: Uuid,
    owner: Option<ProcessIdentity>,
}
impl Fixture {
    fn new(exe: PathBuf) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("shortcut store");
        let mut store = Store::create(&root).unwrap();
        let mut manifest = store.load().unwrap();
        let app = Uuid::new_v4();
        let instance = Uuid::new_v4();
        manifest.applications.push(Application {
            id: app,
            name: "shortcut test".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path: exe },
            template_ref: Template::Environment,
        });
        manifest.instances.push(Instance {
            id: instance,
            application_id: app,
            name: format!("AppProxy contract {instance}"),
            revision: 1,
            data: InstanceData::Original {},
            args: vec!["private-argument-fixture".into()],
            env: SavedEnvironment::default(),
            cwd: WorkingDirectory::Application {},
            network: NetworkBinding::Direct {},
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
        store.commit(1, manifest).unwrap();
        drop(store);
        Self {
            temp: Some(temp),
            root,
            instance,
            owner: None,
        }
    }
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_app-proxy"));
        command.arg("--home").arg(&self.root).args(args);
        command
    }
    fn cli(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
    fn ok(&self, args: &[&str]) -> Value {
        let output = self.cli(args);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("private-argument-fixture"));
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn connect(&mut self) {
        let status: Status = serde_json::from_value(self.ok(&["status", "--json"])).unwrap();
        let owner = identity::inspect(status.coordinator_pid).unwrap();
        assert_eq!(
            owner.image_file,
            identity::file_identity(Path::new(env!("CARGO_BIN_EXE_app-proxy-host"))).unwrap()
        );
        self.owner = Some(owner);
    }
    fn stop(&mut self) {
        if let Some(owner) = self.owner.take() {
            process::terminate_exact(&owner).unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            let _ = process::terminate_exact(&owner);
        }
        // Exact journal evidence controls cleanup, including explicit desktop
        // tests. No deletion by guessed name or recursive desktop cleanup.
        let cleanup = (|| -> app_proxy_windows::Result<()> {
            let mut store = Store::open(&self.root)?;
            if let Some((active, _)) = store.instance_shortcut(self.instance)? {
                if active.action == Action::Remove {
                    store.resume_shortcut(active.id)?;
                } else {
                    store.apply_shortcut(
                        &Request {
                            id: Uuid::new_v4(),
                            instance_id: self.instance,
                            expected_revision: store.load()?.revision,
                            action: Action::Remove,
                            expected_creation: Some(active.id),
                        },
                        None,
                    )?;
                }
            }
            Ok(())
        })();
        if let Err(error) = cleanup
            && let Some(temp) = self.temp.take()
        {
            eprintln!(
                "Fixture cleanup unresolved ({error:?}); retained recovery store at {}",
                temp.keep().display()
            );
        }
    }
}

#[test]
fn real_cli_queries_removes_and_replays_owned_link_without_installation() {
    let mut fixture = Fixture::new(PathBuf::from(r"C:\missing-shortcut-fixture\absent.exe"));
    let path = fixture.temp.as_ref().unwrap().path().join("owned.lnk");
    let mut store = Store::open(&fixture.root).unwrap();
    let create = Request {
        id: Uuid::new_v4(),
        instance_id: fixture.instance,
        expected_revision: 2,
        action: Action::Create,
        expected_creation: None,
    };
    store
        .apply_shortcut(
            &create,
            Some(Plan {
                path: path.clone(),
                spec: Spec {
                    store_id: store.load().unwrap().store_id,
                    instance_id: fixture.instance,
                    home: fixture.root.clone(),
                    host: env!("CARGO_BIN_EXE_app-proxy-host").into(),
                    icon: fixture.temp.as_ref().unwrap().path().join("icon.ico"),
                },
            }),
        )
        .unwrap();
    drop(store);
    fixture.connect();
    let instance = fixture.instance.to_string();
    let view = fixture.ok(&["shortcut", "status", &instance, "--json"]);
    assert_eq!(view["integration"]["request"]["id"], create.id.to_string());
    assert_eq!(view["integration"]["status"]["status"], "created");
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"user modification fixture").unwrap();
    let failed = fixture.cli(&["shortcut", "remove", &instance, "--json"]);
    assert_eq!(failed.status.code(), Some(4));
    let report: Value = serde_json::from_slice(&failed.stdout).unwrap();
    assert_eq!(report["error"], "SHORTCUT_CHANGED");
    assert_eq!(report["status"]["status"], "pending");
    assert_eq!(fs::read(&path).unwrap(), b"user modification fixture");
    let remove_id = report["request_id"].as_str().unwrap();
    let view = fixture.ok(&["shortcut", "status", &instance, "--json"]);
    assert_eq!(view["integration"]["request"]["id"], remove_id);
    assert_eq!(
        view["integration"]["request"]["expected_creation"],
        create.id.to_string()
    );
    fs::write(&path, original).unwrap();
    assert_eq!(
        fixture.ok(&["shortcut", "request", remove_id, "--json"])["status"]["status"],
        "pending"
    );
    assert!(path.exists());
    assert_eq!(
        fixture.ok(&["shortcut", "resume", remove_id, "--json"])["status"]["status"],
        "removed"
    );
    assert!(!path.exists());
    assert_eq!(
        fixture.ok(&["shortcut", "resume", &create.id.to_string(), "--json"])["status"]["status"],
        "created"
    );
    assert!(!path.exists());
    assert!(fixture.ok(&["shortcut", "status", &instance, "--json"])["integration"].is_null());
}

#[test]
fn shortcut_cli_does_not_create_missing_store_or_accept_arbitrary_destinations() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("missing");
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .arg("--home")
        .arg(&root)
        .args(["shortcut", "status", &Uuid::new_v4().to_string(), "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!root.exists());
    let output = Command::new(env!("CARGO_BIN_EXE_app-proxy"))
        .args([
            "shortcut",
            "create",
            &Uuid::new_v4().to_string(),
            "--path",
            temp.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn real_cli_checks_repairs_and_replays_missing_owned_link_without_launching() {
    let mut fixture = Fixture::new(fixture_executable());
    let path = fixture
        .temp
        .as_ref()
        .unwrap()
        .path()
        .join("repair fixture.lnk");
    let mut store = Store::open(&fixture.root).unwrap();
    let icon = shortcuts::icons::cache_bytes(&store, b"fixture icon").unwrap();
    let create = Request {
        id: Uuid::new_v4(),
        instance_id: fixture.instance,
        expected_revision: 2,
        action: Action::Create,
        expected_creation: None,
    };
    store
        .apply_shortcut(
            &create,
            Some(Plan {
                path: path.clone(),
                spec: Spec {
                    store_id: store.load().unwrap().store_id,
                    instance_id: fixture.instance,
                    home: fixture.root.clone(),
                    host: env!("CARGO_BIN_EXE_app-proxy-host").into(),
                    icon,
                },
            }),
        )
        .unwrap();
    drop(store);
    fixture.connect();
    let id = fixture.instance.to_string();
    assert_eq!(
        fixture.ok(&["shortcut", "check", &id, "--json"])["state"],
        "verified"
    );
    fs::remove_file(&path).unwrap();
    assert_eq!(
        fixture.ok(&["shortcut", "check", &id, "--json"])["state"],
        "missing"
    );
    assert!(!path.exists());
    let repaired = fixture.ok(&["shortcut", "repair", &id, "--json"]);
    assert_eq!(repaired["status"]["status"], "repaired");
    assert_eq!(
        fixture.ok(&["shortcut", "check", &id, "--json"])["state"],
        "verified"
    );
    assert_eq!(
        fixture.ok(&["shortcut", "status", &id, "--json"])["integration"]["request"]["id"],
        create.id.to_string()
    );
    fs::remove_file(&path).unwrap();
    let request = repaired["request_id"].as_str().unwrap();
    fixture.ok(&["shortcut", "resume", request, "--json"]);
    assert!(!path.exists());
    fixture.ok(&["shortcut", "repair", &id, "--json"]);
    fs::write(&path, b"user edit").unwrap();
    assert_eq!(
        fixture.ok(&["shortcut", "check", &id, "--json"])["state"],
        "blocked"
    );
    let failed = fixture.cli(&["shortcut", "repair", &id, "--json"]);
    assert!(!failed.status.success());
    assert_eq!(fs::read(&path).unwrap(), b"user edit");
    fs::remove_file(&path).unwrap();
    fixture.ok(&["shortcut", "remove", &id, "--json"]);
    fixture.stop();
    let store = Store::open(&fixture.root).unwrap();
    assert!(store.launch_attempts().unwrap().is_empty());
    assert_eq!(store.load().unwrap().instances.len(), 1);
}

fn fixture_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_app-proxy-host"))
}

#[test]
#[ignore = "creates and removes one owned fixture link on the real user desktop; no target is launched"]
fn native_desktop_cli_creates_exact_fixed_host_link_with_complete_icon() {
    let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    let icon_bytes = shortcuts::icons::extract(&exe).unwrap();
    let mut fixture = Fixture::new(exe);
    fixture.connect();
    let instance = fixture.instance.to_string();
    let created = fixture.ok(&["shortcut", "create", &instance, "--json"]);
    assert_eq!(created["status"]["status"], "created");
    let path = PathBuf::from(created["status"]["path"].as_str().unwrap());
    assert_eq!(path.parent().unwrap(), shortcuts::desktop().unwrap());
    let view = fixture.ok(&["shortcut", "status", &instance, "--json"]);
    assert_eq!(view["integration"]["request"]["id"], created["request_id"]);
    let icons: Vec<_> = fs::read_dir(fixture.root.join("state"))
        .unwrap()
        .filter_map(|e| {
            let path = e.unwrap().path();
            (path.extension().is_some_and(|s| s == "ico")).then_some(path)
        })
        .collect();
    assert_eq!(icons.len(), 1);
    assert_eq!(fs::read(&icons[0]).unwrap(), icon_bytes);
    let receipt = shortcuts::Receipt {
        file: identity::file_identity(&path).unwrap(),
        sha256: {
            use sha2::{Digest, Sha256};
            Sha256::digest(fs::read(&path).unwrap()).into()
        },
    };
    let owner = app_proxy_windows::store::describe(&fixture.root).unwrap();
    shortcuts::verify(
        &path,
        &Spec {
            store_id: owner.store_id,
            instance_id: fixture.instance,
            home: fixture.root.clone(),
            host: env!("CARGO_BIN_EXE_app-proxy-host").into(),
            icon: icons[0].clone(),
        },
        &receipt,
    )
    .unwrap();
    assert_eq!(
        fixture.ok(&["shortcut", "remove", &instance, "--json"])["status"]["status"],
        "removed"
    );
    assert!(!path.exists());
    fixture.stop();
    assert!(
        Store::open(&fixture.root)
            .unwrap()
            .load()
            .unwrap()
            .integrations
            .shortcuts
            .is_empty()
    );
}

#[test]
#[ignore = "interactive PTY: manage instance / desktop shortcut / confirm create; repeat and confirm removal; exit"]
fn native_menu_creates_and_removes_desktop_shortcut() {
    let exe = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/cmd.exe");
    let mut fixture = Fixture::new(exe);
    fixture.connect();
    println!(
        "Fixture home: {}\nChoose 3, 1, 9, 1 to create; 3, 1, 9, 2, 1 to remove; 0 to exit. No application will be launched.",
        fixture.root.display()
    );
    assert!(fixture.command(&["menu"]).status().unwrap().success());
    fixture.stop();
    let store = Store::open(&fixture.root).unwrap();
    let manifest = store.load().unwrap();
    assert_eq!(manifest.revision, 4);
    assert!(manifest.integrations.shortcuts.is_empty());
    let journal: Value =
        serde_json::from_slice(&fs::read(fixture.root.join("state/shortcuts.json")).unwrap())
            .unwrap();
    assert_eq!(journal["entries"].as_array().unwrap().len(), 1);
    assert_eq!(journal["entries"][0]["created_revision"], 3);
    assert_eq!(journal["entries"][0]["removed_revision"], 4);
    assert!(!Path::new(journal["entries"][0]["plan"]["path"].as_str().unwrap()).exists());
}
