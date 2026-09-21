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

/// The two fields read before anything else. Other fields are skipped without
/// being built, and a repeated `format` or `schema_version` is an error rather
/// than "last one wins", so the version that selects the reader is unambiguous.
#[derive(serde::Deserialize)]
struct Header {
    format: String,
    schema_version: u64,
}

fn stored_version(bytes: &[u8], current: u32) -> Result<u32> {
    // Missing, mistyped or repeated header fields are damage, as they were for
    // the strict reader; only a well-formed header can name another format.
    let header: Header =
        serde_json::from_slice(bytes).map_err(|_| ValidationError("INVALID_STORE_JSON"))?;
    if header.format != FORMAT || header.schema_version == 0 {
        return Err(ValidationError("UNSUPPORTED_FORMAT"));
    }
    if header.schema_version > u64::from(current) {
        return Err(ValidationError("STORE_SCHEMA_NEWER"));
    }
    Ok(header.schema_version as u32)
}

/// Decodes without validating references; callers run [`Manifest::validate`].
pub fn load(bytes: &[u8]) -> Result<Loaded> {
    let stored_version = stored_version(bytes, SCHEMA_VERSION)?;
    // A current file goes straight to the strict reader, which also rejects
    // duplicate keys at every depth. Only an older file is migrated as a value.
    let manifest = if stored_version == SCHEMA_VERSION {
        serde_json::from_slice(bytes)
    } else {
        serde_json::from_value(upgrade(bytes, stored_version, MIGRATIONS)?)
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

/// Applies every migration from `stored` up to the current version. Below the
/// header a generic value keeps the last of any repeated key; a migration that
/// cares must check for itself.
fn upgrade(bytes: &[u8], stored: u32, migrations: &[Migration]) -> Result<Value> {
    let mut value: Value =
        serde_json::from_slice(bytes).map_err(|_| ValidationError("INVALID_STORE_JSON"))?;
    for (index, migrate) in migrations.iter().enumerate().skip(stored as usize - 1) {
        value = migrate(value)?;
        value
            .as_object_mut()
            .ok_or(ValidationError("INVALID_STORE_JSON"))?
            .insert("schema_version".into(), (index as u32 + 2).into());
    }
    Ok(value)
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
        let run = |bytes: &[u8]| {
            let from = stored_version(bytes, 3).unwrap();
            (upgrade(bytes, from, chain).unwrap(), from)
        };
        let (value, from) = run(&stored(1, r#","a":7"#));
        assert_eq!(from, 1);
        assert_eq!(
            value,
            serde_json::json!({"format": FORMAT, "schema_version": 3, "b": 7, "c": true})
        );
        // Version 2 already has `b`; only the second migration applies.
        let (value, from) = run(&stored(2, r#","b":7"#));
        assert_eq!(from, 2);
        assert_eq!(
            value,
            serde_json::json!({"format": FORMAT, "schema_version": 3, "b": 7, "c": true})
        );
        let (value, from) = run(&stored(3, r#","b":7,"c":false"#));
        assert_eq!((from, value["c"].as_bool()), (3, Some(false)));
    }

    #[test]
    fn newer_foreign_and_damaged_headers_have_distinct_stable_codes() {
        let code = |bytes: &[u8]| stored_version(bytes, 1).unwrap_err().0;
        assert_eq!(code(&stored(2, "")), "STORE_SCHEMA_NEWER");
        assert_eq!(
            code(br#"{"format":"app-proxy-rust","schema_version":99999999999}"#),
            "STORE_SCHEMA_NEWER"
        );
        // A repeated version must not let the later value pick the reader.
        assert_eq!(
            stored_version(
                br#"{"format":"app-proxy-rust","schema_version":2,"schema_version":1}"#,
                2
            )
            .unwrap_err()
            .0,
            "INVALID_STORE_JSON"
        );
        assert_eq!(code(&stored(0, "")), "UNSUPPORTED_FORMAT");
        assert_eq!(
            code(br#"{"format":"other","schema_version":1}"#),
            "UNSUPPORTED_FORMAT"
        );
        assert_eq!(
            code(br#"{"format":"app-proxy-rust","schema_version":"1"}"#),
            "INVALID_STORE_JSON"
        );
        assert_eq!(code(br#"{"format":"app-proxy-rust""#), "INVALID_STORE_JSON");
        assert_eq!(code(b"[]"), "INVALID_STORE_JSON");
    }

    #[test]
    fn a_failed_migration_stops_the_chain() {
        let chain: &[Migration] = &[rename_a_to_b, add_c];
        let error = upgrade(&stored(1, ""), 1, chain).map(|_| ()).unwrap_err();
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
