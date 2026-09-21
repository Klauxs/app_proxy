use super::*;
use std::{
    os::windows::process::CommandExt,
    process::{Command, Stdio},
};

fn ticket() -> Ticket {
    Ticket {
        version: 1,
        request: Uuid::new_v4(),
        upgrade_from: None,
        store: Uuid::new_v4(),
        issuer: identity::current().unwrap(),
        source: serde_json::from_value(serde_json::json!({
            "image":{"volume_serial":1,"file_index":2},"size":123,"sha256":"a".repeat(64)
        }))
        .unwrap(),
    }
}

#[test]
fn immutable_install_ticket_is_bounded_strict_and_safe_as_one_argument() {
    let mut ticket = ticket();
    ticket.issuer.image_path = r#"C:\夹具 $()%\引号"与空格\host.exe"#.into();
    let encoded = encode(&ticket).unwrap();
    assert!(
        encoded
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    );
    let decoded = decode(&encoded).unwrap();
    assert_eq!(decoded.issuer, ticket.issuer);
    assert_eq!(decoded.store, ticket.store);
    for invalid in [
        String::new(),
        "ab0".into(),
        "zz".into(),
        "AB".into(),
        "00".repeat(TICKET_LIMIT),
    ] {
        assert!(decode(&invalid).is_err());
    }
    for field in ["version", "store", "issuer", "source", "command"] {
        let mut changed = serde_json::to_value(&ticket).unwrap();
        changed[field] = serde_json::json!("wrong");
        let bytes = serde_json::to_vec(&changed).unwrap();
        let encoded = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert!(decode(&encoded).is_err());
    }
    ticket.version = 2;
    assert!(decode(&encode(&ticket).unwrap()).is_err());
    ticket.version = 1;
    ticket.store = Uuid::nil();
    assert!(decode(&encode(&ticket).unwrap()).is_err());
    ticket.issuer.image_path = "x".repeat(TICKET_LIMIT).into();
    assert!(encode(&ticket).is_err());
}

#[test]
fn ordinary_install_entry_rejects_before_decoding_or_machine_writes() {
    identity::assert_ordinary_user().unwrap();
    assert!(matches!(
        elevated(&encode(&ticket()).unwrap()),
        Err(Error::Invalid("ELEVATED_USER_REQUIRED"))
    ));
    assert!(authorize_listener(Uuid::nil()).is_err());
}

#[test]
fn upgrade_ticket_binds_exact_previous_generation() {
    let mut value = ticket();
    let generation = Uuid::new_v4();
    value.upgrade_from = Some(generation);
    assert_eq!(
        decode(&encode(&value).unwrap()).unwrap().upgrade_from,
        Some(generation)
    );
    value.upgrade_from = Some(Uuid::nil());
    assert!(decode(&encode(&value).unwrap()).is_err());
}

#[test]
fn installer_wait_distinguishes_live_process_and_retained_exit_without_termination() {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "guard::guard_install::tests::wait_child",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let handle = identity::open(
        child.id(),
        PROCESS_QUERY_LIMITED_INFORMATION | windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE,
    )
    .unwrap();
    assert_eq!(wait(&handle, Duration::ZERO).unwrap(), None);
    assert_eq!(wait(&handle, Duration::from_secs(3)).unwrap(), Some(7));
    assert_eq!(wait(&handle, Duration::ZERO).unwrap(), Some(7));
    assert_eq!(child.wait().unwrap().code(), Some(7));
}

#[test]
#[ignore = "exact child process for installer wait contract"]
fn wait_child() {
    std::thread::sleep(Duration::from_millis(200));
    std::process::exit(7);
}
