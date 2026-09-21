use super::*;

#[test]
fn login_readiness_rejects_relocated_or_replaced_home_before_task_queries() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("original");
    let store = store::Store::create(&home).unwrap();
    let descriptor = store::describe(&home).unwrap();
    let registration = Registration {
        store_id: descriptor.store_id,
        owner_sid: descriptor.owner_sid,
        home: home.clone(),
        host: temp.path().join("missing/app-proxy-host.exe"),
    };
    let other = temp.path().join("other");
    let _other = store::Store::create(&other).unwrap();
    assert!(matches!(
        registration.ready(&other),
        Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"))
    ));
    drop(store);
    // Both literal paths are confined to the owned fixture directory.
    let moved = temp.path().join("moved");
    std::fs::rename(&home, &moved).unwrap();
    assert!(registration.ready(&moved).is_err());
    let _replacement = store::Store::create(&home).unwrap();
    assert!(matches!(
        registration.ready(&home),
        Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"))
    ));
}

fn registration() -> Registration {
    Registration {
        store_id: Uuid::new_v4(),
        owner_sid: identity::current().unwrap().user_sid,
        home: r"C:\fixture 空格\store\".into(),
        host: r"C:\fixture 空格\app-proxy-host.exe".into(),
    }
}

#[test]
fn login_scope_quotes_store_and_rejects_substitutions_or_other_users() {
    let original = registration();
    let spec = original.spec().unwrap();
    assert!(spec.login);
    assert!(spec.name.starts_with("AppProxy-Login-"));
    assert_eq!(spec.uri, format!("\\{}", spec.name));
    assert!(spec.args.contains(&original.store_id.to_string()));
    assert!(spec.args.contains(r#""C:\fixture 空格\store\\""#));
    for bad in [r"C:\%STORE%", r"C:\$(Arg0)", "relative", r"C:\a\..\b"] {
        let mut changed = original.clone();
        changed.home = bad.into();
        assert!(changed.spec().is_err());
        changed = original.clone();
        changed.host = bad.into();
        assert!(changed.spec().is_err());
    }
    let mut changed = original.clone();
    changed.owner_sid = "S-1-5-18".into();
    assert!(changed.spec().is_err());
    changed = original.clone();
    changed.store_id = Uuid::nil();
    assert!(changed.spec().is_err());
    changed = original;
    changed.host = r"C:\fixture\arbitrary.exe".into();
    assert!(changed.spec().is_err());
}

#[test]
fn login_acl_and_elevated_event_acl_are_not_interchangeable() {
    let sid = identity::current().unwrap().user_sid;
    let ordinary = security::login_sddl(&sid);
    let elevated = security::sddl(&sid);
    security::verify_login(&ordinary, &sid).unwrap();
    security::verify(&elevated, &sid).unwrap();
    assert!(security::verify(&ordinary, &sid).is_err());
    assert!(security::verify_login(&elevated, &sid).is_err());
    for bad in [
        ordinary.replace("D:P", "D:"),
        ordinary.replace(&sid, "WD"),
        format!("{ordinary}(A;;FR;;;WD)"),
        ordinary.replace("(A;;FA;;;BA)", ""),
    ] {
        assert!(security::verify_login(&bad, &sid).is_err());
    }
}

#[test]
fn native_login_definition_requires_exact_user_trigger_and_ordinary_principal() {
    let session = Session::connect().unwrap();
    let spec = registration().spec().unwrap();
    let task = build(&session.service, &spec).unwrap();
    verify_definition(&task, &spec).unwrap();
    assert!(session.find(&spec).unwrap().is_none());
    // SAFETY: only unregistered in-memory COM objects are changed in this test.
    unsafe {
        for change in 0..13 {
            let task = build(&session.service, &spec).unwrap();
            let trigger: ILogonTrigger = task
                .Triggers()
                .unwrap()
                .get_Item(1)
                .unwrap()
                .cast()
                .unwrap();
            match change {
                0 => trigger.SetUserId(&BSTR::new()).unwrap(),
                1 => trigger.SetUserId(&BSTR::from("S-1-5-18")).unwrap(),
                2 => trigger.SetEnabled(VARIANT_FALSE).unwrap(),
                3 => trigger.SetDelay(&BSTR::from("PT1M")).unwrap(),
                4 => trigger
                    .SetStartBoundary(&BSTR::from("2030-01-01T00:00:00"))
                    .unwrap(),
                5 => trigger
                    .SetEndBoundary(&BSTR::from("2031-01-01T00:00:00"))
                    .unwrap(),
                6 => trigger
                    .Repetition()
                    .unwrap()
                    .SetInterval(&BSTR::from("PT1M"))
                    .unwrap(),
                7 => trigger.SetExecutionTimeLimit(&BSTR::from("PT1M")).unwrap(),
                8 => task
                    .Principal()
                    .unwrap()
                    .SetRunLevel(TASK_RUNLEVEL_HIGHEST)
                    .unwrap(),
                9 => task
                    .Principal()
                    .unwrap()
                    .SetLogonType(TASK_LOGON_S4U)
                    .unwrap(),
                10 => {
                    task.Triggers()
                        .unwrap()
                        .Create(TASK_TRIGGER_REGISTRATION)
                        .unwrap();
                }
                11 => {
                    let action: IExecAction =
                        task.Actions().unwrap().get_Item(1).unwrap().cast().unwrap();
                    action
                        .SetArguments(&BSTR::from("serve --home C:\\other"))
                        .unwrap();
                }
                _ => task.SetData(&BSTR::from(MARKER)).unwrap(),
            }
            assert!(verify_definition(&task, &spec).is_err(), "change {change}");
        }
    }
}

#[test]
#[ignore = "registers then removes one current-user UUID login task; never runs it"]
fn native_login_registration_reuses_exact_definition_and_preserves_conflicts() {
    identity::assert_ordinary_user().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("login store");
    let store = store::Store::create(&home).unwrap();
    let manifest = store.load().unwrap();
    let host = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("app-proxy-host.exe");
    assert!(host.is_file(), "build the workspace host first");
    let record = Registration {
        store_id: manifest.store_id,
        owner_sid: manifest.owner_sid,
        home,
        host,
    };
    let spec = record.spec().unwrap();
    println!("LOGIN FIXTURE TASK: {}", spec.name);
    assert!(!record.exists_verified().unwrap());
    struct Cleanup(Registration);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = self.0.remove_idle();
        }
    }
    let cleanup = Cleanup(record.clone());
    register_spec(&spec).unwrap();
    register_spec(&spec).unwrap();
    assert!(record.exists_verified().unwrap());
    let mut conflicting = record.clone();
    conflicting.home = temp.path().join("changed store");
    assert!(register_spec(&conflicting.spec().unwrap()).is_err());
    assert!(conflicting.remove_idle().is_err());
    assert!(record.exists_verified().unwrap());
    // A removed/moved original store does not make the fixed recovery record
    // unusable; removal does not resolve its home or host on disk.
    drop(store);
    record.remove_idle().unwrap();
    assert!(!record.exists_verified().unwrap());
    record.remove_idle().unwrap();
    drop(cleanup);
}

#[test]
#[ignore = "explicit fixture recovery: APP_PROXY_TEST_LOGIN_RECORD contains a previously created registration"]
fn native_login_fixture_recovery_removes_only_the_exact_recorded_task() {
    let path =
        std::env::var_os("APP_PROXY_TEST_LOGIN_RECORD").expect("fixture recovery record required");
    let record: Registration = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(record.exists_verified().unwrap());
    record.remove_idle().unwrap();
    assert!(!record.exists_verified().unwrap());
}
