//! Configuration-only intent → manifest → receipt protocol. No external process,
//! shortcut or registry side effects are allowed inside these transactions.
use crate::journal::{RETENTION, RequestJournal, now};
use crate::{
    Error, Result, storage_security as security,
    store::{self, Store},
};
use app_proxy_core::{
    model::{MANIFEST_LIMIT, Manifest},
    registry::{self, ConfigAction, ConfigReceipt, ConfigRequest},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::path::PathBuf;
use uuid::Uuid;

pub(crate) const REQUEST_LIMIT: usize = 1024 * 1024;
const RECORD_LIMIT: usize = 2 * MANIFEST_LIMIT + 16384;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigOutcome {
    Applied { receipt: ConfigReceipt },
    Rejected { code: String, current_revision: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigRequestStatus {
    Pending {},
    Complete { outcome: ConfigOutcome },
}

// No Debug: a pending target can contain secret-valued configuration fields.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    format: String,
    schema_version: u32,
    store_id: Uuid,
    request_id: Uuid,
    request_digest: [u8; 32],
    expected_revision: u64,
    accepted_at: u64,
    phase: Phase,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum Phase {
    Pending {
        before: [u8; 32],
        after: [u8; 32],
        target: Box<Manifest>,
        receipt: ConfigReceipt,
    },
    Complete {
        outcome: ConfigOutcome,
        completed_at: u64,
    },
}

impl Store {
    pub fn apply_config(&mut self, request: &ConfigRequest) -> Result<ConfigOutcome> {
        self.apply_config_checked(request, None)
    }

    /// Replay before doing read-only installation checks, which may no longer
    /// succeed after a previously accepted request (for example after uninstall).
    pub fn replay_config(&mut self, request: &ConfigRequest) -> Result<Option<ConfigOutcome>> {
        if request.request_id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        self.ensure_request_id_unused_elsewhere(request.request_id, RequestJournal::Config)?;
        let digest = digest_bytes(&store::encode(request, REQUEST_LIMIT)?);
        self.recover_config_requests()?;
        let header = self.load()?;
        let _directory = self.request_directory(&header.owner_sid, false)?;
        let Some(record) = self.read_record(request.request_id, &header)? else {
            return Ok(None);
        };
        if record.request_digest != digest {
            return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
        }
        match record.phase {
            Phase::Complete { outcome, .. } => Ok(Some(outcome)),
            Phase::Pending { .. } => Err(Error::Invalid("CONFIG_REQUEST_PENDING")),
        }
    }

    /// The coordinator computes a fixed-code rejection using a read-only
    /// preflight outside its commit gate. Revision/dedup checks still win here.
    pub fn apply_config_checked(
        &mut self,
        request: &ConfigRequest,
        rejection: Option<&'static str>,
    ) -> Result<ConfigOutcome> {
        self.ensure_request_id_unused_elsewhere(request.request_id, RequestJournal::Config)?;
        if rejection.is_some_and(|code| {
            code.is_empty()
                || code.len() > 96
                || !code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
        }) {
            return Err(Error::Invalid("INVALID_REJECTION_CODE"));
        }
        if request.request_id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let digest = digest_bytes(&store::encode(request, REQUEST_LIMIT)?);
        self.recover_config_requests()?;
        let header = self.load()?;
        let _directory = self.request_directory(&header.owner_sid, true)?;
        if let Some(record) = self.read_record(request.request_id, &header)? {
            if record.request_digest != digest {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
            return match record.phase {
                Phase::Complete { outcome, .. } => Ok(outcome),
                Phase::Pending { .. } => Err(Error::Invalid("CONFIG_REQUEST_PENDING")),
            };
        }
        let before = manifest_digest(&header)?;
        self.ensure_core_update_idle()?;
        let current_revision = header.revision;
        let store_id = header.store_id;
        let accepted_at = now()?;
        let phase = match registry::apply(header, request) {
            Ok((mut target, receipt)) => {
                target.revision = receipt.revision;
                // Ensure the complete snapshot fits and all referenced secrets exist
                // before a pending record can authorize changing the manifest.
                store::encode(&target, MANIFEST_LIMIT)?;
                let rejection = rejection
                    .or(self.profile_edit_rejection(&request.action, &target)?)
                    .or(self.shortcut_edit_rejection(&request.action)?);
                if rejection.is_none() {
                    self.stage_proxy_secret(request)?;
                    if let ConfigAction::EditInstance { edit, .. } = &request.action
                        && let Some(environment) = &edit.env
                    {
                        for entry in &environment.set {
                            self.put_secret_once(entry.secret_id, &entry.value)?;
                        }
                    }
                }
                if let Some(code) = rejection {
                    Phase::Complete {
                        outcome: ConfigOutcome::Rejected {
                            code: code.into(),
                            current_revision,
                        },
                        completed_at: now()?,
                    }
                } else if self.validate(&target).is_err() {
                    Phase::Complete {
                        outcome: ConfigOutcome::Rejected {
                            code: "INVALID_CONFIG_DEPENDENCY".into(),
                            current_revision,
                        },
                        completed_at: now()?,
                    }
                } else {
                    let after = manifest_digest(&target)?;
                    Phase::Pending {
                        before,
                        after,
                        target: Box::new(target),
                        receipt,
                    }
                }
            }
            Err(error) => Phase::Complete {
                outcome: ConfigOutcome::Rejected {
                    code: error.0.into(),
                    current_revision,
                },
                completed_at: now()?,
            },
        };
        let record = Record {
            format: "app-proxy-rust-config-request".into(),
            schema_version: 1,
            store_id,
            request_id: request.request_id,
            request_digest: digest,
            expected_revision: request.expected_revision,
            accepted_at,
            phase,
        };
        self.write_record(&record)?;
        self.finish(record)
    }

    pub(crate) fn stage_proxy_secret(&self, request: &ConfigRequest) -> Result<()> {
        let node = match &request.action {
            ConfigAction::CreateManualProfile { node, .. }
            | ConfigAction::UpdateManualProfile { node, .. } => node,
            _ => return Ok(()),
        };
        if let Some(credentials) = &node.credentials {
            self.put_secret_once(request.request_id, &credentials.password)?;
        }
        Ok(())
    }

    // Checked inside the same store gate as intent/commit. A candidate being
    // prepared outside this gate must still pass the manager's current check.
    fn profile_edit_rejection(
        &self,
        action: &ConfigAction,
        target: &Manifest,
    ) -> Result<Option<&'static str>> {
        use crate::core_state::CoreState;
        let id = match action {
            ConfigAction::UpdateManualProfile { profile_id, .. }
            | ConfigAction::EditSubscriptionProfile { profile_id, .. }
            | ConfigAction::RemoveProfile { profile_id } => profile_id,
            _ => return Ok(None),
        };
        let generation = match self.core_state()? {
            CoreState::Stopped {} => return Ok(None),
            CoreState::Down { generation }
            | CoreState::Starting { generation }
            | CoreState::Running { generation, .. } => generation,
        };
        let generation = self.open_core_generation(generation)?;
        if matches!(action, ConfigAction::EditSubscriptionProfile { .. })
            && self.core_generation_matches(&generation, target)?
        {
            return Ok(None);
        }
        Ok(generation
            .profiles()
            .iter()
            .any(|p| p.id == *id)
            .then_some("CORE_RECONFIGURATION_REQUIRED"))
    }

    pub fn config_request_status(&self, request_id: Uuid) -> Result<Option<ConfigRequestStatus>> {
        if request_id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let header = self.load()?;
        let _directory = self.request_directory(&header.owner_sid, false)?;
        self.read_record(request_id, &header).map(|record| {
            record.map(|record| match record.phase {
                Phase::Pending { .. } => ConfigRequestStatus::Pending {},
                Phase::Complete { outcome, .. } => ConfigRequestStatus::Complete { outcome },
            })
        })
    }

    /// Called before reporting coordinator readiness or accepting another edit.
    pub fn recover_config_requests(&mut self) -> Result<()> {
        let header = self.load()?;
        let Some(_directory) = self.request_directory(&header.owner_sid, false)? else {
            return Ok(());
        };
        let mut pending = None;
        let now = now()?;
        for entry in std::fs::read_dir(self.root().join("state/requests"))? {
            let entry = entry?;
            let name = entry.file_name();
            let text = name
                .to_str()
                .ok_or(Error::Invalid("UNKNOWN_REQUEST_FILE"))?;
            // Incomplete same-directory temporary writes were never committed.
            if text.starts_with(".tmp") {
                continue;
            }
            let id = text
                .strip_suffix(".json")
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(Error::Invalid("UNKNOWN_REQUEST_FILE"))?;
            if text != format!("{id}.json") {
                return Err(Error::Invalid("UNKNOWN_REQUEST_FILE"));
            }
            let record = self
                .read_record(id, &header)?
                .ok_or(Error::Invalid("REQUEST_RECORD_DISAPPEARED"))?;
            match &record.phase {
                Phase::Pending { .. } => {
                    if pending.is_some() {
                        return Err(Error::Invalid("MULTIPLE_PENDING_CONFIG_REQUESTS"));
                    }
                    pending = Some(record);
                }
                Phase::Complete { completed_at, .. }
                    if now
                        .checked_sub(*completed_at)
                        .is_some_and(|age| age > RETENTION) =>
                {
                    // Only our verified UUID file; pending records never expire.
                    std::fs::remove_file(self.record_path(id))?;
                }
                _ => {}
            }
        }
        if let Some(record) = pending {
            self.finish(record)?;
        }
        Ok(())
    }

    fn finish(&mut self, mut record: Record) -> Result<ConfigOutcome> {
        let Phase::Pending {
            before,
            after,
            mut target,
            receipt,
        } = record.phase
        else {
            let Phase::Complete { outcome, .. } = record.phase else {
                unreachable!()
            };
            return Ok(outcome);
        };
        let current = self.load()?;
        let observed = manifest_digest(&current)?;
        if observed == before {
            if current.revision != record.expected_revision {
                return Err(Error::Invalid("CONFIG_TRANSACTION_CONFLICT"));
            }
            target.revision = record.expected_revision;
            self.commit_snapshot(record.expected_revision, *target)?;
        } else if observed != after {
            return Err(Error::Invalid("CONFIG_TRANSACTION_CONFLICT"));
        }
        let outcome = ConfigOutcome::Applied { receipt };
        record.phase = Phase::Complete {
            outcome: outcome.clone(),
            completed_at: now()?,
        };
        self.write_record(&record)?;
        Ok(outcome)
    }

    fn record_path(&self, id: Uuid) -> PathBuf {
        self.root()
            .join("state/requests")
            .join(format!("{id}.json"))
    }

    fn request_directory(&self, sid: &str, create: bool) -> Result<Option<OwnedHandle>> {
        let path = self.root().join("state/requests");
        if !path.try_exists()? {
            if !create {
                return Ok(None);
            }
            security::no_reparse(
                path.parent()
                    .ok_or(Error::Invalid("REQUEST_PARENT_REQUIRED"))?,
            )?;
            std::fs::create_dir(&path)?;
        }
        let handle = security::directory(&path, false)?;
        security::verify(handle.as_raw_handle(), sid, false)?;
        Ok(Some(handle))
    }

    fn write_record(&self, record: &Record) -> Result<()> {
        self.replace_bounded(
            &format!("state/requests/{}.json", record.request_id),
            &store::encode(record, RECORD_LIMIT)?,
            RECORD_LIMIT,
        )
    }

    fn read_record(&self, id: Uuid, manifest: &Manifest) -> Result<Option<Record>> {
        let path = self.record_path(id);
        if !path.try_exists()? {
            return Ok(None);
        }
        let record: Record = store::decode(&store::read_protected(
            &path,
            &manifest.owner_sid,
            RECORD_LIMIT,
        )?)?;
        if record.format != "app-proxy-rust-config-request"
            || record.schema_version != 1
            || record.store_id != manifest.store_id
            || record.request_id != id
            || id.is_nil()
            || record.accepted_at == 0
        {
            return Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"));
        }
        match &record.phase {
            Phase::Pending {
                after,
                target,
                receipt,
                ..
            } => {
                self.validate(target)?;
                if manifest_digest(target)? != *after
                    || receipt.entity_id.is_nil()
                    || target.revision != receipt.revision
                    || record.expected_revision.checked_add(1) != Some(receipt.revision)
                {
                    return Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"));
                }
            }
            Phase::Complete {
                outcome,
                completed_at,
            } => {
                if *completed_at == 0 {
                    return Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"));
                }
                match outcome {
                    ConfigOutcome::Applied { receipt }
                        if receipt.entity_id.is_nil()
                            || receipt.revision > manifest.revision
                            || record.expected_revision.checked_add(1)
                                != Some(receipt.revision) =>
                    {
                        return Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"));
                    }
                    ConfigOutcome::Rejected {
                        code,
                        current_revision,
                    } if code.is_empty()
                        || code.len() > 96
                        || !code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_')
                        || *current_revision > manifest.revision
                        || *current_revision == 0 =>
                    {
                        return Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"));
                    }
                    _ => {}
                }
            }
        }
        Ok(Some(record))
    }
}

fn digest_bytes(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn manifest_digest(manifest: &Manifest) -> Result<[u8; 32]> {
    Ok(digest_bytes(&store::encode(manifest, MANIFEST_LIMIT)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_core::{
        model::{Application, ApplicationLocator, Template},
        registry::ConfigAction,
    };
    use std::{fs, os::windows::fs::OpenOptionsExt};

    fn add(revision: u64) -> ConfigRequest {
        ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: revision,
            action: ConfigAction::AddApplication {
                application: Application {
                    id: Uuid::new_v4(),
                    name: "fixture".into(),
                    revision: 1,
                    locator: ApplicationLocator::Exe {
                        path: PathBuf::from(format!("C:\\fixture\\{}.exe", Uuid::new_v4())),
                    },
                    template_ref: Template::Codex,
                },
            },
        }
    }

    // Materialize the exact durable state at the two interruption boundaries.
    // No production failpoints or real application launches are necessary.
    fn stage(store: &Store, request: &ConfigRequest) -> Record {
        let base = store.load().unwrap();
        let store_id = base.store_id;
        let before = manifest_digest(&base).unwrap();
        let _dir = store.request_directory(&base.owner_sid, true).unwrap();
        let (mut target, receipt) = registry::apply(base, request).unwrap();
        target.revision = receipt.revision;
        let record = Record {
            format: "app-proxy-rust-config-request".into(),
            schema_version: 1,
            store_id,
            request_id: request.request_id,
            request_digest: digest_bytes(&store::encode(request, REQUEST_LIMIT).unwrap()),
            expected_revision: request.expected_revision,
            accepted_at: now().unwrap(),
            phase: Phase::Pending {
                before,
                after: manifest_digest(&target).unwrap(),
                target: Box::new(target),
                receipt,
            },
        };
        store.write_record(&record).unwrap();
        record
    }

    fn applied(outcome: ConfigOutcome, revision: u64) -> Uuid {
        let ConfigOutcome::Applied { receipt } = outcome else {
            panic!("expected applied")
        };
        assert_eq!(receipt.revision, revision);
        receipt.entity_id
    }

    fn editable_instance(store: &mut Store) -> Uuid {
        use app_proxy_core::model::*;
        let app = applied(store.apply_config(&add(1)).unwrap(), 2);
        let instance = Uuid::new_v4();
        let request = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::CreateInstance {
                instance: registry::NewInstance {
                    id: instance,
                    application_id: app,
                    name: "editable".into(),
                    data: registry::NewData::Original {},
                    network: NetworkBinding::Direct {},
                    guard: None,
                    args: vec![],
                    env: SavedEnvironment::default(),
                    cwd: WorkingDirectory::Application {},
                },
            },
        };
        applied(store.apply_config(&request).unwrap(), 3);
        instance
    }
    fn advanced(instance_id: Uuid) -> ConfigRequest {
        ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 3,
            action: ConfigAction::EditInstance {
                instance_id,
                edit: registry::InstanceEdit {
                    args: Some(vec!["next-argument".into()]),
                    cwd: None,
                    env: Some(registry::EnvironmentEdit {
                        set: vec![registry::EnvironmentAssignment {
                            name: "TOKEN".into(),
                            secret_id: Uuid::new_v4(),
                            value: "private-settings-secret".into(),
                        }],
                        unset: vec!["OLD_TOKEN".into()],
                        inherit: vec![],
                    }),
                },
            },
        }
    }
    #[test]
    fn advanced_edit_recovers_with_immutable_secrets_and_receipts_never_contain_values() {
        use app_proxy_core::model::EnvValue;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let instance = editable_instance(&mut store);
        let request = advanced(instance);
        let pinned = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("manifest.json"))
            .unwrap();
        assert!(store.apply_config(&request).is_err());
        assert!(matches!(
            store.config_request_status(request.request_id).unwrap(),
            Some(ConfigRequestStatus::Pending {})
        ));
        let intent =
            fs::read_to_string(root.join(format!("state/requests/{}.json", request.request_id)))
                .unwrap();
        assert!(!intent.contains("private-settings-secret"));
        assert_eq!(fs::read_dir(root.join("secrets")).unwrap().count(), 1);
        drop(pinned);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        let saved = store.load().unwrap();
        assert_eq!(saved.revision, 4);
        let EnvValue::SecretRef { id } = &saved.instances[0].env.set["TOKEN"] else {
            panic!("secret reference required")
        };
        assert_eq!(store.read_secret(*id).unwrap(), "private-settings-secret");
        assert!(
            !fs::read_to_string(root.join("manifest.json"))
                .unwrap()
                .contains("private-settings-secret")
        );
        applied(store.apply_config(&request).unwrap(), 4);
        assert_eq!(fs::read_dir(root.join("secrets")).unwrap().count(), 1);
        let mut changed = request;
        if let ConfigAction::EditInstance { edit, .. } = &mut changed.action {
            edit.env.as_mut().unwrap().set[0].value = "other-private-value".into();
        }
        assert!(matches!(
            store.apply_config(&changed),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        assert_eq!(store.read_secret(*id).unwrap(), "private-settings-secret");
        let current = store.load().unwrap();
        assert_eq!(current.instances[0].args, ["next-argument"]);
    }
    #[test]
    fn rejected_advanced_patch_does_not_stage_even_an_earlier_valid_assignment() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let instance = editable_instance(&mut store);
        for value in ["invalid\0value", "valid value"] {
            let mut request = advanced(instance);
            if let ConfigAction::EditInstance { edit, .. } = &mut request.action {
                edit.env
                    .as_mut()
                    .unwrap()
                    .set
                    .push(registry::EnvironmentAssignment {
                        name: if value.contains('\0') {
                            "SECOND"
                        } else {
                            "HTTP_PROXY"
                        }
                        .into(),
                        secret_id: Uuid::new_v4(),
                        value: value.into(),
                    });
            }
            assert!(matches!(
                store.apply_config(&request).unwrap(),
                ConfigOutcome::Rejected { .. }
            ));
            assert_eq!(
                fs::read_dir(store.root().join("secrets")).unwrap().count(),
                0
            );
            assert_eq!(store.load().unwrap().revision, 3);
        }
    }

    #[test]
    fn pending_instance_removal_finishes_before_shortcut_can_reserve_it() {
        use crate::shortcuts::{
            Spec,
            journal::{Action, Plan, Request},
        };
        use app_proxy_core::model::*;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let app = applied(store.apply_config(&add(1)).unwrap(), 2);
        let instance_id = Uuid::new_v4();
        let create = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::CreateInstance {
                instance: registry::NewInstance {
                    id: instance_id,
                    application_id: app,
                    name: "fixture".into(),
                    data: registry::NewData::Original {},
                    network: NetworkBinding::Direct {},
                    guard: None,
                    args: vec![],
                    env: SavedEnvironment::default(),
                    cwd: WorkingDirectory::Application {},
                },
            },
        };
        applied(store.apply_config(&create).unwrap(), 3);
        let remove = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 3,
            action: ConfigAction::RemoveInstance { instance_id },
        };
        stage(&store, &remove);
        let request = Request {
            id: Uuid::new_v4(),
            instance_id,
            expected_revision: 3,
            action: Action::Create,
            expected_creation: None,
        };
        let path = temp.path().join("entry.lnk");
        let plan = Plan {
            path: path.clone(),
            spec: Spec {
                store_id: store.load().unwrap().store_id,
                instance_id,
                home: root,
                host: temp.path().join("app-proxy-host.exe"),
                icon: temp.path().join("icon.ico"),
            },
        };
        assert!(matches!(
            store.apply_shortcut(&request, Some(plan)),
            Err(Error::Invalid("STALE_MANIFEST_REVISION"))
        ));
        assert!(store.load().unwrap().instances.is_empty());
        assert_eq!(store.load().unwrap().revision, 4);
        assert!(store.shortcut_request_status(request.id).unwrap().is_none());
        assert!(!path.exists());
    }

    fn add_proxy() -> ConfigRequest {
        use app_proxy_core::{model::*, registry::*};
        ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 1,
            action: ConfigAction::CreateManualProfile {
                profile_id: Uuid::new_v4(),
                name: "proxy".into(),
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: 29123,
                },
                node: ManualProxyInput {
                    protocol: ManualProtocol::Http,
                    host: "proxy.example".into(),
                    port: 8080,
                    credentials: Some(ProxyCredentialInput {
                        username: "fixture-user".into(),
                        password: "private-password-fixture".into(),
                    }),
                },
            },
        }
    }

    #[test]
    fn proxy_secret_staging_is_atomic_idempotent_and_not_in_receipts() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let store = Store::create(&root).unwrap();
        let request = add_proxy();
        // Interruption before the pending intent: retry reuses the same immutable secret.
        store.stage_proxy_secret(&request).unwrap();
        drop(store);
        let mut store = Store::open(&root).unwrap();
        let id = applied(store.apply_config(&request).unwrap(), 2);
        assert_eq!(
            store.read_secret(request.request_id).unwrap(),
            "private-password-fixture"
        );
        assert_eq!(fs::read_dir(root.join("secrets")).unwrap().count(), 1);
        for path in [
            root.join("manifest.json"),
            store.record_path(request.request_id),
        ] {
            assert!(
                !fs::read_to_string(path)
                    .unwrap()
                    .contains("private-password-fixture")
            );
        }
        drop(store);
        let mut store = Store::open(&root).unwrap();
        assert_eq!(applied(store.apply_config(&request).unwrap(), 2), id);
        let mut changed = request;
        let ConfigAction::CreateManualProfile { node, .. } = &mut changed.action else {
            panic!()
        };
        node.credentials.as_mut().unwrap().password = "different".into();
        assert!(matches!(
            store.apply_config(&changed),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        assert_eq!(
            store.read_secret(changed.request_id).unwrap(),
            "private-password-fixture"
        );
    }

    #[test]
    fn proxy_pending_recovery_has_all_secret_dependencies_and_no_raw_password() {
        for after_commit in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let mut store = Store::create(&root).unwrap();
            let request = add_proxy();
            store.stage_proxy_secret(&request).unwrap();
            let record = stage(&store, &request);
            assert!(
                !fs::read_to_string(store.record_path(request.request_id))
                    .unwrap()
                    .contains("private-password-fixture")
            );
            if after_commit {
                let Phase::Pending { mut target, .. } = record.phase else {
                    panic!()
                };
                target.revision = 1;
                store.commit_snapshot(1, *target).unwrap();
            }
            drop(store);
            let mut store = Store::open(&root).unwrap();
            applied(store.apply_config(&request).unwrap(), 2);
            assert_eq!(store.load().unwrap().revision, 2);
            assert_eq!(
                store.read_secret(request.request_id).unwrap(),
                "private-password-fixture"
            );
        }
    }

    #[test]
    fn rejected_proxy_edits_never_stage_passwords_or_change_active_generation() {
        use crate::core_state::CoreState;
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let mut stale = add_proxy();
        stale.expected_revision = 99;
        assert!(matches!(
            store.apply_config(&stale).unwrap(),
            ConfigOutcome::Rejected { .. }
        ));
        assert!(
            !store
                .root()
                .join(format!("secrets/{}.json", stale.request_id))
                .exists()
        );
        let request = add_proxy();
        let profile_id = applied(store.apply_config(&request).unwrap(), 2);
        let generation = store.prepare_core_generation(&[profile_id]).unwrap();
        let starting = CoreState::Starting {
            generation: generation.id(),
        };
        store
            .transition_core_state(&CoreState::Stopped {}, starting.clone())
            .unwrap();
        let ConfigAction::CreateManualProfile { node, .. } = request.action else {
            panic!()
        };
        let update = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::UpdateManualProfile { profile_id, node },
        };
        assert!(
            matches!(store.apply_config(&update).unwrap(), ConfigOutcome::Rejected { code, .. } if code == "CORE_RECONFIGURATION_REQUIRED")
        );
        assert!(
            !store
                .root()
                .join(format!("secrets/{}.json", update.request_id))
                .exists()
        );
        assert_eq!(store.core_state().unwrap(), starting);
        let rename = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::RenameProfile {
                profile_id,
                name: "new display".into(),
            },
        };
        applied(store.apply_config(&rename).unwrap(), 3);
        assert!(store.core_generation_is_current(&generation).unwrap());
        let remove = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 3,
            action: ConfigAction::RemoveProfile { profile_id },
        };
        assert!(
            matches!(store.apply_config(&remove).unwrap(), ConfigOutcome::Rejected { code, .. } if code == "CORE_RECONFIGURATION_REQUIRED")
        );
        store
            .transition_core_state(&starting, CoreState::Stopped {})
            .unwrap();
        let remove = ConfigRequest {
            request_id: Uuid::new_v4(),
            ..remove
        };
        applied(store.apply_config(&remove).unwrap(), 4);
        assert!(store.load().unwrap().profiles.is_empty());
        assert_eq!(
            store.read_secret(request.request_id).unwrap(),
            "private-password-fixture"
        );
    }

    #[test]
    fn immutable_secret_collision_and_broken_existing_secret_are_preserved() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::create(&temp.path().join("store")).unwrap();
        let id = Uuid::new_v4();
        store.put_secret_once(id, "first").unwrap();
        assert!(matches!(
            store.put_secret_once(id, "second"),
            Err(Error::Invalid("SECRET_ID_CONFLICT"))
        ));
        assert_eq!(store.read_secret(id).unwrap(), "first");
        let path = store.root().join(format!("secrets/{id}.json"));
        fs::write(&path, b"broken").unwrap();
        assert!(store.put_secret_once(id, "first").is_err());
        assert_eq!(fs::read(path).unwrap(), b"broken");
    }

    #[test]
    fn pending_proxy_edit_prevents_starting_an_old_generation() {
        use crate::core_state::CoreState;
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let create = add_proxy();
        let profile_id = applied(store.apply_config(&create).unwrap(), 2);
        let old = store.prepare_core_generation(&[profile_id]).unwrap();
        let ConfigAction::CreateManualProfile { mut node, .. } = create.action else {
            panic!()
        };
        node.port += 1;
        let update = ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: 2,
            action: ConfigAction::UpdateManualProfile { profile_id, node },
        };
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("manifest.json"))
            .unwrap();
        assert!(store.apply_config(&update).is_err());
        let starting_old = CoreState::Starting {
            generation: old.id(),
        };
        assert!(
            store
                .transition_core_state(&CoreState::Stopped {}, starting_old.clone())
                .is_err()
        );
        assert_eq!(store.core_state().unwrap(), CoreState::Stopped {});
        drop(held);
        assert!(matches!(
            store.transition_core_state(&CoreState::Stopped {}, starting_old),
            Err(Error::Invalid("CORE_CONFIG_CHANGED"))
        ));
        assert_eq!(store.core_state().unwrap(), CoreState::Stopped {});
        applied(store.apply_config(&update).unwrap(), 3);
        let current = store.prepare_core_generation(&[profile_id]).unwrap();
        store
            .transition_core_state(
                &CoreState::Stopped {},
                CoreState::Starting {
                    generation: current.id(),
                },
            )
            .unwrap();
    }

    #[test]
    fn deduplicates_across_reopen_and_later_edits() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let request = add(1);
        let id = applied(store.apply_config(&request).unwrap(), 2);
        applied(store.apply_config(&add(2)).unwrap(), 3);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        assert_eq!(applied(store.apply_config(&request).unwrap(), 2), id);
        assert_eq!(store.load().unwrap().revision, 3);
        assert_eq!(store.load().unwrap().applications.len(), 2);
        let mut changed = add(3);
        changed.request_id = request.request_id;
        assert!(matches!(
            store.apply_config(&changed),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        assert!(matches!(
            store.config_request_status(request.request_id).unwrap(),
            Some(ConfigRequestStatus::Complete { .. })
        ));
        assert!(
            store
                .config_request_status(Uuid::new_v4())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn stale_rejection_remains_the_same_after_configuration_changes() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let stale = add(99);
        for iteration in 0..2 {
            let ConfigOutcome::Rejected {
                code,
                current_revision,
            } = store.apply_config(&stale).unwrap()
            else {
                panic!()
            };
            assert_eq!(code, "STALE_MANIFEST_REVISION");
            assert_eq!(current_revision, 1);
            if iteration == 0 {
                applied(store.apply_config(&add(1)).unwrap(), 2);
            }
        }
        assert_eq!(store.load().unwrap().revision, 2);
    }

    #[test]
    fn recovers_both_sides_of_manifest_commit_without_double_application() {
        for after_commit in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let mut store = Store::create(&root).unwrap();
            let request = add(1);
            let record = stage(&store, &request);
            assert!(matches!(
                store.config_request_status(request.request_id).unwrap(),
                Some(ConfigRequestStatus::Pending {})
            ));
            if after_commit {
                let Phase::Pending { mut target, .. } = record.phase else {
                    panic!()
                };
                target.revision = 1;
                store.commit_snapshot(1, *target).unwrap();
            }
            fs::write(root.join("state/requests/.tmp-interrupted"), "partial").unwrap();
            drop(store);
            let mut store = Store::open(&root).unwrap();
            applied(store.apply_config(&request).unwrap(), 2);
            assert_eq!(store.load().unwrap().applications.len(), 1);
            assert_eq!(store.load().unwrap().revision, 2);
            let backup: Manifest =
                store::decode(&fs::read(root.join("backups/manifest.previous.json")).unwrap())
                    .unwrap();
            assert_eq!(backup.revision, 1);
        }
    }

    #[test]
    fn failed_manifest_replace_keeps_pending_and_recovers_when_unlocked() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let request = add(1);
        let original = fs::read(root.join("manifest.json")).unwrap();
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("manifest.json"))
            .unwrap();
        assert!(store.apply_config(&request).is_err());
        assert_eq!(fs::read(root.join("manifest.json")).unwrap(), original);
        assert!(matches!(
            store.config_request_status(request.request_id).unwrap(),
            Some(ConfigRequestStatus::Pending {})
        ));
        drop(held);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        applied(store.apply_config(&request).unwrap(), 2);
        assert_eq!(store.load().unwrap().applications.len(), 1);
    }

    #[test]
    fn conflicting_or_corrupt_records_preserve_manifest_and_intent() {
        for corruption in [
            "foreign", "digest", "json", "multiple", "conflict", "filename",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let mut store = Store::create(&root).unwrap();
            let request = add(1);
            let mut record = stage(&store, &request);
            match corruption {
                "foreign" => {
                    record.store_id = Uuid::new_v4();
                    store.write_record(&record).unwrap();
                }
                "digest" => {
                    let Phase::Pending { after, .. } = &mut record.phase else {
                        panic!()
                    };
                    *after = [0; 32];
                    store.write_record(&record).unwrap();
                }
                "json" => fs::write(store.record_path(request.request_id), "{broken").unwrap(),
                "multiple" => {
                    stage(&store, &add(1));
                }
                "conflict" => {
                    let base = store.load().unwrap();
                    store.commit_snapshot(1, base).unwrap();
                }
                "filename" => {
                    fs::rename(
                        store.record_path(request.request_id),
                        root.join(format!(
                            "state/requests/{}.json",
                            request.request_id.simple()
                        )),
                    )
                    .unwrap();
                }
                _ => unreachable!(),
            }
            let bytes = fs::read(root.join("manifest.json")).unwrap();
            let records = fs::read_dir(root.join("state/requests")).unwrap().count();
            drop(store);
            assert!(Store::open(&root).is_err(), "{corruption}");
            assert_eq!(
                fs::read(root.join("manifest.json")).unwrap(),
                bytes,
                "{corruption}"
            );
            assert_eq!(
                fs::read_dir(root.join("state/requests")).unwrap().count(),
                records
            );
        }
    }

    #[test]
    fn terminal_receipts_cannot_claim_success_or_rejection_from_a_future_snapshot() {
        for rejected in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let mut store = Store::create(&root).unwrap();
            let original = fs::read(root.join("manifest.json")).unwrap();
            // Advance without a request record so the rejected-record variant
            // independently exercises the rejected outcome validation.
            store.commit(1, store.load().unwrap()).unwrap();
            if rejected {
                assert!(matches!(
                    store.apply_config(&add(1)).unwrap(),
                    ConfigOutcome::Rejected { .. }
                ));
            } else {
                applied(store.apply_config(&add(2)).unwrap(), 3);
            }
            drop(store);
            fs::write(root.join("manifest.json"), &original).unwrap();
            assert!(matches!(
                Store::open(&root),
                Err(Error::Invalid("INVALID_CONFIG_REQUEST_RECORD"))
            ));
            assert_eq!(fs::read(root.join("manifest.json")).unwrap(), original);
        }
    }

    #[test]
    fn receipt_replace_failure_after_manifest_commit_recovers_without_a_second_commit() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let request = add(1);
        let record = stage(&store, &request);
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.record_path(request.request_id))
            .unwrap();
        assert!(store.finish(record).is_err());
        assert_eq!(store.load().unwrap().revision, 2);
        assert!(matches!(
            store.config_request_status(request.request_id).unwrap(),
            Some(ConfigRequestStatus::Pending {})
        ));
        drop(held);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        applied(store.apply_config(&request).unwrap(), 2);
        let backup: Manifest =
            store::decode(&fs::read(root.join("backups/manifest.previous.json")).unwrap()).unwrap();
        assert_eq!(backup.revision, 1);
    }

    #[test]
    fn retention_removes_only_old_complete_records_and_recovers_old_pending() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let request = add(1);
        applied(store.apply_config(&request).unwrap(), 2);
        let header = store.load().unwrap();
        let mut record = store
            .read_record(request.request_id, &header)
            .unwrap()
            .unwrap();
        let Phase::Complete { completed_at, .. } = &mut record.phase else {
            panic!()
        };
        *completed_at = now().unwrap() - RETENTION - 1;
        store.write_record(&record).unwrap();
        let pending = add(2);
        let mut record = stage(&store, &pending);
        record.accepted_at = 1;
        store.write_record(&record).unwrap();
        drop(store);
        let mut store = Store::open(&root).unwrap();
        assert!(
            store
                .config_request_status(request.request_id)
                .unwrap()
                .is_none()
        );
        applied(store.apply_config(&pending).unwrap(), 3);
        assert_eq!(store.load().unwrap().applications.len(), 2);
    }
}
