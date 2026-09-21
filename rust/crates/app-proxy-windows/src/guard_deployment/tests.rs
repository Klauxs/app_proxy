use super::*;

fn record() -> Record {
    Record {
        format: FORMAT.into(),
        protocol: PROTOCOL,
        store_id: Uuid::new_v4(),
        generation: Uuid::new_v4(),
        owner_sid: identity::current().unwrap().user_sid,
        coordinator_path: PathBuf::from(r"C:\fixture\app-proxy-host.exe"),
        coordinator_image: FileIdentity {
            volume_serial: 1,
            file_index: 2,
        },
        host_image: FileIdentity {
            volume_serial: 1,
            file_index: 3,
        },
        image_size: 10,
        image_sha256: "a".repeat(64),
    }
}

#[test]
fn deployment_record_is_strict_and_bound_to_user_store_generation_and_protocol() {
    let mut record = record();
    let sid = record.owner_sid.clone();
    let store = record.store_id;
    let generation = record.generation;
    validate_record(&record, &sid, store, generation).unwrap();
    assert!(validate_record(&record, "S-1-5-18", store, generation).is_err());
    assert!(validate_record(&record, &sid, Uuid::new_v4(), generation).is_err());
    assert!(validate_record(&record, &sid, store, Uuid::new_v4()).is_err());
    assert!(validate_record(&record, &sid, Uuid::nil(), generation).is_err());
    record.protocol += 1;
    assert!(validate_record(&record, &sid, store, generation).is_err());
    record.protocol = PROTOCOL;
    for hash in ["A".repeat(64), "g".repeat(64), "0".repeat(63)] {
        record.image_sha256 = hash;
        assert!(validate_record(&record, &sid, store, generation).is_err());
    }
    record.image_sha256 = "a".repeat(64);
    for path in [
        r"relative.exe",
        r"\\server\share\host.exe",
        r"C:\a\..\host.exe",
        r"C:\host.cmd",
    ] {
        record.coordinator_path = path.into();
        assert!(validate_record(&record, &sid, store, generation).is_err());
    }
    let mut json = serde_json::to_value(&record).unwrap();
    json["execute"] = "arbitrary".into();
    assert!(serde_json::from_value::<Record>(json).is_err());
}

#[test]
fn source_uses_retained_file_identity_and_refuses_mutation_while_copying() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("host.exe");
    std::fs::write(&path, b"fixture source bytes").unwrap();
    let image = identity::file_identity(&path).unwrap();
    let mut source = Source::open(&path, &image).unwrap();
    assert_eq!(source.size, 20);
    assert_eq!(
        source.hash,
        format!("{:x}", Sha256::digest(b"fixture source bytes"))
    );
    assert!(OpenOptions::new().write(true).open(&path).is_err());
    assert!(std::fs::rename(&path, temp.path().join("moved.exe")).is_err());
    let parent = temp
        .path()
        .parent()
        .unwrap()
        .join(format!("guard-moved-{}", Uuid::new_v4()));
    assert!(std::fs::rename(temp.path(), &parent).is_err());
    let mut output = Vec::new();
    source.file.seek(SeekFrom::Start(0)).unwrap();
    source.file.read_to_end(&mut output).unwrap();
    assert_eq!(output, b"fixture source bytes");
    let mut wrong = image.clone();
    wrong.file_index ^= 1;
    assert!(matches!(
        Source::open(&path, &wrong),
        Err(Error::Invalid("GUARD_SOURCE_CHANGED"))
    ));
    drop(source);
    let expected = SourceExpectation {
        image: image.clone(),
        size: 20,
        sha256: format!("{:x}", Sha256::digest(b"fixture source bytes")),
    };
    Source::expected(&path, &expected).unwrap();
    // Same size and same physical file, so only the independent digest detects it.
    std::fs::write(&path, b"changed source bytes").unwrap();
    assert!(matches!(
        Source::expected(&path, &expected),
        Err(Error::Invalid("GUARD_SOURCE_CHANGED"))
    ));
    let changed = Source::open(&path, &image).unwrap();
    assert_ne!(
        changed.hash,
        format!("{:x}", Sha256::digest(b"fixture source bytes"))
    );
}

#[test]
fn issuer_requires_same_user_session_exact_identity_and_live_process_handle() {
    let current = identity::current().unwrap();
    verify_issuer(&current, &current).unwrap();
    let mut wrong = current.clone();
    wrong.creation_time ^= 1;
    assert!(matches!(
        verify_issuer(&current, &wrong),
        Err(Error::IdentityMismatch)
    ));
    wrong = current.clone();
    wrong.session_id = wrong.session_id.wrapping_add(1);
    assert!(matches!(
        verify_issuer(&current, &wrong),
        Err(Error::Invalid("GUARD_INSTALL_CONTEXT_MISMATCH"))
    ));
    wrong = current.clone();
    wrong.user_sid.push_str("-1");
    assert!(verify_issuer(&current, &wrong).is_err());
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "etw::tests::event_fixture_child"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let issuer = identity::inspect(child.id()).unwrap();
    let handle = verify_issuer(&current, &issuer).unwrap();
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(matches!(
        issuer_alive(&handle),
        Err(Error::Invalid("GUARD_INSTALL_ISSUER_EXITED"))
    ));
    assert!(verify_issuer(&current, &issuer).is_err());
}

#[test]
fn sparse_or_empty_images_are_bounded_before_read_and_ordinary_stage_does_not_write() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("host.exe");
    File::create(&path).unwrap();
    let image = identity::file_identity(&path).unwrap();
    assert!(matches!(
        Source::open(&path, &image),
        Err(Error::Invalid("GUARD_IMAGE_SIZE"))
    ));
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(IMAGE_LIMIT + 1)
        .unwrap();
    assert!(matches!(
        Source::open(&path, &image),
        Err(Error::Invalid("GUARD_IMAGE_SIZE"))
    ));
    let current = identity::current().unwrap();
    let store = Uuid::new_v4();
    let expected = SourceExpectation {
        image: current.image_file.clone(),
        size: 1,
        sha256: "0".repeat(64),
    };
    assert!(matches!(
        stage(store, &current, &expected),
        Err(Error::Invalid("ELEVATED_USER_REQUIRED"))
    ));
    assert!(Deployment::open(store, Uuid::nil()).is_err());
    // Querying a missing deployment must not create its machine namespace.
    assert!(Deployment::open(store, Uuid::new_v4()).is_err());
    let user = format!("{:x}", Sha256::digest(current.user_sid.as_bytes()));
    assert!(
        !program_files()
            .unwrap()
            .join("AppProxy/Guard")
            .join(&user[..16])
            .join(store.to_string())
            .exists()
    );
}
