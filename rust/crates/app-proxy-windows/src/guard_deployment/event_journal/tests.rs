use super::*;
use std::{cell::Cell, fs::OpenOptions};

fn file(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .unwrap()
}
fn bytes(file: &mut File) -> Vec<u8> {
    file.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).unwrap();
    bytes
}
fn replace(file: &mut File, value: &[u8]) {
    file.set_len(0).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file.write_all(value).unwrap();
    file.sync_all().unwrap();
}

#[test]
fn epoch_is_durable_before_start_and_failed_recovery_preserves_ownership() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = file(&temp.path().join("event.json"));
    let current = identity::current().unwrap();
    let store = Uuid::new_v4();
    let epoch = prepare(&mut journal, store, &current, |_, _| panic!("new journal")).unwrap();
    let saved = bytes(&mut journal);
    let record: EventOwner = serde_json::from_slice(&saved).unwrap();
    assert_eq!(record.epoch, epoch);
    assert_eq!(record.store, store);
    assert!(matches!(
        prepare(&mut journal, store, &current, |s, e| {
            assert_eq!((s, e), (store, epoch));
            Err(Error::Invalid("fixture conflict"))
        }),
        Err(Error::Invalid("fixture conflict"))
    ));
    assert_eq!(bytes(&mut journal), saved);
    let next = prepare(&mut journal, store, &current, |s, e| {
        assert_eq!((s, e), (store, epoch));
        Ok(())
    })
    .unwrap();
    assert_ne!(next, epoch);
    assert_eq!(
        serde_json::from_slice::<EventOwner>(&bytes(&mut journal))
            .unwrap()
            .epoch,
        next
    );
}

#[test]
fn malformed_or_wrong_scope_record_never_authorizes_recovery_or_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let mut journal = file(&temp.path().join("event.json"));
    let current = identity::current().unwrap();
    let store = Uuid::new_v4();
    prepare(&mut journal, store, &current, |_, _| unreachable!()).unwrap();
    let valid = bytes(&mut journal);
    let mut cases = vec![b"{".to_vec(), vec![b'x'; LIMIT as usize + 1]];
    for (field, value) in [
        ("format", serde_json::json!("old-format")),
        ("sid", serde_json::json!("S-1-5-18")),
        ("store", serde_json::json!(Uuid::new_v4())),
        ("session", serde_json::json!(current.session_id + 1)),
        ("epoch", serde_json::json!(Uuid::nil())),
        ("execute", serde_json::json!("anything")),
    ] {
        let mut changed: serde_json::Value = serde_json::from_slice(&valid).unwrap();
        changed[field] = value;
        cases.push(serde_json::to_vec(&changed).unwrap());
    }
    for case in cases {
        replace(&mut journal, &case);
        let called = Cell::new(false);
        assert!(
            prepare(&mut journal, store, &current, |_, _| {
                called.set(true);
                Ok(())
            })
            .is_err()
        );
        assert!(!called.get());
        assert_eq!(bytes(&mut journal), case);
    }
}

#[test]
fn retained_writer_excludes_other_generations_and_releases_on_exit() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("events-1.json");
    let journal = file(&path);
    let open = || {
        OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
    };
    assert_eq!(open().unwrap_err().raw_os_error(), Some(32));
    assert!(std::fs::rename(&path, temp.path().join("renamed.json")).is_err());
    drop(journal);
    let next = open().unwrap();
    assert_eq!(open().unwrap_err().raw_os_error(), Some(32));
    drop(next);
}
