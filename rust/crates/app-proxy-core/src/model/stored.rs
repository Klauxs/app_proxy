//! Reading a stored manifest of any supported schema version.
//!
//! Long-lived configuration must survive upgrades, so the stored version is read
//! before the strict model is applied. A newer file is refused with its own
//! code and left untouched; an older file passes through one migration per
//! version. Any change to the stored shape of [`Manifest`] raises
//! [`SCHEMA_VERSION`] and appends a migration, even when the change is additive:
//! an older program must report `STORE_SCHEMA_NEWER`, not a parse failure.
use super::{FORMAT, Manifest, SCHEMA_VERSION, ValidationError};
use serde_json::Value;

type Result<T> = std::result::Result<T, ValidationError>;
type Migration = fn(Value) -> Result<Value>;

/// `MIGRATIONS[n]` upgrades schema version `n + 1` to `n + 2`. The loader stamps
/// the new version after each step.
const MIGRATIONS: &[Migration] = &[];
const _: () = assert!(MIGRATIONS.len() as u32 + 1 == SCHEMA_VERSION);

pub struct Loaded {
    pub manifest: Manifest,
    /// Version found in the bytes. When it is below [`SCHEMA_VERSION`] the caller
    /// keeps the original bytes before it first writes the migrated manifest.
    pub stored_version: u32,
}

/// Decodes without validating references; callers run [`Manifest::validate`].
pub fn load(bytes: &[u8]) -> Result<Loaded> {
    let (value, stored_version) = upgrade(bytes, MIGRATIONS)?;
    // A current file is parsed from its bytes so that the strict reader still
    // rejects duplicate keys; only an older file continues from the migrated value.
    let manifest = if stored_version == SCHEMA_VERSION {
        serde_json::from_slice(bytes)
    } else {
        serde_json::from_value(value)
    }
    .map_err(|error| {
        // serde errors can contain the offending value, so only expose stable codes.
        if error
            .to_string()
            .starts_with("LEGACY_IFEO_CLEANUP_REQUIRED")
        {
            ValidationError("LEGACY_IFEO_CLEANUP_REQUIRED")
        } else {
            ValidationError("INVALID_STORE_JSON")
        }
    })?;
    Ok(Loaded {
        manifest,
        stored_version,
    })
}

fn upgrade(bytes: &[u8], migrations: &[Migration]) -> Result<(Value, u32)> {
    let current = migrations.len() as u32 + 1;
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| ValidationError("INVALID_STORE_JSON"))?;
    let header = value
        .as_object()
        .ok_or(ValidationError("INVALID_STORE_JSON"))?;
    if header.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(ValidationError("UNSUPPORTED_FORMAT"));
    }
    let stored = header
        .get("schema_version")
        .and_then(Value::as_u64)
        .and_then(|version| u32::try_from(version).ok())
        .ok_or(ValidationError("UNSUPPORTED_FORMAT"))?;
    if stored == 0 {
        return Err(ValidationError("UNSUPPORTED_FORMAT"));
    }
    if stored > current {
        return Err(ValidationError("STORE_SCHEMA_NEWER"));
    }
    for (index, migrate) in migrations.iter().enumerate().skip(stored as usize - 1) {
        value = migrate(value)?;
        value
            .as_object_mut()
            .ok_or(ValidationError("INVALID_STORE_JSON"))?
            .insert("schema_version".into(), (index as u32 + 2).into());
    }
    Ok((value, stored))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(version: u64, extra: &str) -> Vec<u8> {
        format!(r#"{{"format":"app-proxy-rust","schema_version":{version}{extra}}}"#).into_bytes()
    }

    fn rename_a_to_b(mut value: Value) -> Result<Value> {
        let object = value.as_object_mut().unwrap();
        let moved = object
            .remove("a")
            .ok_or(ValidationError("INVALID_STORE_JSON"))?;
        object.insert("b".into(), moved);
        Ok(value)
    }

    fn add_c(mut value: Value) -> Result<Value> {
        value
            .as_object_mut()
            .unwrap()
            .insert("c".into(), true.into());
        Ok(value)
    }

    #[test]
    fn older_versions_pass_through_every_later_migration_in_order() {
        let chain: &[Migration] = &[rename_a_to_b, add_c];
        let (value, from) = upgrade(&stored(1, r#","a":7"#), chain).unwrap();
        assert_eq!(from, 1);
        assert_eq!(
            value,
            serde_json::json!({"format": FORMAT, "schema_version": 3, "b": 7, "c": true})
        );
        // Version 2 already has `b`; only the second migration applies.
        let (value, from) = upgrade(&stored(2, r#","b":7"#), chain).unwrap();
        assert_eq!(from, 2);
        assert_eq!(
            value,
            serde_json::json!({"format": FORMAT, "schema_version": 3, "b": 7, "c": true})
        );
        let (value, from) = upgrade(&stored(3, r#","b":7,"c":false"#), chain).unwrap();
        assert_eq!((from, value["c"].as_bool()), (3, Some(false)));
    }

    #[test]
    fn newer_foreign_and_damaged_headers_have_distinct_stable_codes() {
        let code = |bytes: &[u8]| upgrade(bytes, &[]).map(|_| ()).unwrap_err().0;
        assert_eq!(code(&stored(2, "")), "STORE_SCHEMA_NEWER");
        assert_eq!(code(&stored(0, "")), "UNSUPPORTED_FORMAT");
        assert_eq!(
            code(br#"{"format":"other","schema_version":1}"#),
            "UNSUPPORTED_FORMAT"
        );
        assert_eq!(
            code(br#"{"format":"app-proxy-rust","schema_version":"1"}"#),
            "UNSUPPORTED_FORMAT"
        );
        assert_eq!(code(br#"{"format":"app-proxy-rust""#), "INVALID_STORE_JSON");
        assert_eq!(code(b"[]"), "INVALID_STORE_JSON");
    }

    #[test]
    fn a_failed_migration_stops_the_chain() {
        let chain: &[Migration] = &[rename_a_to_b, add_c];
        let error = upgrade(&stored(1, ""), chain).map(|_| ()).unwrap_err();
        assert_eq!(error.0, "INVALID_STORE_JSON");
    }

    #[test]
    fn current_manifests_load_unchanged_and_report_their_version() {
        let manifest = Manifest::empty("S-1-5-21-1".into());
        let bytes = serde_json::to_vec(&manifest).unwrap();
        let loaded = load(&bytes).unwrap();
        assert_eq!(loaded.stored_version, SCHEMA_VERSION);
        assert_eq!(serde_json::to_vec(&loaded.manifest).unwrap(), bytes);
        loaded.manifest.validate().unwrap();
    }

    #[test]
    fn unknown_fields_and_the_retired_ifeo_slot_keep_their_codes() {
        let mut value = serde_json::to_value(Manifest::empty("S-1-5-21-1".into())).unwrap();
        value["surprise"] = 1.into();
        let error = load(&serde_json::to_vec(&value).unwrap()).map(|_| ());
        assert_eq!(error.unwrap_err().0, "INVALID_STORE_JSON");

        let mut value = serde_json::to_value(Manifest::empty("S-1-5-21-1".into())).unwrap();
        value["integrations"]["ifeo"] = serde_json::json!([{"id": 1}]);
        let error = load(&serde_json::to_vec(&value).unwrap()).map(|_| ());
        assert_eq!(error.unwrap_err().0, "LEGACY_IFEO_CLEANUP_REQUIRED");

        let duplicate = serde_json::to_string(&Manifest::empty("S-1-5-21-1".into()))
            .unwrap()
            .replacen("\"revision\":1", "\"revision\":1,\"revision\":2", 1);
        let error = load(duplicate.as_bytes()).map(|_| ());
        assert_eq!(error.unwrap_err().0, "INVALID_STORE_JSON");
    }
}
