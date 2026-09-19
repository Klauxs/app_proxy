use super::journal::{decode, encode};
use super::*;
use std::{ffi::OsStr, ptr};

struct Fixture {
    path: String,
    ifeo: String,
    product: String,
}
impl Fixture {
    fn new() -> Self {
        let name = format!("AppProxyRust-IfeoRulesFixture-{}", Uuid::new_v4());
        let path = format!(r"Software\{name}");
        let software = Key::open(HKEY_CURRENT_USER, "Software", true)
            .unwrap()
            .unwrap();
        let (root, created) = Key::create(software.0, &name, None).unwrap();
        assert!(created);
        Key::create(root.0, "IFEO", None).unwrap();
        Self {
            ifeo: format!(r"{path}\IFEO"),
            product: format!(r"{path}\Product"),
            path,
        }
    }
    fn roots(&self) -> Roots<'_> {
        Roots {
            hive: HKEY_CURRENT_USER,
            ifeo: &self.ifeo,
            product: &self.product,
            security: journal::Security::Fixture,
        }
    }
    fn parent(&self, record: &Registration) -> Key {
        let root = Key::path(HKEY_CURRENT_USER, &self.ifeo, true)
            .unwrap()
            .unwrap();
        Key::create(root.0, &record.basename().unwrap(), None)
            .unwrap()
            .0
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let path = crate::wide(OsStr::new(&self.path)).unwrap();
        // SAFETY: the subtree is this test's unique HKCU fixture, never real IFEO.
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, path.as_ptr());
        }
    }
}
fn record() -> Registration {
    let current = identity::current().unwrap();
    Registration {
        format: FORMAT.into(),
        id: Uuid::new_v4(),
        store_id: Uuid::new_v4(),
        owner_sid: current.user_sid,
        application_id: Uuid::new_v4(),
        instance_id: Uuid::new_v4(),
        deployment_generation: Uuid::new_v4(),
        target: r"C:\Fixture\中文 space\target.exe".into(),
        target_image: current.image_file.clone(),
        package_full_name: None,
        host: r"C:\Protected Fixture\app-proxy-host.exe".into(),
        host_image: current.image_file,
    }
}

#[test]
fn installation_and_removal_preserve_mitigations_and_original_parent_value() {
    for original in [None, Some(Value::dword(0)), Some(Value::dword(1))] {
        let fixture = Fixture::new();
        let roots = fixture.roots();
        let record = record();
        let parent = fixture.parent(&record);
        let mitigation = Value {
            kind: REG_BINARY,
            bytes: vec![1, 2, 3, 4],
        };
        parent.set("MitigationOptions", &mitigation).unwrap();
        if let Some(value) = &original {
            parent.set("UseFilter", value).unwrap();
        }
        roots.install(&record).unwrap();
        roots.verify(&record).unwrap();
        roots.install(&record).unwrap(); // Exact repeat does not create another rule.
        assert_eq!(parent.children().unwrap(), vec![record.filter()]);
        let read = roots.read(record.id).unwrap();
        assert_eq!(read.instance_id, record.instance_id);
        roots.remove(&record).unwrap();
        assert!(matches!(
            roots.read(record.id),
            Err(Error::Invalid("IFEO_NOT_REGISTERED"))
        ));
        assert_eq!(parent.value("UseFilter").unwrap(), original);
        assert_eq!(parent.value("MitigationOptions").unwrap(), Some(mitigation));
        assert!(parent.children().unwrap().is_empty());
    }
}

#[test]
fn foreign_debuggers_inactive_filters_and_changed_owned_values_are_preserved() {
    for conflict in 0..3 {
        let fixture = Fixture::new();
        let roots = fixture.roots();
        let record = record();
        let parent = fixture.parent(&record);
        match conflict {
            0 => parent
                .set("Debugger", &Value::string("foreign.exe").unwrap())
                .unwrap(),
            1 => {
                let (key, _) = Key::create(parent.0, "Foreign", None).unwrap();
                key.set(
                    "FilterFullPath",
                    &Value::string(r"C:\Other\app.exe").unwrap(),
                )
                .unwrap();
            }
            _ => parent
                .set("UseFilter", &Value::string("wrong type").unwrap())
                .unwrap(),
        }
        let before_filter = parent.value("UseFilter").unwrap();
        let before_debugger = parent.value("Debugger").unwrap();
        let before_children = parent.children().unwrap();
        assert!(roots.install(&record).is_err());
        assert_eq!(parent.value("UseFilter").unwrap(), before_filter);
        assert_eq!(parent.value("Debugger").unwrap(), before_debugger);
        assert_eq!(parent.children().unwrap(), before_children);
        assert!(roots.read(record.id).is_err());
    }
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let record = record();
    roots.install(&record).unwrap();
    let parent = fixture.parent(&record);
    let filter = Key::open(parent.0, &record.filter(), true)
        .unwrap()
        .unwrap();
    filter
        .set(
            "Debugger",
            &Value::string("third-party-change.exe").unwrap(),
        )
        .unwrap();
    assert!(roots.remove(&record).is_err());
    assert!(roots.read(record.id).is_ok());
    assert_eq!(
        filter.value("Debugger").unwrap().unwrap().text().unwrap(),
        "third-party-change.exe"
    );
}

#[test]
fn registration_records_paths_and_registry_strings_are_strict() {
    let record = record();
    let encoded = encode(&record).unwrap();
    let mut value: serde_json::Value = serde_json::from_slice(&encoded.bytes).unwrap();
    value["unexpected"] = true.into();
    assert!(decode::<Registration>(encode(&value).unwrap()).is_err());
    for path in [
        r"C:relative.exe",
        r"\\server\share\app.exe",
        r"C:\x\..\target.exe",
        r"C:\x \target.exe",
        r"C:\%TEMP%\target.exe",
    ] {
        assert!(dos_path(Path::new(path)).is_err(), "{path}");
    }
    assert_eq!(
        dos_path(Path::new(r"\\?\C:\x\target.exe")).unwrap(),
        r"C:\x\target.exe"
    );
    for value in [
        Value {
            kind: REG_SZ,
            bytes: vec![0],
        },
        Value {
            kind: REG_SZ,
            bytes: vec![b'a', 0],
        },
        Value {
            kind: REG_SZ,
            bytes: vec![0, 0, 0, 0],
        },
        Value::dword(1),
    ] {
        assert!(value.text().is_err());
    }
    assert!(
        record
            .debugger()
            .unwrap()
            .ends_with(&format!(" --registration {} --", record.id))
    );
    // Platform removal checks elevation before any read or registry write.
    if identity::assert_ordinary_user().is_ok() {
        assert!(remove(record.id).is_err());
    }
}

#[test]
fn ordinary_registry_objects_are_not_adopted_as_protected_records() {
    let fixture = Fixture::new();
    let parent = fixture.parent(&record());
    assert!(security::owned(&parent).is_err());
    assert!(security::system_key(&parent).is_err());
    let mut handle = ptr::null_mut();
    let missing = crate::wide(OsStr::new("Missing")).unwrap();
    assert_ne!(
        // SAFETY: live fixture key, terminated child name and writable handle output.
        unsafe { RegOpenKeyExW(parent.0, missing.as_ptr(), 0, KEY_READ, &mut handle) },
        0
    );
}

#[test]
fn interrupted_install_and_remove_resume_from_durable_checkpoints() {
    for step in 1..=7 {
        let fixture = Fixture::new();
        let roots = fixture.roots();
        let record = record();
        journal::interrupt_after(step);
        assert!(roots.install(&record).is_err(), "install checkpoint {step}");
        let parent = fixture.parent(&record);
        if let Some(filter) = Key::open(parent.0, &record.filter(), false).unwrap()
            && filter.value("Debugger").unwrap().is_some()
        {
            assert_eq!(roots.read_for_removal(record.id).unwrap().id, record.id);
        }
        roots.install(&record).unwrap();
        roots.verify(&record).unwrap();
        roots.remove(&record).unwrap();
        assert!(parent.value("UseFilter").unwrap().is_none());
    }
    for step in 1..=5 {
        let fixture = Fixture::new();
        let roots = fixture.roots();
        let record = record();
        roots.install(&record).unwrap();
        journal::interrupt_after(step);
        assert!(roots.remove(&record).is_err(), "remove checkpoint {step}");
        roots.remove(&record).unwrap();
        roots.remove(&record).unwrap();
        let parent = fixture.parent(&record);
        assert!(parent.children().unwrap().is_empty());
        assert!(parent.value("UseFilter").unwrap().is_none());
    }
}

#[test]
fn shared_unicode_parent_restores_only_after_last_participant() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let mut first = record();
    let mut second = record();
    first.target = r"C:\First\Äpp.exe".into();
    second.target = r"C:\Second\äpp.exe".into();
    second.target_image.file_index += 1;
    roots.install(&first).unwrap();
    roots.install(&second).unwrap();
    roots.remove(&first).unwrap();
    roots.verify(&second).unwrap();
    let parent = fixture.parent(&first);
    assert_eq!(parent.value("UseFilter").unwrap(), Some(Value::dword(1)));
    roots.remove(&second).unwrap();
    assert!(parent.value("UseFilter").unwrap().is_none());
    // Completed history does not restore the previous round's parent setting.
    parent.set("UseFilter", &Value::dword(1)).unwrap();
    first.id = Uuid::new_v4();
    roots.install(&first).unwrap();
    roots.remove(&first).unwrap();
    assert_eq!(parent.value("UseFilter").unwrap(), Some(Value::dword(1)));
}

#[test]
fn interrupted_first_intent_does_not_poison_other_registrations() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let first = record();
    journal::interrupt_before_intent();
    assert!(matches!(
        roots.install(&first),
        Err(Error::Invalid("IFEO_FIXTURE_INTERRUPTION"))
    ));
    assert!(roots.read_for_removal(first.id).is_err());
    assert!(fixture.parent(&first).children().unwrap().is_empty());
    let mut second = record();
    second.target = r"C:\Other\target.exe".into();
    second.target_image.file_index += 1;
    roots.install(&second).unwrap();
    roots.install(&first).unwrap();
    roots.remove(&first).unwrap();
    roots.verify(&second).unwrap();
    roots.remove(&second).unwrap();
    assert!(fixture.parent(&first).value("UseFilter").unwrap().is_none());
}

#[test]
fn unowned_filter_and_missing_parent_are_not_created_or_adopted() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let record = record();
    let parent = fixture.parent(&record);
    let (unknown, _) = Key::create(parent.0, &record.filter(), None).unwrap();
    assert!(matches!(
        roots.install(&record),
        Err(Error::Invalid("IFEO_OWNER_CONFLICT"))
    ));
    assert_eq!(unknown.value_count().unwrap(), 0);
    assert!(parent.value("UseFilter").unwrap().is_none());
    assert!(roots.read_for_removal(record.id).is_err());
    drop(unknown);
    parent.delete_child(&record.filter()).unwrap();
    roots.install(&record).unwrap();
    roots.remove(&record).unwrap();
    drop(parent);
    let root = Key::path(HKEY_CURRENT_USER, &fixture.ifeo, true)
        .unwrap()
        .unwrap();
    root.delete_child(&record.basename().unwrap()).unwrap();
    assert!(roots.remove(&record).is_err());
    assert!(
        Key::open(root.0, &record.basename().unwrap(), false)
            .unwrap()
            .is_none()
    );
}

#[test]
fn incomplete_participants_retain_shared_parent_backup() {
    for removing in [false, true] {
        let fixture = Fixture::new();
        let roots = fixture.roots();
        let first = record();
        let mut second = record();
        second.target = r"C:\Second\target.exe".into();
        second.target_image.file_index += 1;
        if removing {
            roots.install(&first).unwrap();
            journal::interrupt_after(3); // Removing journal, Debugger and filter gone.
            assert!(roots.remove(&first).is_err());
        } else {
            journal::interrupt_after(5); // Installing journal, UseFilter enabled.
            assert!(roots.install(&first).is_err());
        }
        roots.install(&second).unwrap();
        roots.remove(&second).unwrap();
        assert_eq!(
            fixture.parent(&first).value("UseFilter").unwrap(),
            Some(Value::dword(1))
        );
        if !removing {
            roots.install(&first).unwrap();
        }
        roots.remove(&first).unwrap();
        assert!(fixture.parent(&first).value("UseFilter").unwrap().is_none());
    }
}

#[test]
fn record_capacity_rejects_new_install_but_keeps_existing_removal_available() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let first = record();
    roots.install(&first).unwrap();
    let records = Key::path(
        HKEY_CURRENT_USER,
        &format!(r"{}\Ifeo\Registrations", fixture.product),
        true,
    )
    .unwrap()
    .unwrap();
    for _ in 1..512 {
        let mut retired = first.clone();
        retired.id = Uuid::new_v4();
        records
            .set(
                &retired.id.to_string(),
                &encode(&serde_json::json!({
                    "record": retired, "phase": "removed"
                }))
                .unwrap(),
            )
            .unwrap();
    }
    assert_eq!(records.value_names().unwrap().len(), 512);
    assert!(matches!(
        roots.install(&record()),
        Err(Error::Invalid("IFEO_RECORD_LIMIT"))
    ));
    roots.install(&first).unwrap();
    roots.remove(&first).unwrap();
    assert!(fixture.parent(&first).value("UseFilter").unwrap().is_none());
}

#[test]
fn filter_capacity_rejects_activation_before_publishing_intent() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let record = record();
    let parent = fixture.parent(&record);
    parent.set("UseFilter", &Value::dword(1)).unwrap();
    for index in 0..512 {
        let (filter, _) = Key::create(parent.0, &format!("Foreign-{index}"), None).unwrap();
        filter
            .set(
                "FilterFullPath",
                &Value::string(&format!(r"C:\Foreign{index}\target.exe")).unwrap(),
            )
            .unwrap();
    }
    assert!(matches!(
        roots.install(&record),
        Err(Error::Invalid("IFEO_KEY_LIMIT"))
    ));
    assert!(roots.read_for_removal(record.id).is_err());
    assert!(
        Key::open(parent.0, &record.filter(), false)
            .unwrap()
            .is_none()
    );
    assert_eq!(parent.children().unwrap().len(), 512);
    parent.delete_child("Foreign-0").unwrap();
    roots.install(&record).unwrap();
    roots.verify(&record).unwrap();
    roots.remove(&record).unwrap();
    assert_eq!(parent.children().unwrap().len(), 511);
    assert_eq!(parent.value("UseFilter").unwrap(), Some(Value::dword(1)));
}

#[test]
fn foreign_addition_blocks_shared_parent_restore_and_image_alias_conflicts() {
    let fixture = Fixture::new();
    let roots = fixture.roots();
    let first = record();
    let mut alias = record();
    alias.target = r"C:\Alias\other.exe".into();
    roots.install(&first).unwrap();
    assert!(matches!(
        roots.install(&alias),
        Err(Error::Invalid("IFEO_OWNER_CONFLICT"))
    ));
    let parent = fixture.parent(&first);
    let (foreign, _) = Key::create(parent.0, "Foreign", None).unwrap();
    foreign
        .set(
            "FilterFullPath",
            &Value::string(r"C:\Foreign\app.exe").unwrap(),
        )
        .unwrap();
    assert!(matches!(
        roots.remove(&first),
        Err(Error::Invalid("IFEO_FOREIGN_FILTERS_ADDED"))
    ));
    assert_eq!(parent.value("UseFilter").unwrap(), Some(Value::dword(1)));
    assert!(foreign.value("FilterFullPath").unwrap().is_some());
    // A later explicit retry can complete once that foreign participant is gone.
    parent.delete_child("Foreign").unwrap();
    roots.remove(&first).unwrap();
}

#[test]
fn link_keys_are_rejected_without_following_their_destination() {
    let fixture = Fixture::new();
    let root = Key::path(HKEY_CURRENT_USER, &fixture.path, true)
        .unwrap()
        .unwrap();
    let (target, _) = Key::create(root.0, "Target", None).unwrap();
    target
        .set("Keep", &Value::string("untouched").unwrap())
        .unwrap();
    let name = crate::wide(OsStr::new("Link")).unwrap();
    let mut link = ptr::null_mut();
    // SAFETY: the live parent is this test's private HKCU subtree, the name is
    // terminated and link points to writable output storage.
    let code = unsafe {
        RegCreateKeyExW(
            root.0,
            name.as_ptr(),
            0,
            ptr::null(),
            REG_OPTION_CREATE_LINK,
            KEY_ALL_ACCESS,
            ptr::null(),
            &mut link,
            ptr::null_mut(),
        )
    };
    assert_eq!(code, 0);
    let link = Key(link);
    let path = format!(
        r"\Registry\User\{}\{}\Target",
        identity::current().unwrap().user_sid,
        fixture.path
    );
    link.set(
        "SymbolicLinkValue",
        &Value {
            kind: REG_LINK,
            bytes: path.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        },
    )
    .unwrap();
    assert!(matches!(
        Key::open(root.0, "Link", false),
        Err(Error::Invalid("IFEO_REGISTRY_LINK"))
    ));
    assert!(matches!(
        Key::create(root.0, "Link", None),
        Err(Error::Invalid("IFEO_REGISTRY_LINK"))
    ));
    assert!(Key::path(root.0, r"Link\Nested", true).is_err());
    assert_eq!(
        target.value("Keep").unwrap().unwrap().text().unwrap(),
        "untouched"
    );
    // Remove the link itself before subtree cleanup; never follow its target.
    root.delete_child("Link").unwrap();
}

#[test]
fn actual_machine_ifeo_root_is_read_only_and_protected_and_records_are_bounded() {
    let software = Key::open(HKEY_LOCAL_MACHINE, "SOFTWARE", false)
        .unwrap()
        .unwrap();
    security::system_key(&software).unwrap();
    let root = Key::path(HKEY_LOCAL_MACHINE, IFEO, false).unwrap().unwrap();
    security::system_key(&root).unwrap();
    let mut oversized = record();
    oversized.owner_sid = "x".repeat(65536);
    assert!(matches!(
        encode(&oversized),
        Err(Error::Invalid("IFEO_RECORD_SIZE"))
    ));
    assert!(security::Mutation::acquire().is_err());
}
