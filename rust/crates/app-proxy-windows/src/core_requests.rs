//! Request identity is durable before core side effects. Pending requests are
//! never replayed automatically: an interrupted launch can have already spawned.
use crate::journal::{RETENTION, RequestJournal, now};
use crate::{
    Error, Result, storage_security as security,
    store::{self, Store},
};
use app_proxy_core::core_control::{CoreAction, CoreOutcome};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use uuid::Uuid;

const LIMIT: usize = 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreRequestPhase {
    Pending {
        epoch: Uuid,
    },
    Complete {
        outcome: CoreOutcome,
        completed_at: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    schema_version: u32,
    store_id: Uuid,
    request_id: Uuid,
    digest: [u8; 32],
    accepted_at: u64,
    phase: CoreRequestPhase,
}

impl Store {
    pub fn begin_core_request(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        action: &CoreAction,
    ) -> Result<(CoreRequestPhase, bool)> {
        if id.is_nil() || epoch.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        self.prune_core_requests()?;
        self.ensure_request_id_unused_elsewhere(id, RequestJournal::Core)?;
        let mut action = action.clone();
        action.normalize().map_err(|e| Error::Invalid(e.0))?;
        let digest: [u8; 32] = Sha256::digest(store::encode(&action, LIMIT)?).into();
        if let Some(record) = self.read_core_request(id)? {
            if record.digest != digest {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
            return Ok((record.phase, false));
        }
        let manifest = self.load()?;
        let _pin = self.core_request_directory(true)?;
        let phase = CoreRequestPhase::Pending { epoch };
        let record = Record {
            schema_version: 1,
            store_id: manifest.store_id,
            request_id: id,
            digest,
            accepted_at: now()?,
            phase: phase.clone(),
        };
        self.write_core_request(&record)?;
        Ok((phase, true))
    }

    pub fn core_request_status(&self, id: Uuid) -> Result<Option<CoreRequestPhase>> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        self.read_core_request(id).map(|r| r.map(|r| r.phase))
    }

    pub fn finish_core_request(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        outcome: CoreOutcome,
    ) -> Result<()> {
        let mut record = self
            .read_core_request(id)?
            .ok_or(Error::Invalid("CORE_REQUEST_NOT_FOUND"))?;
        if !matches!(record.phase, CoreRequestPhase::Pending { epoch: recorded } if recorded == epoch)
        {
            return Err(Error::Invalid("CORE_REQUEST_OWNER_CHANGED"));
        }
        validate_outcome(&outcome, &self.load()?.owner_sid)?;
        record.phase = CoreRequestPhase::Complete {
            outcome,
            completed_at: now()?.max(record.accepted_at),
        };
        self.write_core_request(&record)
    }

    /// Only the matching completed reconfiguration journal may resolve an old
    /// execution. This replaces uncertainty with evidence, never re-executes.
    pub(crate) fn verify_core_apply_request(&self, id: Uuid, plan_id: Uuid) -> Result<()> {
        let record = self
            .read_core_request(id)?
            .ok_or(Error::Invalid("CORE_REQUEST_NOT_FOUND"))?;
        let expected: [u8; 32] =
            Sha256::digest(store::encode(&CoreAction::ApplyUpdate { plan_id }, LIMIT)?).into();
        if record.digest != expected || !matches!(record.phase, CoreRequestPhase::Pending { .. }) {
            return Err(Error::Invalid("CORE_UPDATE_REQUEST_MISMATCH"));
        }
        Ok(())
    }

    pub(crate) fn resolve_core_update_receipts(
        &mut self,
        id: Uuid,
        plan_id: Uuid,
        action: &CoreAction,
        outcome: CoreOutcome,
        exclude: Option<Uuid>,
    ) -> Result<()> {
        let expected: [u8; 32] = Sha256::digest(store::encode(action, LIMIT)?).into();
        let recovery: [u8; 32] = Sha256::digest(store::encode(
            &CoreAction::RecoverUpdate { plan_id },
            LIMIT,
        )?)
        .into();
        // A terminal receipt may have expired. An unresolved receipt never does.
        let original = self.read_core_request(id)?;
        if original
            .as_ref()
            .is_some_and(|record| record.digest != expected)
        {
            return Err(Error::Invalid("CORE_UPDATE_REQUEST_MISMATCH"));
        }
        let mut records = Vec::new();
        for request in self.core_request_ids()? {
            if Some(request) == exclude || request == id {
                continue;
            }
            let record = self
                .read_core_request(request)?
                .ok_or(Error::Invalid("CORE_REQUEST_DISAPPEARED"))?;
            if record.digest == recovery
                && matches!(
                    record.phase,
                    CoreRequestPhase::Pending { .. }
                        | CoreRequestPhase::Complete {
                            outcome: CoreOutcome::Indeterminate { .. },
                            ..
                        }
                )
            {
                records.push(record);
            }
        }
        if let Some(record) = original {
            records.insert(0, record);
        }
        for record in records {
            self.resolve_core_record(record, outcome.clone())?;
        }
        Ok(())
    }

    pub(crate) fn resolve_core_preparation_receipt(
        &mut self,
        id: Uuid,
        action: &CoreAction,
        outcome: CoreOutcome,
    ) -> Result<()> {
        let Some(record) = self.read_core_request(id)? else {
            return Ok(());
        };
        let expected: [u8; 32] = Sha256::digest(store::encode(action, LIMIT)?).into();
        if record.digest != expected {
            return Err(Error::Invalid("CORE_UPDATE_REQUEST_MISMATCH"));
        }
        self.resolve_core_record(record, outcome)
    }

    fn resolve_core_record(&mut self, mut record: Record, outcome: CoreOutcome) -> Result<()> {
        validate_outcome(&outcome, &self.load()?.owner_sid)?;
        match &record.phase {
            CoreRequestPhase::Pending { .. }
            | CoreRequestPhase::Complete {
                outcome: CoreOutcome::Indeterminate { .. },
                ..
            } => {}
            CoreRequestPhase::Complete { outcome: prior, .. }
                if store::encode(prior, LIMIT)? == store::encode(&outcome, LIMIT)? =>
            {
                return Ok(());
            }
            _ => return Err(Error::Invalid("CORE_REQUEST_RESULT_CONFLICT")),
        }
        record.phase = CoreRequestPhase::Complete {
            outcome,
            completed_at: now()?.max(record.accepted_at),
        };
        self.write_core_request(&record)
    }

    pub fn has_unresolved_core_requests(&self) -> Result<bool> {
        for id in self.core_request_ids()? {
            let record = self
                .read_core_request(id)?
                .ok_or(Error::Invalid("CORE_REQUEST_DISAPPEARED"))?;
            if matches!(
                record.phase,
                CoreRequestPhase::Pending { .. }
                    | CoreRequestPhase::Complete {
                        outcome: CoreOutcome::Indeterminate { .. },
                        ..
                    }
            ) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Complete older interrupted reconciliation requests for this exact
    /// generation. Never resolve a recovery admitted for a different core.
    pub fn resolve_core_start_recovery_receipts(
        &mut self,
        generation: Uuid,
        outcome: CoreOutcome,
        exclude: Uuid,
    ) -> Result<()> {
        if !matches!(&outcome, CoreOutcome::Reconciled { generation: observed, .. } if *observed == generation)
        {
            return Err(Error::Invalid("CORE_START_WITNESS_MISMATCH"));
        }
        let action = CoreAction::RecoverStart { generation };
        let expected: [u8; 32] = Sha256::digest(store::encode(&action, LIMIT)?).into();
        for id in self.core_request_ids()? {
            if id == exclude {
                continue;
            }
            let record = self
                .read_core_request(id)?
                .ok_or(Error::Invalid("CORE_REQUEST_DISAPPEARED"))?;
            if record.digest == expected
                && matches!(
                    record.phase,
                    CoreRequestPhase::Pending { .. }
                        | CoreRequestPhase::Complete {
                            outcome: CoreOutcome::Indeterminate { .. },
                            ..
                        }
                )
            {
                self.resolve_core_record(record, outcome.clone())?;
            }
        }
        Ok(())
    }

    fn core_request_directory(&self, create: bool) -> Result<Option<OwnedHandle>> {
        let path = self.root().join("state/core-requests");
        if !path.try_exists()? {
            if !create {
                return Ok(None);
            }
            std::fs::create_dir(&path)?;
        }
        let pin = security::directory(&path, false)?;
        security::verify(pin.as_raw_handle(), &self.load()?.owner_sid, false)?;
        Ok(Some(pin))
    }

    fn read_core_request(&self, id: Uuid) -> Result<Option<Record>> {
        let Some(_pin) = self.core_request_directory(false)? else {
            return Ok(None);
        };
        let path = self.root().join(format!("state/core-requests/{id}.json"));
        if !path.try_exists()? {
            return Ok(None);
        }
        let manifest = self.load()?;
        let record: Record =
            store::decode(&store::read_protected(&path, &manifest.owner_sid, LIMIT)?)?;
        if record.schema_version != 1
            || record.store_id != manifest.store_id
            || record.request_id != id
            || id.is_nil()
            || record.accepted_at == 0
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_RECORD"));
        }
        match &record.phase {
            CoreRequestPhase::Pending { epoch } if epoch.is_nil() => {
                return Err(Error::Invalid("INVALID_CORE_REQUEST_RECORD"));
            }
            CoreRequestPhase::Complete {
                outcome,
                completed_at,
            } => {
                if *completed_at < record.accepted_at {
                    return Err(Error::Invalid("INVALID_CORE_REQUEST_RECORD"));
                }
                validate_outcome(outcome, &manifest.owner_sid)?;
            }
            _ => {}
        }
        Ok(Some(record))
    }

    fn write_core_request(&self, record: &Record) -> Result<()> {
        let _pin = self
            .core_request_directory(false)?
            .ok_or(Error::Invalid("CORE_REQUEST_DIRECTORY_MISSING"))?;
        self.replace_bounded(
            &format!("state/core-requests/{}.json", record.request_id),
            &store::encode(record, LIMIT)?,
            LIMIT,
        )
    }

    fn core_request_ids(&self) -> Result<Vec<Uuid>> {
        let Some(_pin) = self.core_request_directory(false)? else {
            return Ok(Vec::new());
        };
        crate::journal::record_ids(
            &self.root().join("state/core-requests"),
            "UNKNOWN_CORE_REQUEST_FILE",
        )
    }

    fn prune_core_requests(&self) -> Result<()> {
        let at = now()?;
        let _pin = self.core_request_directory(false)?;
        for id in self.core_request_ids()? {
            let record = self
                .read_core_request(id)?
                .ok_or(Error::Invalid("CORE_REQUEST_DISAPPEARED"))?;
            if let CoreRequestPhase::Complete {
                outcome,
                completed_at,
            } = record.phase
                && !matches!(outcome, CoreOutcome::Indeterminate { .. })
                && at
                    .checked_sub(completed_at)
                    .is_some_and(|age| age > RETENTION)
            {
                std::fs::remove_file(self.root().join(format!("state/core-requests/{id}.json")))?;
            }
        }
        Ok(())
    }
}

fn validate_outcome(outcome: &CoreOutcome, owner: &str) -> Result<()> {
    match outcome {
        CoreOutcome::Prepared { impact }
            if impact.plan_id.is_nil()
                || impact.manifest_revision == 0
                || impact.previous_generation.is_nil()
                || impact.changed_profile.is_nil()
                || impact.added_profiles.iter().any(Uuid::is_nil)
                || impact
                    .added_profiles
                    .iter()
                    .any(|p| impact.affected_profiles.contains(p))
                || impact.removed_profiles.iter().any(|p| {
                    p.is_nil()
                        || !impact.affected_profiles.contains(p)
                        || impact.added_profiles.contains(p)
                })
                || impact.affected_profiles.iter().any(Uuid::is_nil)
                || impact.bound_instances.iter().any(Uuid::is_nil)
                || !(impact.affected_profiles.contains(&impact.changed_profile)
                    || impact.added_profiles.contains(&impact.changed_profile)) =>
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::ProfileRemoved {
            profile_id,
            revision,
        } if profile_id.is_nil() || *revision == 0 => {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::Reconciled {
            generation,
            process,
        } if generation.is_nil()
            || process.as_ref().is_some_and(|p| {
                p.pid == 0
                    || p.creation_time == 0
                    || p.user_sid != owner
                    || !p.image_path.is_absolute()
            }) =>
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::Reconfigured { revision: 0, .. } => {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::CancelRequested { request_id } if request_id.is_nil() => {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::Installed { version } if !crate::singbox_binary::valid_version(version) => {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::Ready {
            generation,
            process,
        }
        | CoreOutcome::Reconfigured {
            generation,
            process,
            ..
        } if generation.is_nil()
            || process.pid == 0
            || process.creation_time == 0
            || process.user_sid != owner
            || !process.image_path.is_absolute() =>
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        CoreOutcome::Failed { code } | CoreOutcome::Indeterminate { code }
            if !valid_failure_code(code) =>
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST_OUTCOME"));
        }
        _ => {}
    }
    Ok(())
}

fn valid_failure_code(value: &str) -> bool {
    let mut parts = value.split("; ");
    let code = parts.next().unwrap_or_default();
    if code.is_empty()
        || code.len() > 96
        || !code.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
    {
        return false;
    }
    let details: Vec<_> = parts.collect();
    if details.is_empty() {
        return true;
    }
    if details.len() != 6 || value.len() > 512 {
        return false;
    }
    let Some(phase) = details[0].strip_prefix("phase=") else {
        return false;
    };
    if !matches!(
        phase,
        "checking_existing" | "downloading" | "verifying" | "checking_binary" | "publishing"
    ) {
        return false;
    }
    for (field, prefix) in [(details[1], "operation="), (details[3], "io_kind=")] {
        let Some(name) = field.strip_prefix(prefix) else {
            return false;
        };
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'(' | b')'))
        {
            return false;
        }
    }
    let Some(win32) = details[2].strip_prefix("win32=") else {
        return false;
    };
    if win32 != "none" && win32.parse::<u32>().is_err() {
        return false;
    }
    [
        (details[4], "downloaded_bytes="),
        (details[5], "elapsed_ms="),
    ]
    .iter()
    .all(|(field, prefix)| {
        field.strip_prefix(prefix).is_some_and(|v| {
            !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && v.parse::<u128>().is_ok()
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_core::{
        model::*,
        registry::{ConfigAction, ConfigRequest},
    };

    #[test]
    fn installation_diagnostics_remain_bounded_and_reject_arbitrary_text() {
        let valid = "STORE_ACCESS_DENIED; phase=checking_existing; operation=GetTokenInformation(elevation); win32=5; io_kind=PermissionDenied; downloaded_bytes=0; elapsed_ms=10";
        assert!(valid_failure_code(valid));
        assert!(valid_failure_code("CORE_INSTALL_IO_FAILED"));
        for invalid in [
            valid.replace("checking_existing", "unknown"),
            valid.replace("win32=5", "win32=broken"),
            valid.replace("elapsed_ms=10", "elapsed_ms=-1"),
            valid.replace(
                "GetTokenInformation(elevation)",
                "https://private?token=secret",
            ),
            format!("{valid}\nsecret"),
            format!("{valid}; extra=secret"),
        ] {
            assert!(!valid_failure_code(&invalid));
        }
    }

    fn config(id: Uuid) -> ConfigRequest {
        ConfigRequest {
            request_id: id,
            expected_revision: 1,
            action: ConfigAction::AddApplication {
                application: Application {
                    id: Uuid::new_v4(),
                    revision: 1,
                    name: "fixture".into(),
                    locator: ApplicationLocator::Exe {
                        path: "C:\\fixture.exe".into(),
                    },
                    template_ref: Template::Codex,
                },
            },
        }
    }

    #[test]
    fn pending_survives_reopen_and_canonical_payloads_deduplicate() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let action = CoreAction::Start {
            profiles: vec![first, second, first],
            required: first,
        };
        assert!(store.begin_core_request(id, epoch, &action).unwrap().1);
        drop(store);
        let mut store = Store::open(&root).unwrap();
        let same = CoreAction::Start {
            profiles: vec![second, first],
            required: first,
        };
        assert!(
            !store
                .begin_core_request(id, Uuid::new_v4(), &same)
                .unwrap()
                .1
        );
        assert!(matches!(
            store.begin_core_request(id, epoch, &CoreAction::Stop {}),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        assert!(matches!(
            store.finish_core_request(id, Uuid::new_v4(), CoreOutcome::Stopped {}),
            Err(Error::Invalid("CORE_REQUEST_OWNER_CHANGED"))
        ));
        assert!(store.has_unresolved_core_requests().unwrap());
        store
            .finish_core_request(
                id,
                epoch,
                CoreOutcome::Failed {
                    code: "CORE_BINARY_MISSING".into(),
                },
            )
            .unwrap();
        assert!(!store.has_unresolved_core_requests().unwrap());
        assert!(!store.begin_core_request(id, epoch, &same).unwrap().1);
    }

    #[test]
    fn clock_rollback_keeps_receipts_readable_and_does_not_block_new_requests() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        store
            .begin_core_request(id, epoch, &CoreAction::Stop {})
            .unwrap();
        let mut record = store.read_core_request(id).unwrap().unwrap();
        record.accepted_at = now().unwrap() + 3600;
        store.write_core_request(&record).unwrap();
        store
            .finish_core_request(id, epoch, CoreOutcome::Stopped {})
            .unwrap();
        assert!(
            matches!(store.core_request_status(id).unwrap(), Some(CoreRequestPhase::Complete { completed_at, .. }) if completed_at == record.accepted_at)
        );
        assert!(
            store
                .begin_core_request(Uuid::new_v4(), epoch, &CoreAction::Stop {})
                .unwrap()
                .1
        );
    }

    #[test]
    fn request_ids_cannot_cross_operation_namespaces() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let id = Uuid::new_v4();
        let epoch = Uuid::new_v4();
        store
            .begin_core_request(id, epoch, &CoreAction::Stop {})
            .unwrap();
        assert!(matches!(
            store.apply_config(&config(id)),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        let other = Uuid::new_v4();
        store.apply_config(&config(other)).unwrap();
        assert!(matches!(
            store.begin_core_request(other, epoch, &CoreAction::Stop {}),
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
    }

    #[test]
    fn retention_preserves_unresolved_requests_and_rejects_corruption() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let epoch = Uuid::new_v4();
        let mut ids = Vec::new();
        for terminal in [
            None,
            Some(CoreOutcome::Stopped {}),
            Some(CoreOutcome::Indeterminate {
                code: "CORE_OPERATION_RESULT_UNKNOWN".into(),
            }),
        ] {
            let id = Uuid::new_v4();
            store
                .begin_core_request(id, epoch, &CoreAction::Stop {})
                .unwrap();
            let mut record = store.read_core_request(id).unwrap().unwrap();
            record.accepted_at = now().unwrap() - RETENTION - 10;
            if let Some(outcome) = terminal {
                record.phase = CoreRequestPhase::Complete {
                    outcome,
                    completed_at: record.accepted_at,
                };
            }
            store.write_core_request(&record).unwrap();
            ids.push(id);
        }
        store.prune_core_requests().unwrap();
        assert!(store.core_request_status(ids[0]).unwrap().is_some());
        assert!(store.core_request_status(ids[1]).unwrap().is_none());
        assert!(store.core_request_status(ids[2]).unwrap().is_some());
        let mut record = store.read_core_request(ids[0]).unwrap().unwrap();
        record.store_id = Uuid::new_v4();
        store.write_core_request(&record).unwrap();
        assert!(store.core_request_status(ids[0]).is_err());
        assert!(
            store
                .begin_core_request(Uuid::new_v4(), epoch, &CoreAction::Stop {})
                .is_err()
        );
    }
}
