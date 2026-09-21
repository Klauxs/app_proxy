use super::*;

fn package(bytes: [&'static [u8]; 2], build: &'static str) -> Payload<'static> {
    Payload {
        build,
        files: bytes.map(|bytes| FilePayload {
            bytes,
            sha256: Box::leak(digest(bytes).into_boxed_str()),
        }),
    }
}
fn install(root: &Path, payload: &Payload<'_>) {
    Installation::begin(root, payload)
        .unwrap()
        .complete()
        .unwrap();
}

#[test]
fn initial_install_upgrade_and_same_package_retry_preserve_identity() {
    let root = tempfile::tempdir().unwrap();
    let first = package([b"cli-v1", b"host-v1"], "1");
    let next = package([b"cli-v2", b"host-v2"], "2");
    install(root.path(), &first);
    let transaction = Installation::begin(root.path(), &next).unwrap();
    let identity = app_proxy_windows::identity::file_identity(&root.path().join(NAMES[1])).unwrap();
    assert!(Installation::begin(root.path(), &next).is_err());
    drop(transaction); // UAC cancellation or installer process ending.
    install(root.path(), &next);
    assert_eq!(
        app_proxy_windows::identity::file_identity(&root.path().join(NAMES[1])).unwrap(),
        identity
    );
    install(root.path(), &next);
    assert_eq!(
        app_proxy_windows::identity::file_identity(&root.path().join(NAMES[1])).unwrap(),
        identity
    );
}

#[test]
fn fixed_package_recovers_a_published_but_unfinished_upgrade() {
    for initial in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("app");
        fs::create_dir(&root).unwrap();
        let first = package([b"cli-v1", b"host-v1"], "1");
        let broken = package([b"cli-v2", b"host-v2"], "2");
        let fixed = package([b"cli-v3", b"host-v3"], "3");
        if initial {
            install(&root, &first);
        }
        drop(Installation::begin(&root, &broken).unwrap());
        fs::write(temp.path().join("preserved.txt"), b"user data").unwrap();
        install(&root, &fixed);
        verify_installation(&root, &fixed.manifest().unwrap()).unwrap();
        assert!(!root.join(JOURNAL).exists());
        assert_eq!(
            fs::read(temp.path().join("preserved.txt")).unwrap(),
            b"user data"
        );
    }
}

#[test]
fn recovery_does_not_overwrite_modified_published_files() {
    let root = tempfile::tempdir().unwrap();
    let first = package([b"cli-v1", b"host-v1"], "1");
    let broken = package([b"cli-v2", b"host-v2"], "2");
    let fixed = package([b"cli-v3", b"host-v3"], "3");
    install(root.path(), &first);
    drop(Installation::begin(root.path(), &broken).unwrap());
    fs::write(root.path().join(NAMES[1]), b"user changed file").unwrap();
    assert!(Installation::begin(root.path(), &fixed).is_err());
    assert_eq!(
        fs::read(root.path().join(NAMES[1])).unwrap(),
        b"user changed file"
    );
    assert_eq!(fs::read(root.path().join(NAMES[0])).unwrap(), b"cli-v2");
}

#[test]
fn every_pair_replacement_boundary_rolls_back_without_touching_other_files() {
    for first_install in [false, true] {
        for failure in 0..5 {
            let root = tempfile::tempdir().unwrap();
            let first = package([b"cli-v1", b"host-v1"], "1");
            let next = package([b"cli-v2", b"host-v2"], "2");
            if !first_install {
                install(root.path(), &first);
            }
            let result = Installation::begin_with(root.path(), &next, |step| {
                if step == failure {
                    Err(Error::Invalid("TEST_INTERRUPT"))
                } else {
                    Ok(())
                }
            });
            assert!(result.is_err());
            if first_install {
                assert!(!root.path().join(NAMES[0]).exists());
                assert!(!root.path().join(NAMES[1]).exists());
            } else {
                verify_installation(root.path(), &first.manifest().unwrap()).unwrap();
            }
            assert!(!root.path().join(JOURNAL).exists());
            install(root.path(), &next);
        }
    }
}

#[test]
fn foreign_directory_modified_binary_and_invalid_payload_are_preserved() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("user.txt"), b"preserve").unwrap();
    let first = package([b"cli-v1", b"host-v1"], "1");
    assert!(Installation::begin(root.path(), &first).is_err());
    assert_eq!(fs::read(root.path().join("user.txt")).unwrap(), b"preserve");
    let owned = tempfile::tempdir().unwrap();
    install(owned.path(), &first);
    fs::write(owned.path().join(NAMES[0]), b"modified").unwrap();
    assert!(Installation::begin(owned.path(), &first).is_err());
    assert_eq!(fs::read(owned.path().join(NAMES[0])).unwrap(), b"modified");
    let mut invalid = package([b"cli", b"host"], "2");
    invalid.files[0].sha256 = "wrong";
    assert!(Installation::begin(owned.path(), &invalid).is_err());
}

#[test]
fn interrupted_unpublished_journal_recovers_before_another_upgrade() {
    let root = tempfile::tempdir().unwrap();
    let first = package([b"cli-v1", b"host-v1"], "1");
    let next = package([b"cli-v2", b"host-v2"], "2");
    install(root.path(), &first);
    let record = Journal {
        version: 1,
        old: Some(first.manifest().unwrap()),
        new: next.manifest().unwrap(),
        published: false,
    };
    atomic_json(&root.path().join(JOURNAL), &record).unwrap();
    fs::rename(
        root.path().join(NAMES[0]),
        root.path().join(format!(".setup-old-{}", NAMES[0])),
    )
    .unwrap();
    fs::write(root.path().join(NAMES[0]), next.files[0].bytes).unwrap();
    install(root.path(), &next);
    verify_installation(root.path(), &next.manifest().unwrap()).unwrap();
}

#[test]
fn verification_keeps_other_installers_excluded_after_runtime_gate_is_released() {
    let root = tempfile::tempdir().unwrap();
    let payload = package([b"cli", b"host"], "1");
    let mut transaction = Installation::begin(root.path(), &payload).unwrap();
    transaction.release_for_verification();
    assert!(!setup::requested_at(root.path()).unwrap());
    assert!(Installation::begin(root.path(), &payload).is_err());
    transaction.complete().unwrap();
}

#[test]
fn partial_staging_write_recovers_only_with_matching_payload_bytes() {
    for modified in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let first = package([b"cli-v1", b"host-v1"], "1");
        let next = package([b"cli-v2", b"host-v2"], "2");
        install(root.path(), &first);
        let record = Journal {
            version: 1,
            old: Some(first.manifest().unwrap()),
            new: next.manifest().unwrap(),
            published: false,
        };
        atomic_json(&root.path().join(JOURNAL), &record).unwrap();
        let stage = root.path().join(format!(".setup-new-{}", NAMES[0]));
        fs::write(
            &stage,
            if modified {
                &b"foreign"[..]
            } else {
                &b"cli-"[..]
            },
        )
        .unwrap();
        if modified {
            assert!(Installation::begin(root.path(), &next).is_err());
            assert_eq!(fs::read(stage).unwrap(), b"foreign");
            verify_installation(root.path(), &first.manifest().unwrap()).unwrap();
        } else {
            install(root.path(), &next);
        }
    }
}
