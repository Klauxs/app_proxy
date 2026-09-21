#![cfg(windows)]
use app_proxy_windows::store::Store;
use std::fs;

#[test]
fn creates_owned_store_and_serializes_writers() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let manifest = store.load().unwrap();
    let id = manifest.store_id;
    assert!(Store::open(&root).is_err());
    assert_eq!(store.commit(1, manifest).unwrap(), 2);
    assert_eq!(store.load().unwrap().revision, 2);
    let stale = store.load().unwrap();
    assert!(store.commit(1, stale).is_err());
    let previous: app_proxy_core::model::Manifest =
        serde_json::from_slice(&fs::read(root.join("backups/manifest.previous.json")).unwrap())
            .unwrap();
    assert_eq!(previous.revision, 1);
    drop(store);
    assert_eq!(Store::open(&root).unwrap().load().unwrap().store_id, id);
}

#[test]
fn corrupt_and_unknown_config_are_never_reset() {
    for contents in [
        b"{broken".as_slice(),
        br#"{"schema_version":999}"#.as_slice(),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        drop(Store::create(&root).unwrap());
        fs::write(root.join("manifest.json"), contents).unwrap();
        assert!(Store::open(&root).is_err());
        assert!(Store::create(&root).is_err());
        assert_eq!(fs::read(root.join("manifest.json")).unwrap(), contents);
    }
}

#[test]
fn newer_schema_is_refused_with_its_own_code_and_left_untouched() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["schema_version"] = (app_proxy_core::model::SCHEMA_VERSION + 1).into();
    manifest["added_by_a_newer_program"] = true.into();
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(app_proxy_windows::Error::Invalid("STORE_SCHEMA_NEWER"))
    ));
    assert_eq!(fs::read(path).unwrap(), bytes);
    assert!(fs::read_dir(root.join("backups")).unwrap().next().is_none());
}

#[test]
fn ownership_marker_version_is_independent_of_the_manifest_schema() {
    // The marker is written once and never rewritten. Tying it to the manifest
    // schema would lock every existing store out after the first schema bump.
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    let marker: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join(".app-proxy-rust-owned.json")).unwrap())
            .unwrap();
    assert_eq!(marker["schema_version"], 1);
}

#[test]
fn retired_ifeo_registration_is_rejected_without_resetting_the_store() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    let path = root.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    manifest["integrations"]["ifeo"] = serde_json::json!([{"id":"legacy-registration"}]);
    let bytes = serde_json::to_vec(&manifest).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        Store::open(&root),
        Err(app_proxy_windows::Error::Invalid(
            "LEGACY_IFEO_CLEANUP_REQUIRED"
        ))
    ));
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn secrets_are_immutable_and_missing_references_block_commit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let first = store.put_secret("private value").unwrap();
    let second = store.put_secret("changed").unwrap();
    assert_ne!(first, second);
    assert_eq!(store.read_secret(first).unwrap(), "private value");
    assert_eq!(store.read_secret(second).unwrap(), "changed");
    let mut example: app_proxy_core::model::Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let current = store.load().unwrap();
    example.store_id = current.store_id;
    example.owner_sid = current.owner_sid;
    // The example's package storage namespace must belong to this newly allocated store.
    if let app_proxy_core::model::InstanceData::Isolated {
        location: app_proxy_core::model::StorageLocation::PackageLocalState { namespace, .. },
    } = &mut example.instances[1].data
    {
        *namespace = example.store_id.to_string();
    }
    example.instances[0].env.set.insert(
        "PRIVATE_TOKEN".into(),
        app_proxy_core::model::EnvValue::SecretRef {
            id: uuid::Uuid::new_v4(),
        },
    );
    assert!(store.commit(1, example).is_err());
    assert_eq!(store.load().unwrap().revision, 1);
}

#[test]
fn rejects_foreign_store_and_nonempty_directory() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("user-file"), "keep").unwrap();
    assert!(Store::create(temp.path()).is_err());
    assert_eq!(
        fs::read_to_string(temp.path().join("user-file")).unwrap(),
        "keep"
    );
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    let path = root.join(".app-proxy-rust-owned.json");
    let mut marker: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    marker["owner_sid"] = "S-1-5-21-foreign".into();
    let changed = serde_json::to_vec(&marker).unwrap();
    fs::write(&path, &changed).unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(fs::read(path).unwrap(), changed);
}

#[test]
fn incomplete_initialization_and_oversized_config_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    drop(Store::create(&root).unwrap());
    fs::write(root.join(".initializing"), "interrupted").unwrap();
    assert!(Store::open(&root).is_err());
    fs::remove_file(root.join(".initializing")).unwrap();
    let path = root.join("manifest.json");
    fs::write(&path, vec![b' '; app_proxy_core::model::MANIFEST_LIMIT + 1]).unwrap();
    assert!(Store::open(&root).is_err());
    assert_eq!(
        fs::metadata(path).unwrap().len(),
        app_proxy_core::model::MANIFEST_LIMIT as u64 + 1
    );
}

#[test]
fn temporary_files_do_not_replace_manifest_and_failed_replace_preserves_it() {
    use std::os::windows::fs::OpenOptionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    fs::write(root.join(".tmp-interrupted"), "incomplete").unwrap();
    let current = store.load().unwrap();
    let original = fs::read(root.join("manifest.json")).unwrap();
    // Antivirus/editors can hold a reader that denies rename/delete.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(root.join("manifest.json"))
        .unwrap();
    assert!(store.commit(1, current).is_err());
    assert_eq!(fs::read(root.join("manifest.json")).unwrap(), original);
    drop(held);
    assert_eq!(store.commit(1, store.load().unwrap()).unwrap(), 2);
}

#[test]
fn broader_acl_is_rejected_on_root_and_secret_files() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = Store::create(&root).unwrap();
    let secret = store.put_secret("secret must stay local").unwrap();
    let path = root.join(format!("secrets/{secret}.json"));
    let icacls = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/icacls.exe");
    assert!(
        Command::new(&icacls)
            .arg(&path)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(store.read_secret(secret).is_err());
    drop(store);
    assert!(
        Command::new(&icacls)
            .arg(&root)
            .args(["/grant", "*S-1-1-0:(R)"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(Store::open(&root).is_err());
}

#[test]
fn junction_root_is_rejected_without_touching_target() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("target");
    drop(Store::create(&target).unwrap());
    let before = fs::read(target.join("manifest.json")).unwrap();
    let junction = temp.path().join("junction");
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = Command::new(powershell).args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:APP_PROXY_TEST_JUNCTION -Target $env:APP_PROXY_TEST_TARGET | Out-Null"])
        .env("APP_PROXY_TEST_JUNCTION", &junction).env("APP_PROXY_TEST_TARGET", &target).output().unwrap();
    assert!(output.status.success());
    assert!(Store::open(&junction).is_err());
    assert!(Store::create(&junction).is_err());
    assert_eq!(fs::read(target.join("manifest.json")).unwrap(), before);
    // Remove only the fixture junction itself; both resolved paths are in this temp directory.
    fs::remove_dir(junction).unwrap();
}

#[test]
fn directories_without_child_acl_inheritance_are_rejected() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = Store::create(&root).unwrap();
    let sid = app_proxy_windows::identity::current().unwrap().user_sid;
    let icacls = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/icacls.exe");
    assert!(
        Command::new(&icacls)
            .arg(root.join("secrets"))
            .args([
                "/inheritance:r",
                "/grant:r",
                &format!("*{sid}:(F)"),
                "*S-1-5-18:(F)",
                "*S-1-5-32-544:(F)"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    drop(store);
    assert_eq!(
        Store::open(&root).err().unwrap().to_string(),
        "STORE_ACL_NOT_INHERITABLE"
    );
}

#[test]
fn new_secret_acl_is_checked_before_writing_any_content() {
    use std::process::Command;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let store = Store::create(&root).unwrap();
    let icacls = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/icacls.exe");
    // Simulate permissions changing after store open; the new file would inherit world-read.
    assert!(
        Command::new(&icacls)
            .arg(root.join("secrets"))
            .args(["/grant", "*S-1-1-0:(OI)(CI)(R)"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(store.put_secret("MUST_NOT_REACH_DISK").is_err());
    for entry in fs::read_dir(root.join("secrets")).unwrap() {
        assert_eq!(entry.unwrap().metadata().unwrap().len(), 0);
    }
}
