use super::*;

fn group(images: &[&[u8]]) -> Vec<u8> {
    let mut group = vec![0, 0, 1, 0];
    group.extend_from_slice(&(images.len() as u16).to_le_bytes());
    for (i, image) in images.iter().enumerate() {
        group.extend_from_slice(&[if i == 0 { 16 } else { 0 }, 0, 0, 0, 1, 0, 32, 0]);
        group.extend_from_slice(&(image.len() as u32).to_le_bytes());
        group.extend_from_slice(&(i as u16 + 1).to_le_bytes());
    }
    group
}

#[test]
fn preserves_all_entries_payloads_and_ico_offsets() {
    let dib = [40, 0, 0, 0, 16, 0, 0, 0];
    let png = b"\x89PNG\r\n\x1a\nopaque image resource";
    let input: [&[u8]; 2] = [&dib, png];
    let ico = assemble(&group(&input), |id| Ok(input[id as usize - 1].to_vec())).unwrap();
    assert_eq!(&ico[..6], &[0, 0, 1, 0, 2, 0]);
    assert_eq!(u32::from_le_bytes(ico[18..22].try_into().unwrap()), 38);
    assert_eq!(u32::from_le_bytes(ico[34..38].try_into().unwrap()), 46);
    assert_eq!(ico[6], 16);
    assert_eq!(ico[22], 0); // 256 pixels retains its on-disk representation.
    assert_eq!(&ico[38..46], &dib);
    assert_eq!(&ico[46..], png);
}

#[test]
fn malformed_groups_missing_resources_and_bounds_are_rejected() {
    for invalid in [
        vec![],
        vec![0; 5],
        vec![0, 0, 1, 0, 0, 0],
        vec![0, 0, 1, 0, 1, 1],
    ] {
        assert!(assemble(&invalid, |_| panic!("must reject before reading resources")).is_err());
    }
    let good = group(&[b"test"]);
    assert!(assemble(&good, |_| Err(Error::Invalid("TEST_RESOURCE_MISSING"))).is_err());
    assert!(assemble(&good, |_| Ok(vec![0; 3])).is_err());
    let mut invalid = good.clone();
    invalid[9] = 1;
    assert!(assemble(&invalid, |_| panic!("reserved field")).is_err());
    invalid = good.clone();
    invalid[14..18].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(assemble(&invalid, |_| panic!("size limit")).is_err());
    let large = vec![1; IMAGE_LIMIT];
    let group = group(&[&large, &large]);
    let mut calls = 0;
    assert!(
        assemble(&group, |_| {
            calls += 1;
            Ok(large.clone())
        })
        .is_err()
    );
    assert_eq!(calls, 1); // aggregate cap before allocating the second image
}

#[test]
fn protected_cache_reuses_exact_content_and_preserves_changed_files() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::create(&root.path().join("store")).unwrap();
    let bytes = assemble(&group(&[b"image bytes"]), |_| Ok(b"image bytes".to_vec())).unwrap();
    let path = cache_bytes(&store, &bytes).unwrap();
    let identity = file_id(&information(&File::open(&path).unwrap()).unwrap());
    assert_eq!(cache_bytes(&store, &bytes).unwrap(), path);
    assert_eq!(
        file_id(&information(&File::open(&path).unwrap()).unwrap()),
        identity
    );
    std::fs::write(&path, b"user change").unwrap();
    assert!(matches!(
        cache_bytes(&store, &bytes),
        Err(Error::Invalid("ICON_CACHE_CONFLICT"))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"user change");
    let different = cache_bytes(&store, b"other content").unwrap();
    assert_ne!(path, different);
    let alias = root.path().join("alias.ico");
    std::fs::hard_link(&different, &alias).unwrap();
    assert!(matches!(
        cache_bytes(&store, b"other content"),
        Err(Error::Invalid("ICON_CACHE_CONFLICT"))
    ));
    assert!(alias.exists());
}

#[test]
fn invalid_executables_do_not_produce_cache_entries() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::create(&root.path().join("store")).unwrap();
    let exe = root.path().join("fixture.exe");
    std::fs::write(&exe, b"not executable, never run").unwrap();
    assert!(cache(&store, &exe).is_err());
    assert!(extract(Path::new("relative.exe")).is_err());
    let entries = std::fs::read_dir(store.root().join("state")).unwrap();
    assert!(
        !entries
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().starts_with("icon-"))
    );
}

#[test]
fn source_is_held_against_writes_and_observed_replacement_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.exe");
    std::fs::write(&path, b"source bytes").unwrap();
    let source = open_source(&path).unwrap();
    let (_, expected) = crate::installation::inspect_file(&source).unwrap();
    verify_source(&path, &expected).unwrap();
    assert!(std::fs::write(&path, b"replacement").is_err());
    assert!(std::fs::rename(&path, root.path().join("moved.exe")).is_err());
    drop(source);
    std::fs::rename(&path, root.path().join("moved.exe")).unwrap();
    assert!(matches!(
        verify_source(&path, &expected),
        Err(Error::Invalid("ICON_SOURCE_CHANGED"))
    ));
    std::fs::write(&path, b"source bytes").unwrap();
    assert!(matches!(
        verify_source(&path, &expected),
        Err(Error::Invalid("ICON_SOURCE_CHANGED"))
    ));
    assert_eq!(std::fs::read(&path).unwrap(), b"source bytes");
}

#[test]
#[ignore = "read-only real EXE resource extraction; requires APP_PROXY_TEST_ICON_EXE"]
fn native_executable_icon_is_cached_and_shell_link_roundtrips() {
    let executable =
        PathBuf::from(std::env::var_os("APP_PROXY_TEST_ICON_EXE").expect("explicit source EXE"));
    let root = tempfile::tempdir().unwrap();
    let store = Store::create(&root.path().join("store")).unwrap();
    let icon = cache(&store, &executable).unwrap();
    let bytes = std::fs::read(&icon).unwrap();
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    assert!(count > 0);
    for entry in bytes[6..6 + 16 * count].as_chunks::<16>().0 {
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        assert!(size > 0 && offset >= 6 + 16 * count && offset + size <= bytes.len());
    }
    // Have the OS decode the durable ICO, independently of our resource parser.
    let name = wide(icon.as_os_str()).unwrap();
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    // SAFETY: fixed IMAGE_ICON type, terminated owned ICO path, no code execution.
    let decoded = unsafe {
        LoadImageW(
            std::ptr::null_mut(),
            name.as_ptr(),
            IMAGE_ICON,
            0,
            0,
            LR_LOADFROMFILE | LR_DEFAULTSIZE,
        )
    };
    assert!(!decoded.is_null());
    // SAFETY: successful LoadImageW without LR_SHARED returns an owned HICON.
    unsafe {
        DestroyIcon(decoded);
    }
    let spec = Spec {
        store_id: store.load().unwrap().store_id,
        instance_id: Uuid::new_v4(),
        home: store.root().into(),
        host: root.path().join("app-proxy-host.exe"),
        icon,
    };
    let link = root.path().join("temporary.lnk");
    let receipt = publish(&link, &spec, &encode(&spec).unwrap()).unwrap();
    verify(&link, &spec, &receipt).unwrap();
    remove(&link, &spec, &receipt).unwrap();
    println!(
        "Read-only EXE icon validation: {count} images, {} bytes",
        bytes.len()
    );
}
