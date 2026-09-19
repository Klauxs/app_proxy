use super::*;
use crate::{
    installation,
    process::{CreationMode, SpawnSpec, StartedProcess},
    store::Store,
};
use app_proxy_core::{EnvPatch, model::*};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn application() -> ResolvedApplication {
    installation::resolve(&ApplicationLocator::Exe {
        path: std::env::current_exe().unwrap(),
    })
    .unwrap()
}
fn prepared(root: &Path) -> (Store, PreparedData) {
    let mut store = Store::create(root).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    let id = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: std::env::current_exe().unwrap(),
        },
        template_ref: Template::Codex,
    });
    manifest.instances.push(Instance {
        id,
        application_id: app,
        name: "clone".into(),
        revision: 1,
        data: InstanceData::Isolated {
            location: StorageLocation::Store {
                relative_path: PathBuf::from(format!("instances/{id}")),
            },
        },
        args: vec![],
        env: SavedEnvironment::default(),
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    store.commit(manifest.revision, manifest).unwrap();
    let data = store.prepare_instance_data(id, None).unwrap().unwrap();
    (store, data)
}
fn words(values: &[&str]) -> Vec<OsString> {
    std::iter::once("fixture.exe")
        .chain(values.iter().copied())
        .map(Into::into)
        .collect()
}

#[test]
fn chromium_prefixes_duplicates_terminator_and_unavailable_arguments_are_distinct() {
    let app = application();
    let target = InstanceTarget::new(&app, None, Template::Claude).unwrap();
    let check = |args: &[&str]| target.classify(Some(&words(args))).unwrap();
    assert_eq!(check(&[]), (ProcessRole::Main, InstanceRelation::Target));
    assert_eq!(
        target.classify(None).unwrap(),
        (ProcessRole::Unknown, InstanceRelation::Unknown)
    );
    assert_eq!(
        target.classify(Some(&[])).unwrap(),
        (ProcessRole::Unknown, InstanceRelation::Unknown)
    );
    for prefix in ["--", "-", "/"] {
        assert_eq!(
            check(&[&format!("  {prefix}TyPe=renderer  ")]),
            (ProcessRole::Auxiliary, InstanceRelation::Unknown)
        );
        assert_eq!(
            check(&[&format!("{prefix}USER-DATA-DIR=")]),
            (ProcessRole::Unknown, InstanceRelation::Unknown)
        );
    }
    for args in [
        vec!["--user-data-dir", "C:\\somewhere"],
        vec!["--user-data-dir=C:\\a", "--user-data-dir=C:\\a"],
        vec!["--type=renderer", "--type=utility"],
        vec!["--single-argument", "--type=renderer"],
    ] {
        assert_eq!(
            check(&args),
            (ProcessRole::Unknown, InstanceRelation::Unknown)
        );
    }
    assert_eq!(
        check(&["--", "--type=renderer", "--user-data-dir=C:\\x"]),
        (ProcessRole::Main, InstanceRelation::Target)
    );
    assert_eq!(
        check(&["--type=unexpected"]),
        (ProcessRole::Unknown, InstanceRelation::Unknown)
    );
    assert_eq!(
        check(&["\u{0085}/TyPe=renderer\u{3000}"]),
        (ProcessRole::Auxiliary, InstanceRelation::Unknown)
    );
    assert_eq!(
        check(&["--type="]),
        (ProcessRole::Unknown, InstanceRelation::Unknown)
    );
    assert_eq!(
        check(&["--user-data-dir=C:\\unknown"]),
        (ProcessRole::Main, InstanceRelation::Unknown)
    );
}

#[test]
fn remote_device_relative_and_reparse_directories_stay_unresolved() {
    for path in [
        r"\\unreachable.invalid\share\data",
        r"\\?\UNC\unreachable.invalid\share\data",
        r"\\.\C:\data",
        r"C:relative",
        r"\rooted",
        r"relative",
        r"C:\one\..\two",
    ] {
        assert!(matches!(
            directory_identity(Path::new(path)),
            Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"))
        ));
    }
    let temp = tempfile::tempdir().unwrap();
    let destination = temp.path().join("target");
    std::fs::create_dir_all(destination.join("child")).unwrap();
    std::fs::write(destination.join("child/keep"), b"preserve").unwrap();
    let junction = temp.path().join("junction");
    use std::os::windows::process::CommandExt;
    let created = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(&junction)
        .arg(&destination)
        .creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW)
        .output()
        .unwrap();
    assert!(created.status.success(), "fixture junction creation failed");
    for path in [&junction, &junction.join("child")] {
        assert!(matches!(
            directory_identity(path),
            Err(Error::Invalid("INSTANCE_DIRECTORY_REPARSE"))
        ));
    }
    assert_eq!(
        std::fs::read(destination.join("child/keep")).unwrap(),
        b"preserve"
    );
    // Remove only this junction entry, never recursively traverse its destination.
    std::fs::remove_dir(junction).unwrap();
}

#[test]
fn physical_data_identity_separates_clones_and_never_claims_unmanaged_original() {
    let temp = tempfile::tempdir().unwrap();
    let (_store, data) = prepared(&temp.path().join("store"));
    let app = application();
    let target = InstanceTarget::new(&app, Some(&data), Template::Codex).unwrap();
    let check = |path: &Path, kind: Option<&str>| {
        let mut args = words(&[&format!("--user-data-dir={}", path.display())]);
        if let Some(kind) = kind {
            args.push(format!("--type={kind}").into());
        }
        target.classify(Some(&args)).unwrap()
    };
    assert_eq!(
        target.classify(Some(&words(&[]))).unwrap(),
        (ProcessRole::Main, InstanceRelation::Other)
    );
    assert_eq!(
        check(&data.paths.user_data, None),
        (ProcessRole::Main, InstanceRelation::Target)
    );
    assert_eq!(
        check(
            &std::fs::canonicalize(&data.paths.user_data).unwrap(),
            Some("gpu-process")
        ),
        (ProcessRole::Auxiliary, InstanceRelation::Target)
    );
    let other = temp.path().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("keep"), b"untouched").unwrap();
    let verbatim_root = std::fs::canonicalize(&data.paths.root).unwrap();
    let dotted = verbatim_root.join("user-data.");
    std::fs::create_dir(&dotted).unwrap();
    assert_ne!(
        directory_identity(&dotted).unwrap(),
        directory_identity(&data.paths.user_data).unwrap()
    );
    assert_eq!(
        check(&dotted, None),
        (ProcessRole::Main, InstanceRelation::Other)
    );
    std::fs::remove_dir(&dotted).unwrap();
    assert_eq!(
        check(&other, None),
        (ProcessRole::Main, InstanceRelation::Other)
    );
    assert_eq!(
        check(Path::new("relative"), None),
        (ProcessRole::Main, InstanceRelation::Unknown)
    );
    assert_eq!(
        check(&temp.path().join("missing"), None),
        (ProcessRole::Main, InstanceRelation::Unknown)
    );
    assert_eq!(
        check(&other.join("keep"), None),
        (ProcessRole::Main, InstanceRelation::Unknown)
    );
    assert_eq!(std::fs::read(other.join("keep")).unwrap(), b"untouched");
    assert!(InstanceTarget::new(&app, Some(&data), Template::Environment).is_err());
}

struct Child(StartedProcess);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.terminate();
    }
}

#[tokio::test]
async fn exact_native_child_is_classified_without_adoption_or_termination() {
    let _query = process_query::QUERY_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (_store, data) = prepared(&temp.path().join("store"));
    let app = application();
    let target = InstanceTarget::new(&app, Some(&data), Template::Codex).unwrap();
    let marker = temp.path().join("ready");
    let mut environment = EnvPatch::default();
    environment.set.insert(
        "APP_PROXY_ATTRIBUTION_FIXTURE".into(),
        marker.to_str().unwrap().into(),
    );
    // Rust test harness consumes the synthetic Chromium switch as --skip's value;
    // the fixture proves OS argv/identity attribution, not Electron semantics.
    let child = Child(
        process::spawn(SpawnSpec {
            exe: app.executable().to_owned(),
            args: vec![
                "--ignored".into(),
                "--exact".into(),
                "instance_process::tests::attribution_child".into(),
                "--skip".into(),
                format!("--user-data-dir={}", data.paths.user_data.display()).into(),
            ],
            cwd: temp.path().to_owned(),
            environment,
            mode: CreationMode::Normal,
        })
        .unwrap(),
    );
    let until = Instant::now() + Duration::from_secs(3);
    while !marker.exists() {
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let found = loop {
        match target.inspect(&child.0.identity).await {
            Err(Error::Invalid("PROCESS_QUERY_BUSY")) if Instant::now() < until => {
                tokio::task::yield_now().await
            }
            result => break result.unwrap(),
        }
    };
    assert_eq!(found.relation, InstanceRelation::Target);
    assert_eq!(found.role, ProcessRole::Main);
    assert_eq!(found.identity, child.0.identity);
    assert!(process::is_running_exact(&child.0.identity).unwrap());
    let mut forged = child.0.identity.clone();
    forged.creation_time += 10;
    assert!(target.inspect(&forged).await.is_err());
    let copied_exe = temp.path().join("same-name.exe");
    std::fs::copy(std::env::current_exe().unwrap(), &copied_exe).unwrap();
    let other_app = installation::resolve(&ApplicationLocator::Exe { path: copied_exe }).unwrap();
    assert_eq!(
        InstanceTarget::new(&other_app, None, Template::Codex)
            .unwrap()
            .inspect(&child.0.identity)
            .await
            .unwrap()
            .relation,
        InstanceRelation::Other
    );
    let aliases = tempfile::tempdir_in(std::env::current_exe().unwrap().parent().unwrap()).unwrap();
    let alias = aliases.path().join("alias.exe");
    std::fs::hard_link(std::env::current_exe().unwrap(), &alias).unwrap();
    let alias_app = installation::resolve(&ApplicationLocator::Exe { path: alias }).unwrap();
    let alias_target = InstanceTarget::new(&alias_app, Some(&data), Template::Claude).unwrap();
    let found = loop {
        match alias_target.inspect(&child.0.identity).await {
            Err(Error::Invalid("PROCESS_QUERY_BUSY")) if Instant::now() < until => {
                tokio::task::yield_now().await
            }
            result => break result.unwrap(),
        }
    };
    assert_eq!(found.relation, InstanceRelation::Target);
    assert!(process::is_running_exact(&child.0.identity).unwrap());
}

#[test]
#[ignore = "native instance attribution fixture run by parent"]
fn attribution_child() {
    std::fs::write(
        std::env::var_os("APP_PROXY_ATTRIBUTION_FIXTURE").unwrap(),
        b"ready",
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}
