//! One bounded atomic journal keeps request aliases, attempts and confirmed
//! sessions consistent. Opening or reading it never replays process creation.
use crate::{
    Error, Result, process,
    store::{self, Store},
};
use app_proxy_core::launch::*;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    time::{SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const PATH: &str = "state/launch.json";
const LIMIT: usize = 8 * 1024 * 1024;
const REQUEST_LIMIT: usize = 4096;
const RETENTION: u64 = 7 * 24 * 60 * 60;

mod guard;
#[cfg(test)]
mod tests;
pub use guard::{GuardStopDispatch, GuardStopReceipt};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guard_target: Option<GuardTarget>,
    request: LaunchRequest,
    attempt_id: Uuid,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    schema_version: u32,
    store_id: Uuid,
    requests: Vec<RequestEntry>,
    attempts: Vec<LaunchAttempt>,
}

pub struct LaunchAdmission {
    pub attempt: LaunchAttempt,
    /// Only a new admission authorizes the caller to execute preparation.
    pub is_new: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct DispatchIdentity {
    pub owner: crate::instance_resource::ResourceOwner,
    pub dispatch_id: Uuid,
}

/// Issued once by the atomic ReadyToSpawn -> SpawnRequested transition; never
/// reconstructed from a request ID. Retains store ownership through creation.
pub struct LaunchDispatch {
    context: DispatchIdentity,
    binding: LaunchBinding,
    _owner: std::sync::Arc<std::fs::File>,
    _package: Option<crate::instance_data::PackageControlRoot>,
    package_request: Option<std::path::PathBuf>,
}
impl LaunchDispatch {
    pub(crate) fn context(&self) -> DispatchIdentity {
        self.context
    }
    pub(crate) fn binding(&self) -> &LaunchBinding {
        &self.binding
    }
    pub(crate) fn package_request(&self) -> Option<&std::path::Path> {
        self.package_request.as_deref()
    }
}

impl Store {
    pub fn launch_request(&self, id: Uuid) -> Result<Option<LaunchAttempt>> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let journal = self.read_launch_journal()?;
        Ok(journal
            .requests
            .iter()
            .find(|r| r.request.request_id == id)
            .and_then(|r| journal.attempts.iter().find(|a| a.id == r.attempt_id))
            .cloned())
    }

    pub fn launch_attempts(&self) -> Result<Vec<LaunchAttempt>> {
        Ok(self.read_launch_journal()?.attempts)
    }

    pub fn begin_launch(
        &mut self,
        request: &LaunchRequest,
        epoch: Uuid,
    ) -> Result<LaunchAdmission> {
        self.begin_launch_at_revision(request, epoch, None)
    }

    pub fn begin_launch_at_revision(
        &mut self,
        request: &LaunchRequest,
        epoch: Uuid,
        expected_revision: Option<u64>,
    ) -> Result<LaunchAdmission> {
        self.begin_launch_checked(request, epoch, expected_revision, None)
    }

    pub fn begin_guard_launch(
        &mut self,
        request: &LaunchRequest,
        epoch: Uuid,
        revision: u64,
        target: GuardTarget,
    ) -> Result<LaunchAdmission> {
        if request.origin != LaunchOrigin::Guard {
            return Err(Error::Invalid("INVALID_GUARD_REQUEST"));
        }
        self.begin_launch_checked(request, epoch, Some(revision), Some(target))
    }

    fn begin_launch_checked(
        &mut self,
        request: &LaunchRequest,
        epoch: Uuid,
        expected_revision: Option<u64>,
        guard_target: Option<GuardTarget>,
    ) -> Result<LaunchAdmission> {
        if request.request_id.is_nil() || request.instance_id.is_nil() || epoch.is_nil() {
            return Err(Error::Invalid("INVALID_LAUNCH_REQUEST"));
        }
        if self.core_request_status(request.request_id)?.is_some()
            || self.config_request_status(request.request_id)?.is_some()
            || self.shortcut_request_status(request.request_id)?.is_some()
        {
            return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
        }
        let mut journal = self.read_launch_journal()?;
        if let Some(entry) = journal
            .requests
            .iter()
            .find(|r| r.request.request_id == request.request_id)
        {
            if entry.request != *request || entry.guard_target != guard_target {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
            return Ok(LaunchAdmission {
                attempt: journal
                    .attempts
                    .iter()
                    .find(|a| a.id == entry.attempt_id)
                    .unwrap()
                    .clone(),
                is_new: false,
            });
        }
        let revision = self.load()?.revision;
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        if !self
            .load()?
            .instances
            .iter()
            .any(|i| i.id == request.instance_id)
        {
            return Err(Error::Invalid("INSTANCE_NOT_FOUND"));
        }
        let now = now()?;
        journal.attempts.retain(|a| {
            a.resource_pending
                || a.reserves_instance()
                || a.finished_at
                    .is_none_or(|at| at.saturating_add(RETENTION) > now)
        });
        journal
            .requests
            .retain(|r| journal.attempts.iter().any(|a| a.id == r.attempt_id));
        if journal.requests.len() >= REQUEST_LIMIT {
            return Err(Error::Invalid("LAUNCH_REQUEST_LIMIT"));
        }
        let existing = journal
            .attempts
            .iter()
            .find(|a| a.instance_id == request.instance_id && a.reserves_instance());
        let is_new = existing.is_none();
        if guard_target.is_some() && existing.is_some() {
            return Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"));
        }
        if expected_revision.is_some()
            && existing.is_some_and(|a| {
                !matches!(a.phase, LaunchPhase::Confirmed { .. })
                    && a.expected_revision != expected_revision
            })
        {
            return Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"));
        }
        let attempt = existing.cloned().unwrap_or(LaunchAttempt {
            guard_correction: guard_target.clone().map(|target| {
                Box::new(GuardCorrection {
                    target,
                    stop_started_at: None,
                    stop_nonce: None,
                    stop_confirmed: false,
                })
            }),
            package_request: None,
            id: request.request_id,
            instance_id: request.instance_id,
            origin: request.origin,
            epoch,
            phase: LaunchPhase::Accepted {},
            accepted_at: now,
            finished_at: None,
            cancel_requested: false,
            binding: None,
            dispatch_id: None,
            session_exited: false,
            resource_pending: false,
            expected_revision,
        });
        if is_new {
            journal.attempts.push(attempt.clone());
        }
        journal.requests.push(RequestEntry {
            guard_target,
            request: request.clone(),
            attempt_id: attempt.id,
        });
        self.write_launch_journal(&journal)?;
        Ok(LaunchAdmission { attempt, is_new })
    }

    /// A cancellation is intent, not proof that no application was created.
    pub fn request_launch_cancel(&mut self, request: Uuid) -> Result<LaunchAttempt> {
        let mut journal = self.read_launch_journal()?;
        let id = journal
            .requests
            .iter()
            .find(|r| r.request.request_id == request)
            .ok_or(Error::Invalid("LAUNCH_REQUEST_NOT_FOUND"))?
            .attempt_id;
        let attempt = journal.attempts.iter_mut().find(|a| a.id == id).unwrap();
        if attempt.finished_at.is_none() {
            attempt.cancel_requested = true;
        }
        let result = attempt.clone();
        self.write_launch_journal(&journal)?;
        Ok(result)
    }

    pub fn advance_launch(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        expected: &LaunchPhase,
        next: LaunchPhase,
    ) -> Result<LaunchAttempt> {
        let mut journal = self.read_launch_journal()?;
        let attempt = attempt_mut(&mut journal, id, epoch, expected)?;
        let normal = matches!(
            (&attempt.phase, &next),
            (LaunchPhase::Accepted {}, LaunchPhase::Resolving {})
                | (LaunchPhase::Resolving {}, LaunchPhase::CheckingInstance {})
                | (
                    LaunchPhase::CheckingInstance {},
                    LaunchPhase::PreparingProxy {}
                )
                | (
                    LaunchPhase::PreparingProxy {},
                    LaunchPhase::PreparingData {}
                )
                | (
                    LaunchPhase::SpawnRequested {},
                    LaunchPhase::AwaitingIdentity {}
                )
                | (
                    LaunchPhase::SpawnRequested {} | LaunchPhase::AwaitingIdentity {},
                    LaunchPhase::Indeterminate {}
                )
        );
        let before_spawn_end = attempt.phase.before_spawn()
            && matches!(next, LaunchPhase::Failed { .. } | LaunchPhase::Cancelled {});
        if !normal && !before_spawn_end {
            return Err(Error::Invalid("INVALID_LAUNCH_TRANSITION"));
        }
        if attempt.cancel_requested
            && attempt.phase.before_spawn()
            && !matches!(next, LaunchPhase::Cancelled {} | LaunchPhase::Failed { .. })
        {
            return Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"));
        }
        if before_spawn_end {
            attempt.finished_at = Some(now()?.max(attempt.accepted_at));
            if matches!(&next, LaunchPhase::Failed { code } if code == "INSTANCE_RESOURCE_RELEASE_FAILED")
            {
                attempt.resource_pending = true;
            }
        }
        attempt.phase = next;
        let result = attempt.clone();
        self.write_launch_journal(&journal)?;
        Ok(result)
    }

    pub fn dispatch_launch(&mut self, id: Uuid, epoch: Uuid) -> Result<LaunchDispatch> {
        self.dispatch_launch_kind(id, epoch, None)
    }

    pub fn dispatch_package_launch(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        package: &crate::package::Package,
    ) -> Result<LaunchDispatch> {
        let package = self.prepare_package_control(package)?;
        self.dispatch_launch_kind(id, epoch, Some(package))
    }

    fn dispatch_launch_kind(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        package: Option<crate::instance_data::PackageControlRoot>,
    ) -> Result<LaunchDispatch> {
        let mut journal = self.read_launch_journal()?;
        let store_id = journal.store_id;
        let attempt = attempt_mut(&mut journal, id, epoch, &LaunchPhase::ReadyToSpawn {})?;
        if attempt.cancel_requested {
            return Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"));
        }
        let revision = self.load()?.revision;
        if attempt
            .expected_revision
            .is_some_and(|expected| expected != revision)
        {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        let dispatch_id = Uuid::new_v4();
        attempt.dispatch_id = Some(dispatch_id);
        let package_request = package
            .as_ref()
            .map(|p| p.root.join(format!("package-{id}")));
        attempt.package_request = package_request.clone();
        attempt.resource_pending = true;
        attempt.phase = LaunchPhase::SpawnRequested {};
        let binding = attempt.binding.clone().expect("validated ready binding");
        self.write_launch_journal(&journal)?;
        Ok(LaunchDispatch {
            context: DispatchIdentity {
                owner: crate::instance_resource::ResourceOwner {
                    store_id,
                    attempt_id: id,
                    epoch,
                },
                dispatch_id,
            },
            binding,
            _owner: self.owner_lease(),
            _package: package,
            package_request,
        })
    }

    /// The engine has checked dependencies and acquired the physical resource.
    /// Publishing ReadyToSpawn also holds the durable core launch permission.
    pub fn ready_launch(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        binding: LaunchBinding,
    ) -> Result<LaunchAttempt> {
        self.ensure_core_update_idle()?;
        if let LaunchNetwork::Profile {
            profile_id,
            generation,
            ref endpoint,
        } = binding.network
        {
            let active = self.open_core_generation(generation)?;
            if !matches!(self.core_state()?, crate::core_state::CoreState::Running { generation: g, .. } if g == generation)
                || !self.core_generation_is_current(&active)?
                || !active
                    .profiles()
                    .iter()
                    .any(|p| p.id == profile_id && p.endpoint == *endpoint)
            {
                return Err(Error::Invalid("LAUNCH_CORE_CHANGED"));
            }
        }
        let mut journal = self.read_launch_journal()?;
        if journal.attempts.iter().any(|a| {
            a.id != id
                && a.reserves_instance()
                && a.binding
                    .as_ref()
                    .is_some_and(|b| b.resource_key == binding.resource_key)
        }) {
            return Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"));
        }
        let attempt = attempt_mut(&mut journal, id, epoch, &LaunchPhase::PreparingData {})?;
        if attempt.cancel_requested {
            return Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"));
        }
        attempt.binding = Some(binding);
        attempt.phase = LaunchPhase::ReadyToSpawn {};
        let result = attempt.clone();
        self.write_launch_journal(&journal)?;
        Ok(result)
    }

    /// Called with evidence from the exact newly-created handle or a verified
    /// helper receipt. PID-only discovery is never sufficient for this operation.
    pub fn confirm_launch(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        identity: app_proxy_core::ProcessIdentity,
    ) -> Result<LaunchAttempt> {
        let mut journal = self.read_launch_journal()?;
        let attempt = journal
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        if attempt.epoch != epoch
            || !matches!(
                attempt.phase,
                LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
            )
        {
            return Err(Error::Invalid("INVALID_LAUNCH_TRANSITION"));
        }
        attempt.phase = LaunchPhase::Confirmed { process: identity };
        attempt.finished_at = Some(now()?.max(attempt.accepted_at));
        let result = attempt.clone();
        self.write_launch_journal(&journal)?;
        Ok(result)
    }

    /// Only the platform's evidence of no creation can end a dispatched attempt
    /// as failed. A timeout, lost ACK or failed identity read cannot produce it.
    pub fn fail_launch_not_created(
        &mut self,
        id: Uuid,
        epoch: Uuid,
        evidence: &process::NoProcessCreated,
    ) -> Result<LaunchAttempt> {
        let mut journal = self.read_launch_journal()?;
        let attempt = journal
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        if evidence.context().owner
            != (crate::instance_resource::ResourceOwner {
                store_id: journal.store_id,
                attempt_id: id,
                epoch,
            })
            || attempt.dispatch_id != Some(evidence.context().dispatch_id)
            || attempt.epoch != epoch
            || !matches!(
                attempt.phase,
                LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
            )
        {
            return Err(Error::Invalid("LAUNCH_NO_CREATION_EVIDENCE_MISMATCH"));
        }
        attempt.phase = LaunchPhase::Failed {
            code: "APPLICATION_NOT_CREATED".into(),
        };
        attempt.finished_at = Some(now()?.max(attempt.accepted_at));
        let result = attempt.clone();
        self.write_launch_journal(&journal)?;
        Ok(result)
    }

    /// Preparation is safe to abandon because platform creation/helper dispatch
    /// requires a persisted SpawnRequested first. Later uncertainty stays reserved.
    pub fn recover_launches(&mut self, current_epoch: Uuid) -> Result<()> {
        if current_epoch.is_nil() {
            return Err(Error::Invalid("INVALID_LAUNCH_REQUEST"));
        }
        let mut journal = self.read_launch_journal()?;
        let mut changed = false;
        for attempt in &mut journal.attempts {
            if attempt.epoch == current_epoch || attempt.finished_at.is_some() {
                continue;
            }
            if attempt.phase.before_spawn() {
                attempt.phase = LaunchPhase::Failed {
                    code: if attempt
                        .guard_correction
                        .as_ref()
                        .is_some_and(|g| g.stop_started_at.is_some())
                    {
                        "GUARD_CORRECTION_INTERRUPTED"
                    } else {
                        "LAUNCH_INTERRUPTED_BEFORE_SPAWN"
                    }
                    .into(),
                };
                attempt.finished_at = Some(now()?.max(attempt.accepted_at));
                changed = true;
            } else if !matches!(attempt.phase, LaunchPhase::Indeterminate {}) {
                attempt.phase = LaunchPhase::Indeterminate {};
                changed = true;
            }
        }
        if changed {
            self.write_launch_journal(&journal)?;
        }
        Ok(())
    }

    /// Read only: the identity comes from a validated protected receipt, never
    /// from caller input. Unlike observe_launch_exit, this does not edit it.
    pub fn inspect_launch_process(
        &self,
        id: Uuid,
    ) -> Result<Option<app_proxy_core::ProcessIdentity>> {
        let journal = self.read_launch_journal()?;
        let attempt = journal
            .attempts
            .iter()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        let LaunchPhase::Confirmed { process } = &attempt.phase else {
            return Err(Error::Invalid("LAUNCH_NOT_CONFIRMED"));
        };
        if attempt.session_exited || !process::is_recorded_process_running(process)? {
            Ok(None)
        } else {
            Ok(Some(process.clone()))
        }
    }

    pub fn observe_launch_exit(&mut self, id: Uuid) -> Result<bool> {
        let mut journal = self.read_launch_journal()?;
        let attempt = journal
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        let LaunchPhase::Confirmed { ref process } = attempt.phase else {
            return Err(Error::Invalid("LAUNCH_NOT_CONFIRMED"));
        };
        if attempt.session_exited {
            return Ok(true);
        }
        if process::is_recorded_process_running(process)? {
            return Ok(false);
        }
        attempt.session_exited = true;
        attempt.finished_at = Some(now()?.max(attempt.finished_at.unwrap()));
        self.write_launch_journal(&journal)?;
        Ok(true)
    }

    pub(crate) fn finish_resource_sync(&mut self, id: Uuid) -> Result<()> {
        let mut journal = self.read_launch_journal()?;
        let attempt = journal
            .attempts
            .iter_mut()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        if !matches!(
            attempt.phase,
            LaunchPhase::Confirmed { .. } | LaunchPhase::Failed { .. }
        ) {
            return Err(Error::Invalid("INVALID_LAUNCH_TRANSITION"));
        }
        if attempt.resource_pending {
            attempt.resource_pending = false;
            self.write_launch_journal(&journal)?;
        }
        Ok(())
    }

    pub fn ensure_core_launch_idle(&self) -> Result<()> {
        if self.read_launch_journal()?.attempts.iter().any(|a| {
            matches!(
                a.phase,
                LaunchPhase::ReadyToSpawn {}
                    | LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
            ) && a
                .binding
                .as_ref()
                .is_some_and(|b| matches!(b.network, LaunchNetwork::Profile { .. }))
        }) {
            return Err(Error::Invalid("CORE_LAUNCH_IN_PROGRESS"));
        }
        Ok(())
    }

    fn read_launch_journal(&self) -> Result<Journal> {
        let header = self.load()?;
        if !self.root().join(PATH).try_exists()? {
            return Ok(Journal {
                schema_version: 1,
                store_id: header.store_id,
                requests: vec![],
                attempts: vec![],
            });
        }
        let journal = store::decode(&store::read_protected(
            &self.root().join(PATH),
            &header.owner_sid,
            LIMIT,
        )?)?;
        self.validate_launch_journal(&journal)?;
        Ok(journal)
    }

    fn write_launch_journal(&self, journal: &Journal) -> Result<()> {
        self.validate_launch_journal(journal)?;
        self.replace_bounded(PATH, &store::encode(journal, LIMIT)?, LIMIT)
    }

    fn validate_launch_journal(&self, journal: &Journal) -> Result<()> {
        let invalid = || Error::Invalid("INVALID_LAUNCH_RECORD");
        let header = self.load()?;
        if journal.schema_version != 1
            || journal.store_id != header.store_id
            || journal.requests.len() > REQUEST_LIMIT
        {
            return Err(invalid());
        }
        let mut requests = HashSet::new();
        let mut ids = HashSet::new();
        let mut instances = HashSet::new();
        let mut resources = HashSet::new();
        for entry in &journal.requests {
            if entry.request.request_id.is_nil()
                || !requests.insert(entry.request.request_id)
                || !journal
                    .attempts
                    .iter()
                    .any(|a| a.id == entry.attempt_id && a.instance_id == entry.request.instance_id)
            {
                return Err(invalid());
            }
        }
        for a in &journal.attempts {
            guard::validate(a, &header.owner_sid)?;
            if journal
                .requests
                .iter()
                .find(|r| r.request.request_id == a.id)
                .is_none_or(|r| {
                    r.guard_target.as_ref() != a.guard_correction.as_ref().map(|g| &g.target)
                })
            {
                return Err(invalid());
            }
            if a.dispatch_id.is_some_and(|id| id.is_nil())
                || (a.phase.before_spawn() && a.dispatch_id.is_some())
                || (a.package_request.is_some() && a.dispatch_id.is_none())
                || a.package_request.as_ref().is_some_and(|path| {
                    !path.is_absolute()
                        || path.file_name()
                            != Some(std::ffi::OsStr::new(&format!("package-{}", a.id)))
                        || path.to_str().is_none_or(|s| s.contains('\0'))
                })
            {
                return Err(invalid());
            }
            let terminal = matches!(
                a.phase,
                LaunchPhase::Confirmed { .. }
                    | LaunchPhase::Failed { .. }
                    | LaunchPhase::Cancelled {}
            );
            if a.id.is_nil()
                || a.instance_id.is_nil()
                || a.epoch.is_nil()
                || a.accepted_at == 0
                || !ids.insert(a.id)
                || a.finished_at.is_some() != terminal
                || a.finished_at.is_some_and(|at| at < a.accepted_at)
                || (a.session_exited && !matches!(a.phase, LaunchPhase::Confirmed { .. }))
                || a.expected_revision == Some(0)
                || (a.resource_pending
                    && a.dispatch_id.is_none()
                    && !matches!(&a.phase, LaunchPhase::Failed { code } if code == "INSTANCE_RESOURCE_RELEASE_FAILED"))
                || !journal.requests.iter().any(|r| {
                    r.request.request_id == a.id
                        && r.attempt_id == a.id
                        && r.request.origin == a.origin
                })
                || (a.reserves_instance() && !instances.insert(a.instance_id))
            {
                return Err(invalid());
            }
            if let LaunchPhase::Failed { code } = &a.phase
                && (code.is_empty()
                    || code.len() > 128
                    || !code
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'))
            {
                return Err(invalid());
            }
            let needs_binding = matches!(
                a.phase,
                LaunchPhase::ReadyToSpawn {}
                    | LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
                    | LaunchPhase::Confirmed { .. }
            );
            if needs_binding && a.binding.is_none() {
                return Err(invalid());
            }
            if let Some(b) = &a.binding {
                if b.dependency_digest == [0; 32]
                    || b.resource_key == [0; 32]
                    || !b.executable.is_absolute()
                    || b.executable.to_str().is_none_or(|s| s.contains('\0'))
                    || (a.reserves_instance() && !resources.insert(b.resource_key))
                {
                    return Err(invalid());
                }
                if let LaunchNetwork::Profile {
                    profile_id,
                    generation,
                    endpoint,
                } = &b.network
                    && (profile_id.is_nil()
                        || generation.is_nil()
                        || !endpoint.host.is_loopback()
                        || endpoint.port == 0)
                {
                    return Err(invalid());
                }
                if let LaunchPhase::Confirmed { process } = &a.phase
                    // Resolved paths may have a verbatim prefix while the process
                    // API returns a DOS path. Physical image identity is decisive.
                    && (process.pid == 0
                        || process.creation_time == 0
                        || process.user_sid != header.owner_sid
                        || process.session_id != b.session_id
                        || process.image_file != b.image
                        || !process.image_path.is_absolute()
                        || process.image_path.to_str().is_none_or(|p| p.contains('\0')))
                {
                    return Err(invalid());
                }
            }
        }
        Ok(())
    }
}

fn attempt_mut<'a>(
    journal: &'a mut Journal,
    id: Uuid,
    epoch: Uuid,
    phase: &LaunchPhase,
) -> Result<&'a mut LaunchAttempt> {
    let attempt = journal
        .attempts
        .iter_mut()
        .find(|a| a.id == id)
        .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
    if attempt.epoch != epoch || attempt.phase != *phase {
        return Err(Error::Invalid("LAUNCH_STATE_CHANGED"));
    }
    Ok(attempt)
}
fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::Invalid("CLOCK_BEFORE_EPOCH"))
}
