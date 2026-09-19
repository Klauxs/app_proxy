use super::*;
use std::os::windows::ffi::OsStrExt;

fn command(id: Uuid, target: &str) -> Vec<u16> {
    format!(r#""C:\Protected 中文\app-proxy-host.exe" ifeo-entry --registration {id} -- {target}"#)
        .encode_utf16()
        .collect()
}
fn fixture() -> (ifeo_rules::Registration, app_proxy_core::ProcessIdentity) {
    let mut current = identity::current().unwrap();
    current.image_path = r"C:\Protected 中文\app-proxy-host.exe".into();
    let record = serde_json::from_value(serde_json::json!({
        "format": "app-proxy-rust-ifeo-v1",
        "id": Uuid::new_v4(), "store_id": Uuid::new_v4(),
        "owner_sid": current.user_sid,
        "application_id": Uuid::new_v4(), "instance_id": Uuid::new_v4(),
        "deployment_generation": Uuid::new_v4(),
        "target": r"C:\Target 中文\target.exe",
        "target_image": current.image_file,
        "host": current.image_path,
        "host_image": current.image_file,
        "package_full_name": null,
    }))
    .unwrap();
    (record, current)
}

#[test]
fn native_target_parser_preserves_wide_command_and_windows_escaping() {
    let id = Uuid::new_v4();
    let target =
        r#""C:\Target 中文\target.exe" "" "a b" "quoted\"word" "C:\tail\\" -- --registration fake"#;
    let invocation = parse(&command(id, target)).unwrap();
    assert_eq!(invocation.registration_id(), id);
    assert_eq!(
        invocation.raw_target_command_line(),
        target.encode_utf16().collect::<Vec<_>>()
    );
    assert_eq!(
        invocation.target,
        PathBuf::from(r"C:\Target 中文\target.exe")
    );
    assert_eq!(
        invocation.arguments(),
        [
            "",
            "a b",
            "quoted\"word",
            "C:\\tail\\",
            "--",
            "--registration",
            "fake"
        ]
        .map(OsString::from)
    );

    let mut wide = command(id, r#""C:\Target 中文\target.exe" "#);
    wide.extend([0xd800, b'x' as u16]);
    let invocation = parse(&wide).unwrap();
    assert_eq!(
        invocation.arguments()[0].encode_wide().collect::<Vec<_>>(),
        [0xd800, b'x' as u16]
    );
    assert!(invocation.raw_target_command_line().contains(&0xd800));
}

#[test]
fn envelope_does_not_accept_external_launcher_options_or_search_later_delimiters() {
    let id = Uuid::new_v4();
    let normal = String::from_utf16(&command(id, r#""C:\Target 中文\target.exe""#)).unwrap();
    for value in [
        normal.replacen("ifeo-entry", "serve", 1),
        normal.replacen(" -- ", " --home C:\\Other -- ", 1),
        normal.replacen(" -- ", " ", 1),
        normal.replacen("--registration", "--registration-extra", 1),
        normal.replacen(&id.to_string(), &Uuid::nil().to_string(), 1),
        normal.replacen(&id.to_string(), "not-a-uuid", 1),
        format!(" {normal}"),
        normal.replacen(
            "\"C:\\Protected 中文\\app-proxy-host.exe\"",
            "app-proxy-host.exe",
            1,
        ),
        normal.replacen(" -- ", " --  ", 1),
        normal.replacen("\"C:\\Target 中文\\target.exe\"", "target.exe", 1),
        normal.replacen("\"C:\\Target 中文\\target.exe\"", "\"\"", 1),
    ] {
        assert!(parse(&value.encode_utf16().collect::<Vec<_>>()).is_err());
    }
    let mut nul = command(id, r"C:\target.exe");
    nul.push(0);
    assert!(parse(&nul).is_err());
    assert!(parse(&vec![b'a' as u16; COMMAND_LIMIT + 1]).is_err());
    assert!(parse(&command(id, "")).is_err());
}

#[test]
fn routing_requires_registered_paths_and_actual_host_identity() {
    let (record, current) = fixture();
    let invocation = parse(&command(record.id, r#""C:\Target 中文\target.exe""#)).unwrap();
    invocation.matches_record(&record, &current).unwrap();
    let case_alias = parse(&command(record.id, r#""\\?\c:\target 中文\TARGET.EXE""#)).unwrap();
    case_alias.matches_record(&record, &current).unwrap();
    for target in [
        r"C:\Other\target.exe",
        r"C:\Target\..\target.exe",
        r"C:\Target.\target.exe",
        r"C:\Target 中文\target.exe:stream",
        r"\\server\share\target.exe",
        r"C:\Target",
    ] {
        let value = parse(&command(record.id, &format!("\"{target}\""))).unwrap();
        assert!(value.matches_record(&record, &current).is_err());
    }
    let mut changed = current.clone();
    changed.image_file.file_index += 1;
    assert!(matches!(
        invocation.matches_record(&record, &changed),
        Err(Error::Invalid("IFEO_ENTRY_IMAGE_MISMATCH"))
    ));
    changed = current.clone();
    changed.image_path = r"C:\Portable\app-proxy-host.exe".into();
    assert!(invocation.matches_record(&record, &changed).is_err());
    changed = current;
    changed.user_sid.push_str("-1000");
    assert!(matches!(
        invocation.matches_record(&record, &changed),
        Err(Error::Invalid("IFEO_OWNER_MISMATCH"))
    ));
    let wrong_id = parse(&command(Uuid::new_v4(), r#""C:\Target 中文\target.exe""#)).unwrap();
    assert!(wrong_id.matches_record(&record, &changed).is_err());
}

#[test]
fn native_capture_reads_process_command_and_unknown_registration_cannot_verify() {
    let raw = raw_command_line().unwrap();
    let args = crate::process_query::parse_arguments(&raw)
        .unwrap()
        .unwrap();
    assert_eq!(args, std::env::args_os().collect::<Vec<_>>());
    // This test runner is not launched with the fixed IFEO envelope.
    assert!(capture().is_err());
    let invocation = parse(&command(Uuid::new_v4(), r"C:\Unknown\target.exe")).unwrap();
    assert!(invocation.verify().is_err());
}
