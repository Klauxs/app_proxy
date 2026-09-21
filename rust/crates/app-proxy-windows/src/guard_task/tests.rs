use super::*;

fn spec() -> Spec {
    Spec::new(
        &identity::current().unwrap().user_sid,
        Uuid::new_v4(),
        Uuid::new_v4(),
        Path::new(r"C:\Program Files\AppProxy\Guard\测试\app-proxy-host.exe"),
    )
    .unwrap()
}

#[test]
fn fixed_task_scope_excludes_substitutions_and_other_stores() {
    let first = spec();
    let second = spec();
    assert_ne!(first.name, second.name);
    assert!(first.args.starts_with("event-listen --store "));
    assert!(first.args.contains(" --generation "));
    assert!(!first.args.contains("$("));
    assert!(!first.name.contains(['\\', '/']));
    assert!(
        Spec::new(
            &first.sid,
            Uuid::new_v4(),
            Uuid::nil(),
            Path::new(&first.path)
        )
        .is_err()
    );
    for path in [
        r"C:\%ROOT%\host.exe",
        r"C:\$(Arg0)\host.exe",
        r#"C:\bad"\host.exe"#,
        "relative.exe",
    ] {
        assert!(Spec::new(&first.sid, Uuid::new_v4(), Uuid::new_v4(), Path::new(path)).is_err());
    }
}

#[test]
fn native_unregistered_definition_roundtrips_and_rejects_changed_actions_or_principal() {
    // Creates only in-memory COM definitions; never registers or runs a task.
    let session = Session::connect().unwrap();
    let spec = spec();
    let task = build(&session.service, &spec).unwrap();
    verify_definition(&task, &spec).unwrap();
    assert!(session.find(&spec).unwrap().is_none());
    // SAFETY: these unregistered local COM objects stay on this test thread.
    unsafe {
        let action: IExecAction = task.Actions().unwrap().get_Item(1).unwrap().cast().unwrap();
        action
            .SetArguments(&BSTR::from("event-listen --store $(Arg0)"))
            .unwrap();
        assert!(verify_definition(&task, &spec).is_err());
        action.SetArguments(&BSTR::from(&spec.args)).unwrap();
        action
            .SetPath(&BSTR::from(r"C:\user-writable\other.exe"))
            .unwrap();
        assert!(verify_definition(&task, &spec).is_err());
        action.SetPath(&BSTR::from(&spec.path)).unwrap();
        task.Principal()
            .unwrap()
            .SetRunLevel(TASK_RUNLEVEL_LUA)
            .unwrap();
        assert!(verify_definition(&task, &spec).is_err());
        task.Principal()
            .unwrap()
            .SetRunLevel(TASK_RUNLEVEL_HIGHEST)
            .unwrap();
        task.Principal()
            .unwrap()
            .SetUserId(&BSTR::from("S-1-5-18"))
            .unwrap();
        assert!(verify_definition(&task, &spec).is_err());
    }
}

#[test]
fn native_event_definition_rejects_wrong_uri_generation_or_role_marker() {
    let session = Session::connect().unwrap();
    let spec = spec();
    assert_eq!(spec.uri, format!("\\{}", spec.name));
    // SAFETY: only fresh unregistered in-memory definitions are modified.
    unsafe {
        for change in 0..3 {
            let task = build(&session.service, &spec).unwrap();
            match change {
                0 => task
                    .RegistrationInfo()
                    .unwrap()
                    .SetURI(&BSTR::from("\\foreign"))
                    .unwrap(),
                1 => {
                    let action: IExecAction =
                        task.Actions().unwrap().get_Item(1).unwrap().cast().unwrap();
                    action
                        .SetArguments(&BSTR::from(format!("{}-changed", spec.args)))
                        .unwrap();
                }
                _ => task.SetData(&BSTR::from(LOGIN_MARKER)).unwrap(),
            }
            assert!(verify_definition(&task, &spec).is_err());
        }
    }
}

#[test]
fn native_definition_rejects_auto_triggers_extra_actions_and_changed_lifecycle() {
    let session = Session::connect().unwrap();
    let spec = spec();
    // SAFETY: unregistered in-memory definitions, retained on this COM apartment.
    unsafe {
        let task = build(&session.service, &spec).unwrap();
        task.Triggers()
            .unwrap()
            .Create(TASK_TRIGGER_REGISTRATION)
            .unwrap();
        assert!(verify_definition(&task, &spec).is_err());
        let task = build(&session.service, &spec).unwrap();
        task.Actions().unwrap().Create(TASK_ACTION_EXEC).unwrap();
        assert!(verify_definition(&task, &spec).is_err());
        // Automatic maintenance is not represented in Triggers. Adding it
        // raises the persisted compatibility above the fixed V2 demand task.
        let task = build(&session.service, &spec).unwrap();
        let settings: ITaskSettings3 = task.Settings().unwrap().cast().unwrap();
        let maintenance = settings.CreateMaintenanceSettings().unwrap();
        maintenance.SetPeriod(&BSTR::from("P1D")).unwrap();
        maintenance.SetDeadline(&BSTR::from("P2D")).unwrap();
        let mut count = -1;
        task.Triggers().unwrap().Count(&mut count).unwrap();
        assert_eq!(count, 0);
        assert!(verify_definition(&task, &spec).is_err());
        for which in 0..8 {
            let task = build(&session.service, &spec).unwrap();
            let settings = task.Settings().unwrap();
            match which {
                0 => settings.SetAllowHardTerminate(VARIANT_TRUE).unwrap(),
                1 => settings
                    .SetMultipleInstances(TASK_INSTANCES_PARALLEL)
                    .unwrap(),
                2 => settings.SetRestartCount(3).unwrap(),
                3 => settings.SetExecutionTimeLimit(&BSTR::from("PT1H")).unwrap(),
                4 => settings.SetEnabled(VARIANT_FALSE).unwrap(),
                5 => settings
                    .SetDisallowStartIfOnBatteries(VARIANT_TRUE)
                    .unwrap(),
                6 => settings.SetCompatibility(TASK_COMPATIBILITY_V1).unwrap(),
                _ => task
                    .Actions()
                    .unwrap()
                    .SetContext(&BSTR::from("foreign"))
                    .unwrap(),
            }
            assert!(verify_definition(&task, &spec).is_err());
        }
    }
}
