//! Blocking configuration service. Only short store work holds the commit gate;
//! read-only installation/package queries run outside it. No launch side effects.
use app_proxy_core::{
    model::{self, Manifest},
    registry::{self, ConfigAction, ConfigRequest},
};
use app_proxy_windows::{
    Error, Result,
    config_transaction::{ConfigOutcome, ConfigRequestStatus},
    installation::{self, ResolvedApplication},
    store::Store,
};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, MutexGuard};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceSummary {
    pub id: Uuid,
    pub application_id: Uuid,
    pub name: String,
    pub revision: u64,
    pub isolated: bool,
    pub network: model::NetworkBinding,
    pub guard: model::Desired,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProfileSummary {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub endpoint: model::Endpoint,
    pub protocol: ProfileProtocol,
    pub host: String,
    pub port: u16,
    pub authenticated: bool,
    pub auto_test_nodes: usize,
}

impl ProfileSummary {
    pub fn upstream_label(&self) -> String {
        if self.auto_test_nodes > 1 {
            format!("自动测速 · {} 个候选节点", self.auto_test_nodes)
        } else {
            let host: String = self
                .host
                .chars()
                .map(|c| if c.is_control() { ' ' } else { c })
                .collect();
            format!("{} {}:{}", self.protocol.label(), host, self.port)
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProfileProtocol {
    Manual(model::ManualProtocol),
    Subscription(app_proxy_core::subscription::saved::ProtocolKind),
}
impl ProfileProtocol {
    pub fn label(&self) -> &'static str {
        use app_proxy_core::subscription::saved::ProtocolKind;
        match self {
            Self::Manual(model::ManualProtocol::Http) => "HTTP",
            Self::Manual(model::ManualProtocol::Socks5) => "SOCKS5",
            Self::Subscription(ProtocolKind::AnyTls) => "AnyTLS",
            Self::Subscription(ProtocolKind::Vless) => "VLESS",
            Self::Subscription(ProtocolKind::Vmess) => "VMess",
            Self::Subscription(ProtocolKind::Shadowsocks) => "Shadowsocks",
            Self::Subscription(ProtocolKind::Trojan) => "Trojan",
            Self::Subscription(ProtocolKind::Hysteria2) => "Hysteria2",
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogPage {
    pub revision: u64,
    pub next_offset: Option<usize>,
    pub applications: Vec<model::Application>,
    pub instances: Vec<InstanceSummary>,
    pub profiles: Vec<ProfileSummary>,
}

/// Only display metadata, never argv, environment, credentials or subscription URLs.
pub fn catalog_page(
    manifest: Manifest,
    offset: usize,
    expected_revision: Option<u64>,
) -> Result<CatalogPage> {
    const PAGE: usize = 16;
    if expected_revision.is_some_and(|r| r != manifest.revision) {
        return Err(Error::Invalid("CATALOG_CHANGED"));
    }
    offset
        .checked_add(PAGE)
        .ok_or(Error::Invalid("INVALID_CATALOG_OFFSET"))?;
    let total = manifest
        .applications
        .len()
        .max(manifest.instances.len())
        .max(manifest.profiles.len());
    let display = |name: String| name.chars().take(256).collect();
    let mut page = CatalogPage {
        revision: manifest.revision,
        next_offset: None,
        applications: manifest
            .applications
            .into_iter()
            .skip(offset)
            .take(PAGE)
            .map(|mut a| {
                a.name = display(a.name);
                a
            })
            .collect(),
        instances: manifest
            .instances
            .into_iter()
            .skip(offset)
            .take(PAGE)
            .map(|i| InstanceSummary {
                id: i.id,
                application_id: i.application_id,
                name: display(i.name),
                revision: i.revision,
                isolated: matches!(i.data, model::InstanceData::Isolated { .. }),
                network: i.network,
                guard: i.guard.desired,
            })
            .collect(),
        profiles: manifest
            .profiles
            .into_iter()
            .skip(offset)
            .take(PAGE)
            .map(|p| {
                let count = p.selected_node_ids().len();
                let (protocol, host, port, authenticated) = match p.source {
                    model::ProxySource::Manual { nodes } => {
                        let node = nodes
                            .into_iter()
                            .find(|n| n.id == p.selected_node_id)
                            .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?;
                        (
                            ProfileProtocol::Manual(node.protocol),
                            node.host,
                            node.port,
                            node.credentials.is_some(),
                        )
                    }
                    model::ProxySource::Subscription { nodes, .. } => {
                        let node = nodes
                            .into_iter()
                            .find(|n| n.id == p.selected_node_id)
                            .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?;
                        (
                            ProfileProtocol::Subscription(node.protocol),
                            node.server,
                            node.port,
                            true,
                        )
                    }
                };
                Ok(ProfileSummary {
                    id: p.id,
                    name: display(p.name),
                    revision: p.revision,
                    endpoint: p.endpoint,
                    protocol,
                    host,
                    port,
                    authenticated,
                    auto_test_nodes: if count > 1 { count } else { 0 },
                })
            })
            .collect::<Result<Vec<_>>>()?,
    };
    // Count limits alone do not bound UTF-8 locator sizes. Keep ample room for
    // the RPC envelope, preserve complete locators, and fail explicitly if even
    // one entry cannot fit. All three collections advance by the same count.
    let mut count = PAGE;
    loop {
        let end = offset + count;
        page.next_offset = (end < total).then_some(end);
        if serde_json::to_vec(&page)?.len() <= 512 * 1024 {
            return Ok(page);
        }
        if count == 1 {
            return Err(Error::Invalid("CATALOG_ENTRY_TOO_LARGE"));
        }
        count /= 2;
        page.applications.truncate(count);
        page.instances.truncate(count);
        page.profiles.truncate(count);
    }
}

pub struct Configuration {
    store: Mutex<Store>,
}

impl Configuration {
    pub fn new(store: Store) -> Self {
        Self {
            store: Mutex::new(store),
        }
    }
    pub(crate) fn lock(&self) -> Result<MutexGuard<'_, Store>> {
        self.store
            .lock()
            .map_err(|_| Error::Invalid("CONFIGURATION_OWNER_FAILED"))
    }
    pub fn snapshot(&self) -> Result<Manifest> {
        self.lock()?.load()
    }
    pub fn request_status(&self, id: Uuid) -> Result<Option<ConfigRequestStatus>> {
        let mut store = self.lock()?;
        // A transient replace failure must not strand an accepted intent until
        // owner restart. Querying resumes only its already authorized pure edit.
        store.recover_config_requests()?;
        store.config_request_status(id)
    }
    pub fn apply(&self, request: &ConfigRequest) -> Result<ConfigOutcome> {
        self.apply_with(request, installation::resolve)
    }
    fn apply_with(
        &self,
        request: &ConfigRequest,
        resolve: impl Fn(&model::ApplicationLocator) -> Result<ResolvedApplication>,
    ) -> Result<ConfigOutcome> {
        let snapshot = {
            let mut store = self.lock()?;
            if let Some(outcome) = store.replay_config(request)? {
                return Ok(outcome);
            }
            store.load()?
        };
        // Validate the proposed edit before expensive OS queries. The store
        // validates it again against the latest revision when committing.
        let shared_package_files = self.lock()?.shared_package_files()?;
        let rejection = match registry::apply(snapshot, request) {
            Ok((target, _)) => {
                preflight(&target, &request.action, &resolve, shared_package_files).err()
            }
            Err(_) => None,
        };
        self.lock()?.apply_config_checked(request, rejection)
    }
}

fn preflight(
    target: &Manifest,
    action: &ConfigAction,
    resolve: &impl Fn(&model::ApplicationLocator) -> Result<ResolvedApplication>,
    shared_package_files: bool,
) -> std::result::Result<(), &'static str> {
    use model::InstanceData;
    let (application_id, new_instance_id) = match action {
        ConfigAction::AddApplication { application } => (application.id, None),
        ConfigAction::CreateInstance { instance } => (instance.application_id, Some(instance.id)),
        ConfigAction::CloneInstance { instance_id, .. } => {
            let instance = target
                .instances
                .iter()
                .find(|i| i.id == *instance_id)
                .ok_or("INSTANCE_NOT_FOUND")?;
            (instance.application_id, Some(*instance_id))
        }
        // Existing registration remains editable/removable after uninstall.
        _ => return Ok(()),
    };
    let application = target
        .applications
        .iter()
        .find(|a| a.id == application_id)
        .ok_or("APPLICATION_NOT_FOUND")?;
    let resolved = resolve(&application.locator).map_err(installation_error)?;
    if let Some(id) = new_instance_id {
        let instance = target
            .instances
            .iter()
            .find(|i| i.id == id)
            .ok_or("INSTANCE_NOT_FOUND")?;
        match &instance.data {
            InstanceData::Isolated { location } => {
                let wants_package =
                    resolved.package().is_some_and(|p| p.isolated_storage) && !shared_package_files;
                if wants_package
                    != matches!(location, model::StorageLocation::PackageLocalState { .. })
                {
                    return Err("INSTANCE_STORAGE_SELECTION_CHANGED");
                }
            }
            InstanceData::Original {} => {
                for existing in target
                    .instances
                    .iter()
                    .filter(|i| i.id != id && matches!(i.data, InstanceData::Original {}))
                {
                    let other = target
                        .applications
                        .iter()
                        .find(|a| a.id == existing.application_id)
                        .ok_or("APPLICATION_NOT_FOUND")?;
                    if same_resolved(&resolved, &other.locator, resolve)? {
                        return Err("DUPLICATE_PHYSICAL_ORIGINAL");
                    }
                }
            }
        }
    } else {
        for other in target
            .applications
            .iter()
            .filter(|a| a.id != application.id && a.template_ref == application.template_ref)
        {
            if same_resolved(&resolved, &other.locator, resolve)? {
                return Err("DUPLICATE_PHYSICAL_APPLICATION");
            }
        }
    }
    Ok(())
}

fn same_resolved(
    candidate: &ResolvedApplication,
    other: &model::ApplicationLocator,
    resolve: &impl Fn(&model::ApplicationLocator) -> Result<ResolvedApplication>,
) -> std::result::Result<bool, &'static str> {
    match resolve(other) {
        Ok(existing) => Ok(candidate.same_installation(&existing)),
        // A removed installation cannot be a currently matching file. Other
        // failures remain unknown and block the edit instead of guessing.
        Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(Error::Invalid(app_proxy_core::error_code::APP_NOT_INSTALLED)) => Ok(false),
        Err(error) => Err(installation_error(error)),
    }
}

fn installation_error(error: Error) -> &'static str {
    match error {
        Error::Invalid(code) => code,
        Error::Io(e) if e.kind() == std::io::ErrorKind::NotFound => "APP_NOT_INSTALLED",
        Error::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            "INSTALLATION_ACCESS_DENIED"
        }
        _ => "INSTALLATION_CHECK_FAILED",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::*;
    use registry::{NewData, NewInstance};
    use std::{fs, sync::Arc};

    #[test]
    fn catalog_bounds_serialized_bytes_preserves_locators_and_detects_revision_changes() {
        let mut manifest = Manifest::empty("S-1-5-21-fixture".into());
        let directories = format!("{}\\", "目录".repeat(60)).repeat(180);
        for n in 0..20 {
            let path = std::path::PathBuf::from(format!("C:\\{directories}app-{n}.exe"));
            assert!(path.to_str().unwrap().encode_utf16().count() < 32767);
            manifest
                .applications
                .push(application(path, Template::Codex));
        }
        manifest.validate().unwrap();
        let encoded = serde_json::to_vec(&manifest).unwrap();
        let mut offset = 0;
        let mut observed = Vec::new();
        loop {
            let page =
                catalog_page(serde_json::from_slice(&encoded).unwrap(), offset, Some(1)).unwrap();
            assert!(serde_json::to_vec(&page).unwrap().len() <= 512 * 1024);
            if offset == 0 {
                assert!(page.applications.len() < 16);
            }
            observed.extend(page.applications);
            let Some(next) = page.next_offset else {
                break;
            };
            offset = next;
        }
        assert_eq!(observed.len(), manifest.applications.len());
        for (a, b) in observed.iter().zip(&manifest.applications) {
            assert!(a.locator == b.locator);
            assert_eq!(a.id, b.id);
        }
        manifest.revision = 2;
        assert!(matches!(
            catalog_page(manifest, 1, Some(1)),
            Err(Error::Invalid("CATALOG_CHANGED"))
        ));
        let mut manifest = Manifest::empty("S-1-5-21-fixture".into());
        manifest.applications.push(application(
            std::path::PathBuf::from(format!("C:\\{}.exe", "目录".repeat(100_000))),
            Template::Codex,
        ));
        assert!(matches!(
            catalog_page(manifest, 0, None),
            Err(Error::Invalid("CATALOG_ENTRY_TOO_LARGE"))
        ));
    }

    fn request(revision: u64, action: ConfigAction) -> ConfigRequest {
        ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: revision,
            action,
        }
    }
    fn application(path: std::path::PathBuf, template_ref: Template) -> Application {
        Application {
            id: Uuid::new_v4(),
            name: "fixture".into(),
            revision: 1,
            locator: ApplicationLocator::Exe { path },
            template_ref,
        }
    }
    fn create(application_id: Uuid) -> ConfigAction {
        ConfigAction::CreateInstance {
            instance: NewInstance {
                id: Uuid::new_v4(),
                application_id,
                name: "original".into(),
                data: NewData::Original {},
                network: NetworkBinding::Direct {},
                guard: None,
                args: vec![],
                env: SavedEnvironment::default(),
                cwd: WorkingDirectory::Application {},
            },
        }
    }
    fn applied(outcome: ConfigOutcome) {
        assert!(matches!(outcome, ConfigOutcome::Applied { .. }));
    }
    fn rejected(outcome: ConfigOutcome, expected: &str) {
        let ConfigOutcome::Rejected { code, .. } = outcome else {
            panic!("expected rejection")
        };
        assert_eq!(code, expected);
    }

    #[test]
    fn query_completes_an_accepted_write_after_a_transient_file_lock() {
        use std::os::windows::fs::OpenOptionsExt;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let path = temp.path().join("app.exe");
        fs::write(&path, b"not executed").unwrap();
        let service = Configuration::new(Store::create(&root).unwrap());
        let change = request(
            1,
            ConfigAction::AddApplication {
                application: application(path, Template::Codex),
            },
        );
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("manifest.json"))
            .unwrap();
        assert!(service.apply(&change).is_err());
        assert_eq!(service.snapshot().unwrap().revision, 1);
        drop(held);
        assert!(matches!(
            service.request_status(change.request_id).unwrap(),
            Some(ConfigRequestStatus::Complete {
                outcome: ConfigOutcome::Applied { .. }
            })
        ));
        assert_eq!(service.snapshot().unwrap().revision, 2);
    }

    #[test]
    fn rejects_physical_alias_registration_and_original_across_templates() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first.exe");
        let alias = temp.path().join("alias.exe");
        fs::write(&first, b"not executed").unwrap();
        fs::hard_link(&first, &alias).unwrap();
        let service = Configuration::new(Store::create(&temp.path().join("store")).unwrap());
        let a = application(first, Template::Codex);
        let a_id = a.id;
        applied(
            service
                .apply(&request(1, ConfigAction::AddApplication { application: a }))
                .unwrap(),
        );
        rejected(
            service
                .apply(&request(
                    2,
                    ConfigAction::AddApplication {
                        application: application(alias.clone(), Template::Codex),
                    },
                ))
                .unwrap(),
            "DUPLICATE_PHYSICAL_APPLICATION",
        );
        let b = application(alias, Template::Chromium);
        let b_id = b.id;
        applied(
            service
                .apply(&request(2, ConfigAction::AddApplication { application: b }))
                .unwrap(),
        );
        applied(service.apply(&request(3, create(a_id))).unwrap());
        rejected(
            service.apply(&request(4, create(b_id))).unwrap(),
            "DUPLICATE_PHYSICAL_ORIGINAL",
        );
        assert_eq!(service.snapshot().unwrap().instances.len(), 1);
    }

    #[test]
    fn replay_bypasses_changed_dependencies_and_preflight_failure_is_durable() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.exe");
        fs::write(&path, b"not executed").unwrap();
        let root = temp.path().join("store");
        let service = Configuration::new(Store::create(&root).unwrap());
        let saved = request(
            1,
            ConfigAction::AddApplication {
                application: application(path.clone(), Template::Codex),
            },
        );
        applied(service.apply(&saved).unwrap());
        fs::remove_file(&path).unwrap();
        applied(
            service
                .apply_with(&saved, |_| panic!("replay must not query OS"))
                .unwrap(),
        );
        let missing = temp.path().join("missing.exe");
        let failed = request(
            2,
            ConfigAction::AddApplication {
                application: application(missing.clone(), Template::Codex),
            },
        );
        rejected(service.apply(&failed).unwrap(), "APP_NOT_INSTALLED");
        fs::write(missing, b"now present").unwrap();
        drop(service);
        let service = Configuration::new(Store::open(&root).unwrap());
        rejected(
            service
                .apply_with(&failed, |_| panic!("rejected request must replay"))
                .unwrap(),
            "APP_NOT_INSTALLED",
        );
    }

    #[test]
    fn slow_installation_query_does_not_hold_commit_gate_and_stale_check_wins() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("app.exe");
        fs::write(&path, b"not executed").unwrap();
        let service = Arc::new(Configuration::new(
            Store::create(&temp.path().join("store")).unwrap(),
        ));
        let slow = request(
            1,
            ConfigAction::AddApplication {
                application: application(path.clone(), Template::Codex),
            },
        );
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_service = service.clone();
        let worker = std::thread::spawn(move || {
            worker_service
                .apply_with(&slow, |_| {
                    entered_tx.send(()).unwrap();
                    release_rx
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                    Err(Error::Invalid(
                        app_proxy_core::error_code::APP_NOT_INSTALLED,
                    ))
                })
                .unwrap()
        });
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(service.snapshot().unwrap().revision, 1);
        applied(
            service
                .apply(&request(
                    1,
                    ConfigAction::AddApplication {
                        application: application(path, Template::Codex),
                    },
                ))
                .unwrap(),
        );
        release_tx.send(()).unwrap();
        rejected(worker.join().unwrap(), "STALE_MANIFEST_REVISION");
        assert_eq!(service.snapshot().unwrap().revision, 2);
    }
}
