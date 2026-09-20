use super::*;
use app_proxy_core::model::LoginTask;
use app_proxy_windows::{guard_task::login::journal::Action, store::Store};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::fs;

struct Fixture {
    configuration: Configuration,
    request: Request,
    record: serde_json::Value,
    root: std::path::PathBuf,
    _temp: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let mut manifest = store.load().unwrap();
        let host = temp.path().join("missing/app-proxy-host.exe");
        let scope = format!("{:x}", Sha256::digest(manifest.owner_sid.as_bytes()));
        manifest.integrations.guard_login_task = Some(LoginTask {
            name: format!("AppProxyRust-Login-{}-{}", &scope[..16], manifest.store_id),
            target: host.clone(),
            args: vec![
                "serve".into(),
                "--home".into(),
                root.to_str().unwrap().into(),
                "--expected-store".into(),
                manifest.store_id.to_string(),
            ],
        });
        let request = Request {
            id: Uuid::new_v4(),
            expected_revision: 1,
            action: Action::Create,
            expected_creation: None,
        };
        let record = json!({"version":1, "store_id":manifest.store_id, "entries":[{
            "create":request, "registration":{"store_id":manifest.store_id,
                "owner_sid":manifest.owner_sid,"home":root,"host":host},
            "created_revision":2,"removal":null,"removed_revision":null,"removed_at":null
        }]});
        store.commit(1, manifest).unwrap();
        fs::write(
            root.join("state/login-task.json"),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        Self {
            configuration: Configuration::new(store),
            request,
            record,
            root,
            _temp: temp,
        }
    }
    fn write(&self, record: &serde_json::Value) {
        fs::write(
            self.root.join("state/login-task.json"),
            serde_json::to_vec(record).unwrap(),
        )
        .unwrap();
    }
}

#[test]
fn login_service_terminal_replay_needs_no_installed_release_and_checks_full_payload() {
    let fixture = Fixture::new();
    assert!(matches!(
        apply(&fixture.configuration, &fixture.root, &fixture.request).unwrap(),
        Status::Created { revision: 2 }
    ));
    assert!(matches!(
        resume(&fixture.configuration, fixture.request.id).unwrap(),
        Status::Created { revision: 2 }
    ));
    let mut conflict = fixture.request.clone();
    conflict.expected_revision = 2;
    assert!(matches!(
        apply(&fixture.configuration, &fixture.root, &conflict),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    assert_eq!(fixture.configuration.snapshot().unwrap().revision, 2);
}

#[test]
fn login_readiness_requires_current_native_evidence_metadata_and_completed_receipt() {
    let fixture = Fixture::new();
    assert!(
        status_with(&fixture.configuration, |_| Ok(true))
            .unwrap()
            .ready
    );
    let missing = status_with(&fixture.configuration, |_| Ok(false)).unwrap();
    assert!(!missing.ready);
    assert_eq!(
        missing.diagnostic.as_deref(),
        Some("GUARD_LOGIN_TASK_MISSING")
    );
    let conflict = status_with(&fixture.configuration, |_| {
        Err(Error::Invalid("FIXTURE_CONFLICT"))
    })
    .unwrap();
    assert!(!conflict.ready);
    assert_eq!(conflict.diagnostic.as_deref(), Some("FIXTURE_CONFLICT"));
    let mut record = fixture.record.clone();
    record["entries"][0]["created_revision"] = serde_json::Value::Null;
    fixture.write(&record);
    assert!(
        !status_with(&fixture.configuration, |_| Ok(true))
            .unwrap()
            .ready
    );
    // The real query may observe a missing UUID fixture task but never creates it.
    assert!(!status(&fixture.configuration, &fixture.root).unwrap().ready);
    assert_eq!(fixture.configuration.snapshot().unwrap().revision, 2);
    assert_eq!(
        fs::read(fixture.root.join("state/login-task.json")).unwrap(),
        serde_json::to_vec(&record).unwrap()
    );
}

#[test]
fn login_status_detects_journal_race_without_holding_configuration_during_native_read() {
    let fixture = Fixture::new();
    let mut changed = fixture.record.clone();
    changed["entries"][0]["removal"] = json!({"id":Uuid::new_v4(),"expected_revision":2,
        "action":"remove","expected_creation":fixture.request.id});
    assert!(matches!(
        status_with(&fixture.configuration, |_| {
            assert_eq!(fixture.configuration.snapshot()?.revision, 2);
            fixture.write(&changed);
            Ok(true)
        }),
        Err(Error::Invalid("GUARD_LOGIN_STATE_CHANGED"))
    ));
}

#[test]
fn login_status_preserves_foreign_metadata_and_preflight_precedes_deployment_access() {
    let fixture = Fixture::new();
    let mut model = fixture.configuration.snapshot().unwrap();
    model.integrations.guard_login_task.as_mut().unwrap().name = "foreign".into();
    fixture
        .configuration
        .lock()
        .unwrap()
        .commit(2, model)
        .unwrap();
    let view = status_with(&fixture.configuration, |_| {
        panic!("do not verify conflicting metadata")
    })
    .unwrap();
    assert!(!view.ready);
    assert_eq!(
        view.diagnostic.as_deref(),
        Some("GUARD_LOGIN_METADATA_CONFLICT")
    );
    let request = Request {
        id: Uuid::new_v4(),
        expected_revision: 2,
        ..fixture.request.clone()
    };
    assert!(matches!(
        apply(&fixture.configuration, &fixture.root, &request),
        Err(Error::Invalid("STALE_MANIFEST_REVISION"))
    ));
    assert_eq!(
        fixture
            .configuration
            .snapshot()
            .unwrap()
            .integrations
            .guard_login_task
            .unwrap()
            .name,
        "foreign"
    );
}
