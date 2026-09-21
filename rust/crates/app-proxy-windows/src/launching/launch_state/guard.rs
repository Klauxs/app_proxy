use super::*;
use crate::{identity, instance_resource::ResourceOwner, process_stop::StopOutcome};
use app_proxy_core::model::{Desired, NetworkBinding};

/// One-use stop intent; deserializing a journal can never reconstruct this token.
pub struct GuardStopDispatch {
    pub(crate) owner: ResourceOwner,
    pub(crate) nonce: Uuid,
    pub(crate) target: GuardTarget,
    _owner: std::sync::Arc<std::fs::File>,
}

pub struct GuardStopReceipt {
    pub(crate) owner: ResourceOwner,
    pub(crate) nonce: Uuid,
    pub(crate) target: GuardTarget,
    pub(crate) outcome: StopOutcome,
}
impl GuardStopReceipt {
    pub fn outcome(&self) -> StopOutcome {
        self.outcome
    }
}

impl Store {
    pub fn dispatch_guard_stop(&mut self, id: Uuid, epoch: Uuid) -> Result<GuardStopDispatch> {
        self.recover_config_requests()?;
        let manifest = self.load()?;
        let mut journal = self.read_launch_journal()?;
        let phase = journal
            .attempts
            .iter()
            .find(|a| a.id == id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?
            .phase
            .clone();
        if !matches!(
            phase,
            LaunchPhase::Accepted {} | LaunchPhase::CheckingInstance {}
        ) {
            return Err(Error::Invalid("INVALID_LAUNCH_TRANSITION"));
        }
        let attempt = attempt_mut(&mut journal, id, epoch, &phase)?;
        if attempt.cancel_requested {
            return Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"));
        }
        if attempt.expected_revision != Some(manifest.revision) {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        let correction = attempt
            .guard_correction
            .as_mut()
            .ok_or(Error::Invalid("GUARD_CORRECTION_REQUIRED"))?;
        if correction.stop_nonce.is_some() {
            return Err(Error::Invalid("GUARD_STOP_ALREADY_DISPATCHED"));
        }
        let instance = manifest
            .instances
            .iter()
            .find(|i| i.id == attempt.instance_id)
            .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
        let application = manifest
            .applications
            .iter()
            .find(|a| a.id == instance.application_id)
            .ok_or(Error::Invalid("APPLICATION_NOT_FOUND"))?;
        let caller = identity::current()?;
        if instance.guard.desired != Desired::Enabled
            || !application.template_ref.supports_guard()
            || correction.target.process.user_sid != caller.user_sid
            || correction.target.process.session_id != caller.session_id
            || !manifest.profiles.iter().any(|p| {
                instance.network == NetworkBinding::Profile { profile_id: p.id }
                    && p.endpoint == correction.target.endpoint
            })
        {
            return Err(Error::Invalid("GUARD_CONFIG_CHANGED"));
        }
        let at = now()?;
        if at < attempt.accepted_at {
            return Err(Error::Invalid("GUARD_CLOCK_ROLLBACK"));
        }
        let nonce = Uuid::new_v4();
        correction.stop_nonce = Some(nonce);
        correction.stop_started_at = Some(at);
        let target = correction.target.clone();
        // Event preparation has no external creation side effects. Persist its
        // completion together with the one-use stop intent, before termination.
        attempt.phase = LaunchPhase::CheckingInstance {};
        self.write_launch_journal(&journal)?;
        Ok(GuardStopDispatch {
            owner: ResourceOwner {
                store_id: manifest.store_id,
                attempt_id: id,
                epoch,
            },
            nonce,
            target,
            _owner: self.owner_lease(),
        })
    }

    /// Accept only a receipt issued after exact native exit confirmation. This
    /// can arrive after a worker timeout; it never authorizes a late relaunch.
    pub fn confirm_guard_stop(&mut self, receipt: &GuardStopReceipt) -> Result<()> {
        let mut journal = self.read_launch_journal()?;
        if journal.store_id != receipt.owner.store_id
            || !matches!(receipt.outcome, StopOutcome::Exited | StopOutcome::Forced)
        {
            return Err(Error::Invalid("GUARD_STOP_RECEIPT_MISMATCH"));
        }
        let attempt = journal
            .attempts
            .iter_mut()
            .find(|a| a.id == receipt.owner.attempt_id)
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        let correction = attempt
            .guard_correction
            .as_mut()
            .ok_or(Error::Invalid("GUARD_CORRECTION_REQUIRED"))?;
        if attempt.epoch != receipt.owner.epoch
            || correction.stop_nonce != Some(receipt.nonce)
            || correction.target != receipt.target
        {
            return Err(Error::Invalid("GUARD_STOP_RECEIPT_MISMATCH"));
        }
        correction.stop_confirmed = true;
        self.write_launch_journal(&journal)
    }
}

pub(super) fn validate(attempt: &LaunchAttempt, sid: &str) -> Result<()> {
    let Some(g) = &attempt.guard_correction else {
        return Ok(());
    };
    let p = &g.target.process;
    if attempt.origin != LaunchOrigin::Guard
        || attempt.expected_revision.is_none()
        || p.pid == 0
        || p.creation_time == 0
        || p.user_sid != sid
        || !p.image_path.is_absolute()
        || p.image_path.to_str().is_none_or(|p| p.contains('\0'))
        || !g.target.endpoint.host.is_loopback()
        || g.target.endpoint.port == 0
        || g.stop_nonce.is_some_and(|n| n.is_nil())
        || g.stop_nonce.is_some() != g.stop_started_at.is_some()
        || g.stop_started_at.is_some_and(|at| at < attempt.accepted_at)
        || (g.stop_confirmed && g.stop_nonce.is_none())
        || (matches!(
            attempt.phase,
            LaunchPhase::PreparingProxy {}
                | LaunchPhase::PreparingData {}
                | LaunchPhase::ReadyToSpawn {}
                | LaunchPhase::SpawnRequested {}
                | LaunchPhase::AwaitingIdentity {}
                | LaunchPhase::Indeterminate {}
                | LaunchPhase::Confirmed { .. }
        ) && !g.stop_confirmed)
    {
        return Err(Error::Invalid("INVALID_GUARD_CORRECTION"));
    }
    Ok(())
}
