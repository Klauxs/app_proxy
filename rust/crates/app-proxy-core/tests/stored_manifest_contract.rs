//! Guards the stored shape of the manifest across releases.
//!
//! `fixtures/manifests/vN.json` is a manifest exactly as schema version N wrote
//! it, using every variant and optional field of that version. These files are
//! never edited after the version ships. When the stored shape changes: raise
//! `SCHEMA_VERSION`, append a migration in `model/stored.rs`, and add the new
//! `vN.json` here. The tests below fail until all three are done.
use app_proxy_core::model::{self, SCHEMA_VERSION};
use std::path::PathBuf;

fn fixture(version: u32) -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/manifests")
        .join(format!("v{version}.json"));
    std::fs::read(&path).unwrap_or_else(|_| panic!("missing fixture {}", path.display()))
}

#[test]
fn every_released_schema_version_still_loads_and_validates() {
    for version in 1..=SCHEMA_VERSION {
        let loaded = model::load(&fixture(version)).unwrap_or_else(|e| panic!("v{version}: {e}"));
        assert_eq!(loaded.stored_version, version);
        assert_eq!(loaded.manifest.schema_version, SCHEMA_VERSION);
        loaded
            .manifest
            .validate()
            .unwrap_or_else(|e| panic!("v{version}: {e}"));
    }
}

/// Writing the current fixture back must reproduce it exactly. A difference
/// means the stored shape changed (a field was added, renamed or re-encoded)
/// without a new schema version.
#[test]
fn current_fixture_round_trips_without_any_change_of_shape() {
    let bytes = fixture(SCHEMA_VERSION);
    let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let written = serde_json::to_value(model::load(&bytes).unwrap().manifest).unwrap();
    assert_eq!(
        written, original,
        "stored manifest shape changed: raise SCHEMA_VERSION, add a migration and a new fixture"
    );
}

#[test]
fn a_manifest_from_the_next_version_is_refused_by_name() {
    let mut value: serde_json::Value = serde_json::from_slice(&fixture(SCHEMA_VERSION)).unwrap();
    value["schema_version"] = (SCHEMA_VERSION + 1).into();
    let error = model::load(&serde_json::to_vec(&value).unwrap()).map(|_| ());
    assert_eq!(error.unwrap_err().0, "STORE_SCHEMA_NEWER");
}
