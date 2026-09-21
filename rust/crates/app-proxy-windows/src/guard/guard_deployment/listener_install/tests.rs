use super::*;

#[test]
fn first_upgrade_has_no_pending_record() {
    let root = tempfile::tempdir().unwrap();
    let pending = root.path().join("listener-upgrade.json");
    assert!(read_upgrade(&pending).unwrap().is_none());
    assert!(!pending.exists());
}

#[test]
fn listener_intent_binds_scope_and_generation_and_rejects_partial_records() {
    let store = Uuid::new_v4();
    let sid = identity::current().unwrap().user_sid;
    let valid = serde_json::json!({"format":FORMAT,"store":store,"owner_sid":sid,"generation":Uuid::new_v4()});
    let mut cases = vec![];
    for (key, value) in [
        ("format", serde_json::json!("wrong")),
        ("store", serde_json::json!(Uuid::new_v4())),
        ("owner_sid", serde_json::json!("S-1-5-18")),
        ("generation", serde_json::json!(Uuid::nil())),
        ("execute", serde_json::json!("foreign.exe")),
    ] {
        let mut value_copy = valid.clone();
        value_copy[key] = value;
        cases.push(serde_json::to_vec(&value_copy).unwrap());
    }
    cases.extend([vec![], b"{".to_vec(), vec![b'x'; RECORD_LIMIT as usize + 1]]);
    let mut file = tempfile::tempfile().unwrap();
    let initial = serde_json::to_vec(&valid).unwrap();
    file.write_all(&initial).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    assert_eq!(read_record(&mut file, store, &sid).unwrap().store, store);
    for bytes in cases {
        file.set_len(0).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&bytes).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        assert!(read_record(&mut file, store, &sid).is_err());
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut after = Vec::new();
        file.read_to_end(&mut after).unwrap();
        assert_eq!(after, bytes);
    }
}

#[test]
fn only_missing_installation_objects_are_classified_as_absent() {
    for code in [2, 3] {
        assert!(matches!(
            missing(Error::Io(std::io::Error::from_raw_os_error(code))),
            Error::Invalid(app_proxy_core::error_code::GUARD_LISTENER_MISSING)
        ));
    }
    assert!(matches!(
        missing(Error::Io(std::io::Error::from_raw_os_error(5))),
        Error::Io(_)
    ));
    assert!(matches!(
        missing(Error::Windows {
            operation: "fixture",
            code: 2
        }),
        Error::Invalid(app_proxy_core::error_code::GUARD_LISTENER_MISSING)
    ));
    assert!(matches!(
        missing(Error::Windows {
            operation: "fixture",
            code: 5
        }),
        Error::Windows { code: 5, .. }
    ));
    assert!(matches!(
        Deployment::listener(Uuid::nil()),
        Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"))
    ));
    assert!(matches!(
        Deployment::listener(Uuid::new_v4()),
        Err(Error::Invalid(
            app_proxy_core::error_code::GUARD_LISTENER_MISSING
        ))
    ));
}
