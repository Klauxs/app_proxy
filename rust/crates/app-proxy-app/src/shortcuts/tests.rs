use super::*;
use app_proxy_core::{
    model::*,
    registry::{ConfigAction, ConfigRequest},
};
use app_proxy_windows::{shortcuts::Receipt, store::Store};
use sha2::{Digest, Sha256};
use std::{fs, os::windows::fs::OpenOptionsExt};

struct Fixture {
    temp: tempfile::TempDir,
    root: PathBuf,
    configuration: Configuration,
    request: Request,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let mut manifest = store.load().unwrap();
        let app = Uuid::new_v4();
        let id = Uuid::new_v4();
        let host = temp.path().join("app-proxy-host.exe");
        fs::write(&host, b"fixture only, never launched").unwrap();
        fs::create_dir(temp.path().join("desktop")).unwrap();
        manifest.applications.push(Application {
            id: app,
            name: "app".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path: host },
            template_ref: Template::Codex,
        });
        manifest.instances.push(Instance {
            id,
            application_id: app,
            name: "分身 : fixture".into(),
            revision: 1,
            data: InstanceData::Original {},
            args: vec!["private-fixture-arg".into()],
            env: SavedEnvironment::default(),
            cwd: WorkingDirectory::Application {},
            network: NetworkBinding::Direct {},
            guard: GuardConfig {
                desired: Desired::Disabled,
                policy: GuardPolicy::StopUnproxied,
            },
        });
        store.commit(1, manifest).unwrap();
        Self {
            temp,
            root,
            configuration: Configuration::new(store),
            request: Request {
                id: Uuid::new_v4(),
                instance_id: id,
                expected_revision: 2,
                action: Action::Create,
                expected_creation: None,
            },
        }
    }
    fn assets(&self) -> Assets {
        // COM stores a path only. Real resource decode is covered by native icon
        // tests; this fixture never asks Explorer to render the display bytes.
        Assets {
            desktop: self.temp.path().join("desktop"),
            host: self.temp.path().join("app-proxy-host.exe"),
            icon: b"fixture icon payload".to_vec(),
        }
    }
    fn create(&self) -> Result<Status> {
        apply_with(&self.configuration, &self.root, &self.request, |_| {
            Ok(self.assets())
        })
    }
    fn remove(&self) -> Request {
        Request {
            id: Uuid::new_v4(),
            expected_revision: self.configuration.snapshot().unwrap().revision,
            action: Action::Remove,
            expected_creation: Some(self.request.id),
            ..self.request.clone()
        }
    }
}

#[test]
fn service_derives_fixed_fields_and_replays_without_resolving_removed_application() {
    let fixture = Fixture::new();
    let Status::Created { path, .. } = fixture.create().unwrap() else {
        panic!("create missing")
    };
    assert_eq!(
        path,
        fixture
            .assets()
            .desktop
            .join(native::filename("分身 : fixture", fixture.request.instance_id).unwrap())
    );
    let manifest = fixture.configuration.snapshot().unwrap();
    let icon = fixture.root.join("state").join(format!(
        "icon-{:x}.ico",
        Sha256::digest(fixture.assets().icon)
    ));
    let spec = Spec {
        store_id: manifest.store_id,
        instance_id: fixture.request.instance_id,
        home: fixture.root.clone(),
        host: fixture.assets().host.clone(),
        icon,
    };
    let receipt = Receipt {
        file: app_proxy_windows::identity::file_identity(&path).unwrap(),
        sha256: Sha256::digest(fs::read(&path).unwrap()).into(),
    };
    native::verify(&path, &spec, &receipt).unwrap();
    assert_eq!(
        manifest.integrations.shortcuts[0].args,
        [
            "launch".into(),
            fixture.request.instance_id.to_string(),
            "--home".into(),
            fixture.root.to_string_lossy().into_owned(),
            "--notify".into()
        ]
    );
    fs::remove_file(fixture.assets().host).unwrap();
    let no_prepare = |_: &ApplicationLocator| -> Result<Assets> {
        panic!("historical replay/removal cannot query installation")
    };
    apply_with(
        &fixture.configuration,
        &fixture.root,
        &fixture.request,
        no_prepare,
    )
    .unwrap();
    let changed = Request {
        expected_revision: 3,
        ..fixture.request.clone()
    };
    assert!(matches!(
        apply_with(&fixture.configuration, &fixture.root, &changed, no_prepare),
        Err(Error::Invalid("REQUEST_ID_CONFLICT"))
    ));
    let remove = fixture.remove();
    apply_with(&fixture.configuration, &fixture.root, &remove, no_prepare).unwrap();
    assert!(!path.exists());
    assert!(
        status(&fixture.configuration, fixture.request.instance_id)
            .unwrap()
            .integration
            .is_none()
    );
    apply_with(
        &fixture.configuration,
        &fixture.root,
        &fixture.request,
        no_prepare,
    )
    .unwrap();
    assert!(!path.exists());
}

#[test]
fn preparation_releases_gate_and_final_revision_check_precedes_icon_writes() {
    let fixture = Fixture::new();
    let invalid = Request {
        expected_creation: Some(Uuid::new_v4()),
        ..fixture.request.clone()
    };
    assert!(matches!(
        apply_with(&fixture.configuration, &fixture.root, &invalid, |_| panic!(
            "invalid payload before resource queries"
        )),
        Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"))
    ));
    let result = apply_with(
        &fixture.configuration,
        &fixture.root,
        &fixture.request,
        |_| {
            fixture.configuration.apply(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 2,
                action: ConfigAction::RenameInstance {
                    instance_id: fixture.request.instance_id,
                    name: "changed during preparation".into(),
                },
            })?;
            Ok(fixture.assets())
        },
    );
    assert!(matches!(
        result,
        Err(Error::Invalid("STALE_MANIFEST_REVISION"))
    ));
    assert_eq!(fs::read_dir(fixture.assets().desktop).unwrap().count(), 0);
    assert!(!fs::read_dir(fixture.root.join("state")).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with("icon-")
    }));
    assert!(
        fixture
            .configuration
            .lock()
            .unwrap()
            .shortcut_request_status(fixture.request.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn simultaneous_same_payload_replays_before_stale_revision_check() {
    let fixture = Fixture::new();
    let result = apply_with(
        &fixture.configuration,
        &fixture.root,
        &fixture.request,
        |_| {
            fixture.create()?;
            Ok(fixture.assets())
        },
    )
    .unwrap();
    assert!(matches!(result, Status::Created { revision: 3, .. }));
    assert_eq!(
        fixture
            .configuration
            .snapshot()
            .unwrap()
            .integrations
            .shortcuts
            .len(),
        1
    );
    assert_eq!(fs::read_dir(fixture.assets().desktop).unwrap().count(), 1);
}

#[test]
fn instance_query_surfaces_pending_removal_and_queries_never_resume_side_effects() {
    let fixture = Fixture::new();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(1)
        .open(fixture.root.join("manifest.json"))
        .unwrap();
    assert!(fixture.create().is_err());
    let view = status(&fixture.configuration, fixture.request.instance_id).unwrap();
    let entry = view.integration.unwrap();
    assert_eq!(entry.request.id, fixture.request.id);
    assert!(matches!(
        entry.status,
        Status::Pending {
            action: Action::Create,
            ..
        }
    ));
    drop(held);
    // Query must not finish the manifest just because it now can.
    assert_eq!(
        status(&fixture.configuration, fixture.request.instance_id)
            .unwrap()
            .revision,
        2
    );
    fixture
        .configuration
        .lock()
        .unwrap()
        .resume_shortcut(fixture.request.id)
        .unwrap();
    let path = fixture
        .configuration
        .snapshot()
        .unwrap()
        .integrations
        .shortcuts[0]
        .path
        .clone();
    let original = fs::read(&path).unwrap();
    fs::write(&path, b"user modification").unwrap();
    let remove = fixture.remove();
    assert!(
        apply_with(&fixture.configuration, &fixture.root, &remove, |_| panic!(
            "no queries"
        ))
        .is_err()
    );
    let entry = status(&fixture.configuration, fixture.request.instance_id)
        .unwrap()
        .integration
        .unwrap();
    assert_eq!(entry.request.id, remove.id);
    assert!(matches!(
        entry.status,
        Status::Pending {
            action: Action::Remove,
            ..
        }
    ));
    assert_eq!(fs::read(&path).unwrap(), b"user modification");
    fs::write(&path, original).unwrap();
    fixture
        .configuration
        .lock()
        .unwrap()
        .resume_shortcut(remove.id)
        .unwrap();
    assert!(!path.exists());
}
