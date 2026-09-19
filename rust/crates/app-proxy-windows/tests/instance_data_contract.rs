#![cfg(windows)]
use app_proxy_core::model::*;
use app_proxy_windows::store::Store;
use std::fs;
use uuid::Uuid;

fn populated(root: &std::path::Path) -> (Store, Uuid, Uuid) {
    let mut store = Store::create(root).unwrap();
    let current = store.load().unwrap();
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    manifest.store_id = current.store_id;
    manifest.owner_sid = current.owner_sid;
    manifest.applications[0].locator = ApplicationLocator::Exe {
        path: r"C:\Apps\Codex.exe".into(),
    };
    let ids = (manifest.instances[0].id, manifest.instances[1].id);
    store.commit(1, manifest).unwrap();
    (store, ids.0, ids.1)
}

#[test]
fn original_does_not_create_or_adopt_any_data_directory() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let (store, original, _) = populated(&root);
    let before = fs::read_dir(&root).unwrap().count();
    assert!(
        store
            .prepare_instance_data(original, None)
            .unwrap()
            .is_none()
    );
    assert!(!root.join("instances").exists());
    assert_eq!(fs::read_dir(root).unwrap().count(), before);
}

#[test]
fn inspection_never_creates_missing_directories_and_pins_owned_existing_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let (store, original, isolated) = populated(&root);
    assert!(
        store
            .inspect_instance_data(original, None)
            .unwrap()
            .is_none()
    );
    assert!(store.inspect_instance_data(isolated, None).is_err());
    assert!(!root.join("instances").exists());
    let prepared = store
        .prepare_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    let path = prepared.paths.root.clone();
    let user_data = prepared.paths.user_data.clone();
    let app_home = prepared.paths.app_home.clone();
    drop(prepared);
    let inspected = store
        .inspect_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    assert!(fs::rename(&path, temp.path().join("moved")).is_err());
    drop(inspected);
    fs::remove_dir(&app_home).unwrap();
    assert!(store.inspect_instance_data(isolated, None).is_err());
    assert!(!app_home.exists());
    assert!(user_data.exists());
    fs::write(path.join(".app-proxy-rust-data.json"), b"invalid owner").unwrap();
    assert!(store.inspect_instance_data(isolated, None).is_err());
    assert_eq!(
        fs::read(path.join(".app-proxy-rust-data.json")).unwrap(),
        b"invalid owner"
    );
}

#[test]
fn isolated_is_blank_then_reused_and_remove_keeps_data() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let (mut store, _, isolated) = populated(&root);
    let prepared = store
        .prepare_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    assert_eq!(fs::read_dir(&prepared.paths.user_data).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&prepared.paths.app_home).unwrap().count(), 0);
    let data_path = prepared.paths.user_data.join("user-state");
    fs::write(&data_path, "preserve fixture state").unwrap();
    let original_root = prepared.paths.root.clone();
    drop(prepared);
    let mut manifest = store.load().unwrap();
    manifest.instances[1].name = "renamed".into();
    manifest.instances[1].revision += 1;
    store.commit(2, manifest).unwrap();
    let prepared = store
        .prepare_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    assert_eq!(prepared.paths.root, original_root);
    assert_eq!(
        fs::read_to_string(&data_path).unwrap(),
        "preserve fixture state"
    );
    drop(prepared);
    let mut manifest = store.load().unwrap();
    manifest
        .instances
        .retain(|instance| instance.id != isolated);
    store.commit(3, manifest).unwrap();
    assert!(store.prepare_instance_data(isolated, None).is_err());
    assert_eq!(
        fs::read_to_string(data_path).unwrap(),
        "preserve fixture state"
    );
}

#[test]
fn unknown_existing_directory_and_wrong_owner_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let (store, _, isolated) = populated(&root);
    let target = root.join("instances").join(isolated.to_string());
    fs::create_dir_all(&target).unwrap();
    fs::write(target.join("keep"), "unowned data").unwrap();
    assert!(store.prepare_instance_data(isolated, None).is_err());
    assert_eq!(
        fs::read_to_string(target.join("keep")).unwrap(),
        "unowned data"
    );

    let root = temp.path().join("another-store");
    let (store, _, isolated) = populated(&root);
    let prepared = store
        .prepare_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    let marker = prepared.paths.root.join(".app-proxy-rust-data.json");
    drop(prepared);
    let mut data: serde_json::Value = serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    data["store_id"] = Uuid::new_v4().to_string().into();
    let bytes = serde_json::to_vec(&data).unwrap();
    fs::write(&marker, &bytes).unwrap();
    assert!(store.prepare_instance_data(isolated, None).is_err());
    assert_eq!(fs::read(&marker).unwrap(), bytes);
}

#[test]
fn prepared_roots_cannot_be_renamed_and_junctions_are_not_followed() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let (store, _, isolated) = populated(&root);
    let prepared = store
        .prepare_instance_data(isolated, None)
        .unwrap()
        .unwrap();
    assert!(fs::rename(&prepared.paths.root, temp.path().join("renamed")).is_err());
    let data = prepared.paths.user_data.clone();
    drop(prepared);
    fs::remove_dir(&data).unwrap();
    let other = temp.path().join("other");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("keep"), "untouched").unwrap();
    let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32/WindowsPowerShell/v1.0/powershell.exe");
    let output = std::process::Command::new(powershell).args(["-NoProfile", "-NonInteractive", "-Command", "$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:APP_PROXY_TEST_JUNCTION -Target $env:APP_PROXY_TEST_TARGET | Out-Null"])
        .env("APP_PROXY_TEST_JUNCTION", &data).env("APP_PROXY_TEST_TARGET", &other).output().unwrap();
    assert!(output.status.success());
    assert!(store.prepare_instance_data(isolated, None).is_err());
    assert_eq!(fs::read_to_string(other.join("keep")).unwrap(), "untouched");
    fs::remove_dir(data).unwrap();
}
