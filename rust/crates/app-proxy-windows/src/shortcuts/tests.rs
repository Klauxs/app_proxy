use super::*;
use std::os::windows::process::CommandExt;

#[test]
fn installer_menu_link_reuses_exact_entry_and_preserves_foreign_contents() {
    let root = tempfile::tempdir().unwrap();
    let frontend = std::env::current_exe().unwrap();
    let path = root.path().join("AppProxy.lnk");
    menu_entry_at(&frontend, &path).unwrap();
    let original = std::fs::read(&path).unwrap();
    menu_entry_at(&frontend, &path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(menu_entry_at(&root.path().join("other.exe"), &path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let foreign = root.path().join("foreign.lnk");
    std::fs::write(&foreign, b"foreign link").unwrap();
    assert!(menu_entry_at(&frontend, &foreign).is_err());
    assert_eq!(std::fs::read(foreign).unwrap(), b"foreign link");
}

// Fixture edits deliberately race Shell notifications. Retry only Windows
// sharing violations; never hide content/identity assertions or other errors.
pub(super) fn edit_fixture(mut edit: impl FnMut() -> std::io::Result<()>) -> std::io::Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match edit() {
            Err(error)
                if error.raw_os_error() == Some(32) && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            result => return result,
        }
    }
}

pub(super) fn setup() -> (tempfile::TempDir, Spec, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("数据 store");
    std::fs::create_dir(&home).unwrap();
    let host = root.path().join("app-proxy-host.exe");
    std::fs::write(&host, b"fixture, never executed").unwrap();
    let icon = root.path().join("fixture.ico");
    std::fs::write(&icon, b"icon path fixture only").unwrap();
    let spec = Spec {
        store_id: Uuid::new_v4(),
        instance_id: Uuid::new_v4(),
        home,
        host,
        icon,
    };
    let path = root
        .path()
        .join(filename("Claude : 分身", spec.instance_id).unwrap());
    (root, spec, path)
}

#[test]
fn canonical_host_path_roundtrips_through_shell_link() {
    let (_root, mut spec, path) = setup();
    spec.host = std::fs::canonicalize(&spec.host).unwrap();
    spec.icon = std::fs::canonicalize(&spec.icon).unwrap();
    let bytes = encode(&spec).unwrap();
    let staged = staging::prepare(&path, &spec, &bytes).unwrap();
    staging::publish(&staged, &path, &spec).unwrap();
    verify(&path, &spec, &staged.receipt).unwrap();
    remove(&path, &spec, &staged.receipt).unwrap();
}

#[test]
fn shell_link_roundtrip_uses_fixed_hidden_host_and_exact_owned_deletion() {
    let (_root, spec, path) = setup();
    let bytes = encode(&spec).unwrap();
    let receipt = publish(&path, &spec, &bytes).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    verify(&path, &spec, &receipt).unwrap();
    let command = wide(std::ffi::OsStr::new(&format!(
        "host {}",
        spec.arguments().unwrap()
    )))
    .unwrap();
    let words = crate::process_query::parse_arguments(&command[..command.len() - 1])
        .unwrap()
        .unwrap();
    assert_eq!(
        words,
        [
            "host".into(),
            "launch".into(),
            spec.instance_id.to_string().into(),
            "--home".into(),
            spec.home.as_os_str().to_owned(),
            "--notify".into()
        ]
    );
    remove(&path, &spec, &receipt).unwrap();
    assert!(!path.exists());
    assert!(spec.host.exists());
    assert!(spec.icon.exists());
    assert!(spec.home.exists());
}

#[test]
fn occupied_changed_and_replaced_links_are_preserved() {
    let (_root, spec, path) = setup();
    let bytes = encode(&spec).unwrap();
    std::fs::write(&path, b"user file").unwrap();
    assert!(matches!(
        publish(&path, &spec, &bytes),
        Err(Error::Invalid("SHORTCUT_PATH_OCCUPIED"))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"user file");
    std::fs::remove_file(&path).unwrap();
    let receipt = publish(&path, &spec, &bytes).unwrap();
    let mut modified = bytes.clone();
    modified.push(1);
    std::fs::write(&path, &modified).unwrap();
    assert!(matches!(
        remove(&path, &spec, &receipt),
        Err(Error::Invalid("SHORTCUT_CHANGED"))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), modified);
    std::fs::write(&path, &bytes).unwrap();
    std::fs::rename(&path, path.with_extension("moved.lnk")).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        remove(&path, &spec, &receipt),
        Err(Error::Invalid("SHORTCUT_CHANGED"))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn field_changes_runas_and_in_use_links_refuse_cleanup() {
    let (_root, spec, path) = setup();
    let bytes = encode(&spec).unwrap();
    let receipt = publish(&path, &spec, &bytes).unwrap();
    let mut changed = spec.clone();
    changed.instance_id = Uuid::new_v4();
    assert!(matches!(
        remove(&path, &changed, &receipt),
        Err(Error::Invalid("SHORTCUT_CONTENT_CONFLICT"))
    ));
    let mut changed = bytes.clone();
    // Shell Link Header's LinkFlags field, per MS-SHLLINK.
    let flags = u32::from_le_bytes(changed[20..24].try_into().unwrap()) | SLDF_RUNAS_USER.0 as u32;
    changed[20..24].copy_from_slice(&flags.to_le_bytes());
    std::fs::write(&path, &changed).unwrap();
    let mut matching_hash = receipt.clone();
    matching_hash.sha256 = Sha256::digest(&changed).into();
    assert!(matches!(
        remove(&path, &spec, &matching_hash),
        Err(Error::Invalid("SHORTCUT_CONTENT_CONFLICT"))
    ));
    std::fs::write(&path, &bytes).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&path)
        .unwrap();
    let started = std::time::Instant::now();
    assert!(matches!(
        remove(&path, &spec, &receipt),
        Err(Error::Invalid("SHORTCUT_FILE_BUSY"))
    ));
    assert!(started.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    drop(held);
    remove(&path, &spec, &receipt).unwrap();
}

#[test]
fn two_publishers_cannot_replace_each_other_or_leave_partial_links() {
    let (_root, spec, path) = setup();
    let bytes = encode(&spec).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut threads = Vec::new();
    for _ in 0..2 {
        let (spec, path, bytes, barrier) =
            (spec.clone(), path.clone(), bytes.clone(), barrier.clone());
        threads.push(std::thread::spawn(move || {
            barrier.wait();
            publish(&path, &spec, &bytes)
        }));
    }
    let mut receipts = threads
        .into_iter()
        .filter_map(|t| t.join().unwrap().ok())
        .collect::<Vec<_>>();
    assert_eq!(receipts.len(), 1);
    let receipt = receipts.pop().unwrap();
    verify(&path, &spec, &receipt).unwrap();
    remove(&path, &spec, &receipt).unwrap();
}

#[test]
fn transient_reader_is_bounded_and_file_is_revalidated_after_the_wait() {
    for change in [false, true] {
        let (_root, spec, path) = setup();
        let bytes = encode(&spec).unwrap();
        let receipt = publish(&path, &spec, &bytes).unwrap();
        let mut held = OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(40));
            if change {
                held.write_all(b"changed while waiting").unwrap();
                held.sync_all().unwrap();
            }
            drop(held);
        });
        let result = remove(&path, &spec, &receipt);
        release.join().unwrap();
        if change {
            assert!(matches!(result, Err(Error::Invalid("SHORTCUT_CHANGED"))));
            assert!(path.exists());
        } else {
            result.unwrap();
            assert!(!path.exists());
        }
    }
}

#[test]
fn mapped_reader_can_release_after_delete_access_is_granted() {
    use std::os::windows::io::{FromRawHandle, OwnedHandle};
    use windows_sys::Win32::System::Memory::*;
    let (_root, spec, path) = setup();
    let bytes = encode(&spec).unwrap();
    let receipt = publish(&path, &spec, &bytes).unwrap();
    let mapped_path = path.clone();
    let (ready, wait) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let source = OpenOptions::new()
            .read(true)
            .share_mode(7)
            .open(mapped_path)
            .unwrap();
        // SAFETY: read-only mapping of a live fixture handle; section adopted once,
        // view unmapped before section/file drop; no pointers leave this thread.
        unsafe {
            let section = CreateFileMappingW(
                source.as_raw_handle(),
                std::ptr::null(),
                PAGE_READONLY,
                0,
                0,
                std::ptr::null(),
            );
            assert!(!section.is_null());
            let section = OwnedHandle::from_raw_handle(section);
            let view = MapViewOfFile(section.as_raw_handle(), FILE_MAP_READ, 0, 0, 0);
            assert!(!view.Value.is_null());
            ready.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(60));
            assert_ne!(UnmapViewOfFile(view), 0);
        }
    });
    wait.recv_timeout(std::time::Duration::from_secs(2))
        .unwrap();
    let result = remove(&path, &spec, &receipt);
    reader.join().unwrap();
    result.unwrap();
    assert!(!path.exists());
}

#[test]
fn unsafe_paths_invalid_bytes_and_hardlinks_fail_without_deleting() {
    let (root, spec, path) = setup();
    assert!(
        encode(&Spec {
            host: root.path().join("cmd.exe"),
            ..spec.clone()
        })
        .is_err()
    );
    assert!(
        encode(&Spec {
            host: root.path().join("%TEMP%").join("app-proxy-host.exe"),
            ..spec.clone()
        })
        .is_err()
    );
    assert!(publish(&path, &spec, &[0; 4]).is_err());
    let oversized = Spec {
        home: PathBuf::from(format!("C:\\{}", "a".repeat(32700))),
        ..spec.clone()
    };
    assert!(matches!(
        oversized.arguments(),
        Err(Error::Invalid("SHORTCUT_COMMAND_TOO_LONG"))
    ));
    assert!(publish(&path, &spec, &vec![0; LIMIT + 1]).is_err());
    assert!(!path.exists());
    let bytes = encode(&spec).unwrap();
    let receipt = publish(&path, &spec, &bytes).unwrap();
    let alias = root.path().join("alias.lnk");
    std::fs::hard_link(&path, &alias).unwrap();
    assert!(matches!(
        remove(&path, &spec, &receipt),
        Err(Error::Invalid("SHORTCUT_FILE_INVALID"))
    ));
    assert!(path.exists());
    assert!(alias.exists());
    assert!(
        filename("../CON:*?\n", spec.instance_id)
            .unwrap()
            .starts_with("CON - ")
    );
    assert!(
        filename(" ... ", spec.instance_id)
            .unwrap()
            .starts_with("实例 - ")
    );
}

#[test]
fn directory_chain_is_pinned_and_junction_swap_at_open_is_rejected() {
    let (root, spec, _) = setup();
    let destination = root.path().join("desktop");
    let outside = root.path().join("outside");
    std::fs::create_dir(&destination).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let held = parent(&destination.join("test.lnk")).unwrap();
    assert!(std::fs::rename(&destination, root.path().join("renamed")).is_err());
    drop(held);
    let observed = pin_directory(&destination, || {
        std::fs::rename(&destination, root.path().join("renamed")).unwrap();
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "New-Item -ItemType Junction -Path $env:APP_PROXY_TEST_JUNCTION -Target $env:APP_PROXY_TEST_TARGET -ErrorAction Stop | Out-Null"])
            .env("APP_PROXY_TEST_JUNCTION", &destination).env("APP_PROXY_TEST_TARGET", &outside)
            .creation_flags(0x08000000).output().unwrap();
        assert!(output.status.success());
    });
    assert!(matches!(
        observed,
        Err(Error::Invalid("SHORTCUT_PARENT_INVALID"))
    ));
    let bytes = encode(&spec).unwrap();
    assert!(publish(&destination.join("test.lnk"), &spec, &bytes).is_err());
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    // Only remove the verified fixture junction itself, never traverse its target.
    std::fs::remove_dir(destination).unwrap();
}
