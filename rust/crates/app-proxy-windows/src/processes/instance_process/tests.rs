use super::*;
use crate::{
    installation,
    process::{SpawnSpec, StartedProcess},
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
fn proxy_evidence_distinguishes_missing_conflicting_and_unreadable_arguments() {
    let endpoint: SocketAddr = "127.0.0.1:32123".parse().unwrap();
    let check = |args: &[&str]| proxy_arguments(Some(&words(args)), endpoint);
    for arg in [
        "--proxy-server=http://127.0.0.1:32123",
        "--proxy-server=127.0.0.1:32123",
        "-PROXY-SERVER=HTTP://127.0.0.1:32123",
        "/proxy-server=http://127.0.0.1:32123",
    ] {
        assert_eq!(check(&[arg]), ProxyArguments::Matching);
    }
    assert_eq!(check(&[]), ProxyArguments::Mismatched);
    for args in [
        vec!["--proxy-server"],
        vec!["--proxy-server=direct://"],
        vec!["--proxy-server=http://127.0.0.1:32124"],
        vec!["--proxy-server=socks5://127.0.0.1:32123"],
        vec![
            "--proxy-server=http://127.0.0.1:32123",
            "--proxy-server=http://127.0.0.1:32123",
        ],
        vec!["--proxy-server=http://127.0.0.1:32123", "--no-proxy-server"],
        vec![
            "--proxy-server=http://127.0.0.1:32123",
            "--proxy-pac-url=private-value",
        ],
        vec![
            "--proxy-server=http://127.0.0.1:32123",
            "--proxy-auto-detect",
        ],
        vec![
            "--proxy-server=http://127.0.0.1:32123",
            "--proxy-bypass-list=*",
        ],
        vec!["--", "--proxy-server=http://127.0.0.1:32123"],
    ] {
        assert_eq!(check(&args), ProxyArguments::Mismatched);
    }
    for args in [
        vec!["--proxy-server=http://localhost:32123"],
        vec!["--proxy-server=http=127.0.0.1:32123;https=127.0.0.1:32123"],
        vec!["--proxy-server=http://127.0.0.1:32123,direct://"],
        vec!["--proxy-server=unknown://127.0.0.1:32123"],
        vec!["--single-argument"],
        vec!["--user-data-dir="],
        vec!["--user-data-dir=C:\\one", "--user-data-dir=C:\\two"],
    ] {
        assert_eq!(check(&args), ProxyArguments::Unknown);
    }
    assert_eq!(proxy_arguments(None, endpoint), ProxyArguments::Unknown);
    assert_eq!(
        proxy_arguments(Some(&[]), endpoint),
        ProxyArguments::Unknown
    );
    assert_eq!(
        proxy_arguments(
            Some(&words(&["--proxy-server=http://[::1]:32123"])),
            "[::1]:32123".parse().unwrap()
        ),
        ProxyArguments::Matching
    );
    // A URL/file payload after the terminator is not a proxy switch.
    assert_eq!(
        check(&[
            "--proxy-server=http://127.0.0.1:32123",
            "--",
            "--no-proxy-server"
        ]),
        ProxyArguments::Matching
    );
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
                "processes::instance_process::tests::attribution_child".into(),
                "--skip".into(),
                format!("--user-data-dir={}", data.paths.user_data.display()).into(),
                "--skip".into(),
                "--proxy-server=http://127.0.0.1:32123".into(),
            ],
            cwd: temp.path().to_owned(),
            environment,
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
            Err(Error::Invalid(app_proxy_core::error_code::PROCESS_QUERY_BUSY))
                if Instant::now() < until =>
            {
                tokio::task::yield_now().await
            }
            result => break result.unwrap(),
        }
    };
    assert_eq!(found.relation, InstanceRelation::Target);
    assert_eq!(found.role, ProcessRole::Main);
    assert_eq!(found.identity, child.0.identity);
    let endpoint: SocketAddr = "127.0.0.1:32123".parse().unwrap();
    assert_eq!(
        target
            .inspect_proxy(&child.0.identity, endpoint)
            .await
            .unwrap()
            .proxy,
        ProxyArguments::Matching
    );
    assert_eq!(
        target
            .inspect_proxy(&child.0.identity, "127.0.0.1:32124".parse().unwrap())
            .await
            .unwrap()
            .proxy,
        ProxyArguments::Mismatched
    );
    assert!(
        target
            .inspect_proxy(&child.0.identity, "192.0.2.1:32123".parse().unwrap())
            .await
            .is_err()
    );
    let (_other_store, other_data) = prepared(&temp.path().join("other-store"));
    let other_target = InstanceTarget::new(&app, Some(&other_data), Template::Codex).unwrap();
    let other = other_target
        .inspect_proxy(&child.0.identity, endpoint)
        .await
        .unwrap();
    assert_eq!(other.relation, InstanceRelation::Other);
    assert_eq!(other.proxy, ProxyArguments::Unknown);
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
            Err(Error::Invalid(app_proxy_core::error_code::PROCESS_QUERY_BUSY))
                if Instant::now() < until =>
            {
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
    if std::env::var_os("APP_PROXY_ATTRIBUTION_TREE").is_some() {
        let mut environment = EnvPatch::default();
        environment.unset.push("APP_PROXY_ATTRIBUTION_TREE".into());
        let child = Child(
            process::spawn(SpawnSpec {
                exe: std::env::current_exe().unwrap(),
                args: words(&[
                    "--ignored",
                    "--exact",
                    "processes::instance_process::tests::attribution_child",
                    "--skip",
                    "--type=renderer",
                ])[1..]
                    .to_vec(),
                cwd: std::env::current_dir().unwrap(),
                environment,
            })
            .unwrap(),
        );
        std::fs::write(
            std::env::var_os("APP_PROXY_ATTRIBUTION_TREE").unwrap(),
            serde_json::to_vec(&child.0.identity).unwrap(),
        )
        .unwrap();
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    std::fs::write(
        std::env::var_os("APP_PROXY_ATTRIBUTION_FIXTURE").unwrap(),
        b"ready",
    )
    .unwrap();
    std::thread::sleep(Duration::from_secs(30));
}

#[tokio::test]
async fn auxiliary_inherits_only_live_exact_ancestry_and_keeps_its_role() {
    let _query = process_query::QUERY_TEST_LOCK.lock().await;
    let temp = tempfile::tempdir().unwrap();
    let (_store, data) = prepared(&temp.path().join("store"));
    let app = application();
    let target = InstanceTarget::new(&app, Some(&data), Template::Codex).unwrap();
    for isolated in [false, true] {
        let receipt = temp.path().join(format!("tree-{isolated}"));
        let mut environment = EnvPatch::default();
        environment.set.insert(
            "APP_PROXY_ATTRIBUTION_TREE".into(),
            receipt.to_str().unwrap().into(),
        );
        environment.set.insert(
            "APP_PROXY_ATTRIBUTION_FIXTURE".into(),
            temp.path().join("ready").to_str().unwrap().into(),
        );
        let mut args = words(&[
            "--ignored",
            "--exact",
            "processes::instance_process::tests::attribution_child",
        ])[1..]
            .to_vec();
        if isolated {
            args.extend([
                "--skip".into(),
                format!("--user-data-dir={}", data.paths.user_data.display()).into(),
            ]);
        }
        let mut parent = Child(
            process::spawn(SpawnSpec {
                exe: app.executable().to_owned(),
                args,
                cwd: temp.path().into(),
                environment,
            })
            .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while !receipt.exists() {
            assert!(Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let child: ProcessIdentity =
            serde_json::from_slice(&std::fs::read(receipt).unwrap()).unwrap();
        struct Cleanup(ProcessIdentity);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = process::terminate_exact(&self.0);
            }
        }
        let _cleanup = Cleanup(child.clone());
        let observed = target.inspect(&child).await.unwrap();
        let grouped = target
            .inspect_candidates(
                &[parent.0.identity.clone(), child.clone()],
                Some("127.0.0.1:32123".parse().unwrap()),
            )
            .await
            .unwrap();
        let grouped_child = grouped.iter().find(|p| p.identity == child).unwrap();
        assert_eq!(grouped_child.role, observed.role);
        assert_eq!(grouped_child.relation, observed.relation);
        assert_eq!(grouped_child.proxy, ProxyArguments::Unknown);
        let parent_watch = process::watch_exit(&parent.0.identity).unwrap();
        let child_watch = process::watch_exit(&child).unwrap();
        assert_eq!(observed.role, ProcessRole::Auxiliary);
        assert_eq!(
            target
                .inspect_proxy(&child, "127.0.0.1:32123".parse().unwrap())
                .await
                .unwrap()
                .proxy,
            ProxyArguments::Unknown
        );
        assert_eq!(
            observed.relation,
            if isolated {
                InstanceRelation::Target
            } else {
                InstanceRelation::Other
            }
        );
        assert!(process::is_running_exact(&parent.0.identity).unwrap());
        assert!(valid_parent(&child, &parent.0.identity));
        let mut reused = parent.0.identity.clone();
        reused.creation_time = child.creation_time + 1;
        assert!(!valid_parent(&child, &reused));
        let mut foreign = parent.0.identity.clone();
        foreign.session_id += 1;
        assert!(!valid_parent(&child, &foreign));
        parent.0.terminate().unwrap();
        assert!(!parent_watch.is_running().unwrap());
        assert!(child_watch.is_running().unwrap());
        assert!(process::is_running_exact(&child).unwrap());
        // No live ancestor means the orphan cannot be excluded as another instance.
        assert!(target.inspect(&child).await.is_err());
    }
}
