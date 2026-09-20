//! A checked, immutable proposal is distinct from permission to stop a core.
//! One active switch per store. Interrupted side effects require reconciliation;
//! opening the store never spawns or terminates a process on its own.
use crate::{
    Error, Result,
    core_state::CoreState,
    store::{self, Store},
};
pub use app_proxy_core::core_control::UpdateImpact;
use app_proxy_core::{
    model::{MANIFEST_LIMIT, Manifest, NetworkBinding},
    registry::{self, ConfigAction, ConfigRequest},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const LIMIT: usize = 2 * MANIFEST_LIMIT + 16384;
const PATH: &str = "state/core/update.json";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdatePhase {
    Prepared {},
    Switching {},
    Committing {},
    Committed {},
    Restoring {},
    Restored { core_down: bool },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdateChange {
    EditProfile {},
    RemoveProfile {},
    Expand { profiles: Vec<Uuid> },
    RefreshSubscription {},
    SelectSubscriptionNode {},
}

impl Default for UpdateChange {
    fn default() -> Self {
        Self::EditProfile {}
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreUpdate {
    schema_version: u32,
    store_id: Uuid,
    pub plan_id: Uuid,
    pub profile_id: Uuid,
    #[serde(default)]
    pub change: UpdateChange,
    pub before: Manifest,
    pub after: Manifest,
    pub previous: CoreState,
    pub candidate: Uuid,
    pub phase: UpdatePhase,
    pub execution_request: Option<Uuid>,
    pub result: Option<app_proxy_core::core_control::CoreOutcome>,
}

impl CoreUpdate {
    pub fn removes_last(&self) -> bool {
        matches!(self.change, UpdateChange::RemoveProfile {})
            && self.old_generation().ok() == Some(self.candidate)
    }
    pub fn old_generation(&self) -> Result<Uuid> {
        match self.previous {
            CoreState::Running { generation, .. } => Ok(generation),
            _ => Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD")),
        }
    }
    pub fn active(&self) -> bool {
        matches!(
            self.phase,
            UpdatePhase::Switching {} | UpdatePhase::Committing {} | UpdatePhase::Restoring {}
        )
    }
}

impl Store {
    pub fn ensure_core_update_idle(&self) -> Result<()> {
        if self.core_update()?.is_some_and(|p| p.active()) {
            return Err(Error::Invalid("CORE_RECONFIGURATION_PENDING"));
        }
        Ok(())
    }

    pub fn core_update(&self) -> Result<Option<CoreUpdate>> {
        if !self.root().join(PATH).try_exists()? {
            return Ok(None);
        }
        let _pins = self.core_directories(false)?;
        let header = self.load()?;
        let record: CoreUpdate = store::decode(&store::read_protected(
            &self.root().join(PATH),
            &header.owner_sid,
            LIMIT,
        )?)?;
        self.validate_core_update(&record)?;
        Ok(Some(record))
    }

    pub fn prepare_core_update(&mut self, request: &ConfigRequest) -> Result<CoreUpdate> {
        self.ensure_core_update_idle()?;
        self.recover_config_requests()?;
        let (profile_id, change) = match &request.action {
            ConfigAction::RemoveProfile { profile_id } => {
                (*profile_id, UpdateChange::RemoveProfile {})
            }
            ConfigAction::UpdateManualProfile { profile_id, .. } => {
                (*profile_id, UpdateChange::EditProfile {})
            }
            ConfigAction::EditSubscriptionProfile { profile_id, edit } => (
                *profile_id,
                match edit {
                    registry::SubscriptionEdit::Refresh { .. } => {
                        UpdateChange::RefreshSubscription {}
                    }
                    registry::SubscriptionEdit::Select { .. } => {
                        UpdateChange::SelectSubscriptionNode {}
                    }
                },
            ),
            _ => return Err(Error::Invalid("INVALID_CORE_UPDATE_ACTION")),
        };
        let before = self.load()?;
        let previous = self.core_state()?;
        let CoreState::Running { generation, .. } = previous else {
            return Err(Error::Invalid("CORE_UPDATE_REQUIRES_RUNNING"));
        };
        let active = self.open_core_generation(generation)?;
        if !active.profiles().iter().any(|p| p.id == profile_id) {
            return Err(Error::Invalid("PROFILE_NOT_ACTIVE"));
        }
        if !self.core_generation_is_current(&active)? {
            return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
        }
        let (mut after, receipt) =
            registry::apply(self.load()?, request).map_err(|e| Error::Invalid(e.0))?;
        after.revision = receipt.revision;
        self.stage_proxy_secret(request)?;
        let ids: Vec<_> = active
            .profiles()
            .iter()
            .map(|p| p.id)
            .filter(|id| !matches!(change, UpdateChange::RemoveProfile {}) || *id != profile_id)
            .collect();
        // Last-route removal retains the old generation solely for rollback.
        let candidate_id = if ids.is_empty() {
            generation
        } else {
            self.prepare_core_generation_for(&after, &ids)?.id()
        };
        Ok(CoreUpdate {
            schema_version: 2,
            store_id: before.store_id,
            plan_id: request.request_id,
            profile_id,
            change,
            before,
            after,
            previous,
            candidate: candidate_id,
            phase: UpdatePhase::Prepared {},
            execution_request: None,
            result: None,
        })
    }

    pub fn prepare_core_expansion(
        &mut self,
        id: Uuid,
        expected_revision: u64,
        profiles: &[Uuid],
        required: Uuid,
    ) -> Result<CoreUpdate> {
        use app_proxy_core::core_control::CoreAction;
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let mut action = CoreAction::PrepareExpand {
            expected_revision,
            profiles: profiles.to_vec(),
            required,
        };
        action.normalize().map_err(|e| Error::Invalid(e.0))?;
        let CoreAction::PrepareExpand { profiles, .. } = action else {
            unreachable!()
        };
        self.ensure_core_update_idle()?;
        self.recover_config_requests()?;
        let before = self.load()?;
        if before.revision != expected_revision {
            return Err(Error::Invalid("STALE_MANIFEST_REVISION"));
        }
        let previous = self.core_state()?;
        let CoreState::Running { generation, .. } = previous else {
            return Err(Error::Invalid("CORE_UPDATE_REQUIRES_RUNNING"));
        };
        let old = self.open_core_generation(generation)?;
        if !self.core_generation_is_current(&old)? {
            return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
        }
        let mut combined: Vec<_> = old
            .profiles()
            .iter()
            .map(|p| p.id)
            .chain(profiles.iter().copied())
            .collect();
        combined.sort();
        combined.dedup();
        if combined.len() == old.profiles().len() {
            return Err(Error::Invalid("CORE_PROFILE_SET_UNCHANGED"));
        }
        if combined.len() > 1024 {
            return Err(Error::Invalid("CORE_PROFILE_LIMIT"));
        }
        let candidate = self.prepare_core_generation_for(&before, &combined)?;
        let after = self.load()?;
        Ok(CoreUpdate {
            schema_version: 2,
            store_id: before.store_id,
            plan_id: id,
            profile_id: required,
            change: UpdateChange::Expand { profiles },
            before,
            after,
            previous,
            candidate: candidate.id(),
            phase: UpdatePhase::Prepared {},
            execution_request: None,
            result: None,
        })
    }

    /// Caller must check the candidate and the original binary/config first.
    /// Publication authorizes no stop; apply requires this exact plan ID.
    pub fn publish_core_update(&mut self, plan: CoreUpdate) -> Result<UpdateImpact> {
        self.ensure_core_update_idle()?;
        if let Some(previous) = self.core_update()? {
            self.resolve_core_update_request(previous.plan_id, None)?;
        }
        self.recover_config_requests()?;
        self.validate_core_update(&plan)?;
        if plan.phase != (UpdatePhase::Prepared {}) {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE"));
        }
        self.check_update_dependencies(&plan)?;
        let impact = self.core_update_impact(&plan)?;
        store::encode(&impact, 512 * 1024)?;
        self.write_core_update(&plan)?;
        Ok(impact)
    }

    pub fn core_update_impact(&self, plan: &CoreUpdate) -> Result<UpdateImpact> {
        let previous_generation = plan.old_generation()?;
        let active = self.open_core_generation(previous_generation)?;
        let affected_profiles: Vec<_> = active.profiles().iter().map(|p| p.id).collect();
        let added_profiles = self
            .open_core_generation(plan.candidate)?
            .profiles()
            .iter()
            .filter(|p| !affected_profiles.contains(&p.id))
            .map(|p| p.id)
            .collect();
        let bound_instances = plan
            .before
            .instances
            .iter()
            .filter(|i| match i.network {
                NetworkBinding::Profile { profile_id } => affected_profiles.contains(&profile_id),
                _ => false,
            })
            .map(|i| i.id)
            .collect();
        Ok(UpdateImpact {
            plan_id: plan.plan_id,
            manifest_revision: plan.before.revision,
            previous_generation,
            changed_profile: plan.profile_id,
            added_profiles,
            removed_profiles: if matches!(plan.change, UpdateChange::RemoveProfile {}) {
                vec![plan.profile_id]
            } else {
                vec![]
            },
            affected_profiles,
            bound_instances,
        })
    }

    pub fn start_core_update(&mut self, id: Uuid, execution_request: Uuid) -> Result<CoreUpdate> {
        self.ensure_core_launch_idle()?;
        if execution_request.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        self.verify_core_apply_request(execution_request, id)?;
        self.recover_config_requests()?;
        let mut plan = self.require_core_update(id)?;
        if plan.phase != (UpdatePhase::Prepared {}) {
            return Err(Error::Invalid("CORE_UPDATE_ALREADY_APPLIED"));
        }
        self.check_update_dependencies(&plan)?;
        plan.phase = UpdatePhase::Switching {};
        plan.execution_request = Some(execution_request);
        self.write_core_update(&plan)?;
        Ok(plan)
    }

    pub fn transition_core_update(
        &mut self,
        id: Uuid,
        expected: &CoreState,
        next: CoreState,
    ) -> Result<()> {
        let plan = self.require_core_update(id)?;
        let (generation, manifest) = match plan.phase {
            UpdatePhase::Switching {} => (plan.candidate, &plan.after),
            UpdatePhase::Restoring {} => (plan.old_generation()?, &plan.before),
            _ => return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE")),
        };
        match &next {
            CoreState::Starting { generation: g } | CoreState::Running { generation: g, .. }
                if *g != generation =>
            {
                return Err(Error::Invalid("CORE_UPDATE_GENERATION_MISMATCH"));
            }
            CoreState::Stopped {}
                if !plan.removes_last() || plan.phase != (UpdatePhase::Switching {}) =>
            {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE"));
            }
            _ => {}
        }
        self.transition_core_state_inner(expected, next, Some(manifest))
    }

    /// Health succeeded. Write the commit intent before changing the manifest.
    /// Repeating after an interrupted manifest/receipt write completes one commit.
    pub fn commit_core_update(&mut self, id: Uuid) -> Result<u64> {
        let mut plan = self.require_core_update(id)?;
        if !matches!(
            plan.phase,
            UpdatePhase::Switching {} | UpdatePhase::Committing {}
        ) {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE"));
        }
        let state = self.core_state()?;
        if !(if plan.removes_last() {
            state == (CoreState::Stopped {})
        } else {
            matches!(state, CoreState::Running { generation, .. } if generation == plan.candidate)
        }) {
            return Err(Error::Invalid("CORE_UPDATE_NOT_RUNNING"));
        }
        let current = self.load()?;
        let before = same(&current, &plan.before)?;
        let after = same(&current, &plan.after)?;
        if !before && !after {
            return Err(Error::Invalid("CORE_UPDATE_CONFIG_CONFLICT"));
        }
        if matches!(plan.phase, UpdatePhase::Switching {}) {
            if !before {
                return Err(Error::Invalid("CORE_UPDATE_CONFIG_CONFLICT"));
            }
            plan.phase = UpdatePhase::Committing {};
            self.write_core_update(&plan)?;
        }
        let revision = plan.after.revision;
        if before && !after {
            let mut target: Manifest = store::decode(&store::encode(&plan.after, MANIFEST_LIMIT)?)?;
            target.revision = plan.before.revision;
            self.commit_snapshot(plan.before.revision, target)?;
        }
        plan.phase = UpdatePhase::Committed {};
        plan.result = Some(if matches!(plan.change, UpdateChange::RemoveProfile {}) {
            app_proxy_core::core_control::CoreOutcome::ProfileRemoved {
                profile_id: plan.profile_id,
                revision,
            }
        } else {
            let CoreState::Running {
                generation,
                process,
            } = state
            else {
                return Err(Error::Invalid("CORE_COMMIT_STATE_UNKNOWN"));
            };
            app_proxy_core::core_control::CoreOutcome::Reconfigured {
                generation,
                process,
                revision,
            }
        });
        self.write_core_update(&plan)?;
        Ok(revision)
    }

    pub fn begin_core_restore(&mut self, id: Uuid) -> Result<CoreUpdate> {
        let mut plan = self.require_core_update(id)?;
        if !matches!(
            plan.phase,
            UpdatePhase::Switching {} | UpdatePhase::Restoring {}
        ) {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE"));
        }
        if !same(&self.load()?, &plan.before)? {
            return Err(Error::Invalid("CORE_UPDATE_CONFIG_CONFLICT"));
        }
        plan.phase = UpdatePhase::Restoring {};
        self.write_core_update(&plan)?;
        Ok(plan)
    }

    pub fn finish_core_restore(&mut self, id: Uuid, core_down: bool) -> Result<()> {
        let mut plan = self.require_core_update(id)?;
        if plan.phase != (UpdatePhase::Restoring {}) || !same(&self.load()?, &plan.before)? {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_PHASE"));
        }
        let old = plan.old_generation()?;
        let matches = match self.core_state()? {
            CoreState::Running { generation, .. } => !core_down && generation == old,
            CoreState::Down { generation } => core_down && generation == old,
            _ => false,
        };
        if !matches {
            return Err(Error::Invalid("CORE_RESTORE_NOT_CONFIRMED"));
        }
        plan.phase = UpdatePhase::Restored { core_down };
        plan.result = Some(app_proxy_core::core_control::CoreOutcome::Restored { core_down });
        self.write_core_update(&plan)
    }

    pub fn mark_core_restore_down(&mut self, id: Uuid) -> Result<()> {
        let plan = self.require_core_update(id)?;
        let state = self.core_state()?;
        if plan.phase != (UpdatePhase::Restoring {}) || !matches!(state, CoreState::Down { .. }) {
            return Err(Error::Invalid("CORE_RESTORE_STATE_UNKNOWN"));
        }
        self.transition_core_state_inner(
            &state,
            CoreState::Down {
                generation: plan.old_generation()?,
            },
            Some(&plan.before),
        )
    }

    pub fn require_core_update(&self, id: Uuid) -> Result<CoreUpdate> {
        self.core_update()?
            .filter(|p| p.plan_id == id)
            .ok_or(Error::Invalid("CORE_UPDATE_PLAN_CHANGED"))
    }

    pub fn resolve_core_update_request(&mut self, id: Uuid, exclude: Option<Uuid>) -> Result<()> {
        use app_proxy_core::{
            core_control::{CoreAction, CoreOutcome},
            model::ProxySource,
            registry::{ManualProxyInput, ProxyCredentialInput},
        };
        let plan = self.require_core_update(id)?;
        let prepare = if matches!(plan.change, UpdateChange::RemoveProfile {}) {
            CoreAction::PrepareRemove {
                expected_revision: plan.before.revision,
                profile_id: plan.profile_id,
            }
        } else if let UpdateChange::Expand { ref profiles } = plan.change {
            CoreAction::PrepareExpand {
                expected_revision: plan.before.revision,
                profiles: profiles.clone(),
                required: plan.profile_id,
            }
        } else if let Some(edit) = subscription_edit(&plan)? {
            CoreAction::PrepareSubscription {
                expected_revision: plan.before.revision,
                profile_id: plan.profile_id,
                edit,
            }
        } else {
            let profile = plan
                .after
                .profiles
                .iter()
                .find(|p| p.id == plan.profile_id)
                .expect("validated profile");
            let ProxySource::Manual { nodes } = &profile.source else {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            };
            let node = &nodes[0];
            let credentials = node
                .credentials
                .as_ref()
                .map(|c| {
                    self.read_secret(c.password_secret_id)
                        .map(|password| ProxyCredentialInput {
                            username: c.username.clone(),
                            password,
                        })
                })
                .transpose()?;
            CoreAction::PrepareUpdate {
                expected_revision: plan.before.revision,
                profile_id: plan.profile_id,
                node: ManualProxyInput {
                    protocol: node.protocol.clone(),
                    host: node.host.clone(),
                    port: node.port,
                    credentials,
                },
            }
        };
        let prepared = CoreOutcome::Prepared {
            impact: self.core_update_impact(&plan)?,
        };
        if plan.phase == (UpdatePhase::Prepared {}) {
            return self.resolve_core_update_receipts(
                plan.plan_id,
                id,
                &prepare,
                prepared,
                exclude,
            );
        }
        // Check the execution binding first, before modifying any earlier receipt.
        let request = plan
            .execution_request
            .ok_or(Error::Invalid("CORE_UPDATE_NOT_EXECUTED"))?;
        let outcome = plan
            .result
            .ok_or(Error::Invalid("CORE_UPDATE_RESULT_UNKNOWN"))?;
        self.resolve_core_update_receipts(
            request,
            id,
            &CoreAction::ApplyUpdate { plan_id: id },
            outcome,
            exclude,
        )?;
        self.resolve_core_preparation_receipt(plan.plan_id, &prepare, prepared)
    }

    fn check_update_dependencies(&self, plan: &CoreUpdate) -> Result<()> {
        if !same(&self.load()?, &plan.before)? || self.core_state()? != plan.previous {
            return Err(Error::Invalid("CORE_UPDATE_PLAN_STALE"));
        }
        Ok(())
    }

    fn validate_core_update(&self, plan: &CoreUpdate) -> Result<()> {
        let header = self.load()?;
        crate::core_state::validate_state(&plan.previous, &header.owner_sid)?;
        if !(plan.schema_version == 2
            || (plan.schema_version == 1 && matches!(plan.change, UpdateChange::EditProfile {})))
            || plan.store_id != header.store_id
            || plan.plan_id.is_nil()
            || plan.profile_id.is_nil()
            || plan.candidate.is_nil()
        {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        let valid_phase = match (&plan.phase, &plan.result, plan.execution_request) {
            (UpdatePhase::Prepared {}, None, None) => true,
            (
                UpdatePhase::Switching {} | UpdatePhase::Committing {} | UpdatePhase::Restoring {},
                None,
                Some(id),
            ) => !id.is_nil(),
            (
                UpdatePhase::Committed {},
                Some(app_proxy_core::core_control::CoreOutcome::Reconfigured {
                    generation,
                    revision,
                    process,
                }),
                Some(id),
            ) => {
                !id.is_nil()
                    && !matches!(plan.change, UpdateChange::RemoveProfile {})
                    && *generation == plan.candidate
                    && *revision == plan.after.revision
                    && process.user_sid == header.owner_sid
                    && process.pid != 0
                    && process.creation_time != 0
                    && process.image_path.is_absolute()
            }
            (
                UpdatePhase::Committed {},
                Some(app_proxy_core::core_control::CoreOutcome::ProfileRemoved {
                    profile_id,
                    revision,
                }),
                Some(id),
            ) => {
                !id.is_nil()
                    && matches!(plan.change, UpdateChange::RemoveProfile {})
                    && *profile_id == plan.profile_id
                    && *revision == plan.after.revision
            }
            (
                UpdatePhase::Restored { core_down },
                Some(app_proxy_core::core_control::CoreOutcome::Restored {
                    core_down: result_down,
                }),
                Some(id),
            ) => !id.is_nil() && core_down == result_down,
            _ => false,
        };
        if !valid_phase {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        self.validate(&plan.before)?;
        self.validate(&plan.after)?;
        let old = self.open_core_generation(plan.old_generation()?)?;
        let candidate = self.open_core_generation(plan.candidate)?;
        if !self.core_generation_matches(&old, &plan.before)?
            || (!plan.removes_last() && !self.core_generation_matches(&candidate, &plan.after)?)
        {
            return Err(Error::Invalid("CORE_UPDATE_GENERATION_MISMATCH"));
        }
        let old_ids: Vec<_> = old.profiles().iter().map(|p| p.id).collect();
        let candidate_ids: Vec<_> = candidate.profiles().iter().map(|p| p.id).collect();
        if matches!(plan.change, UpdateChange::RemoveProfile {}) {
            let retained: Vec<_> = old_ids
                .iter()
                .copied()
                .filter(|id| *id != plan.profile_id)
                .collect();
            if !old_ids.contains(&plan.profile_id)
                || (if retained.is_empty() {
                    !plan.removes_last()
                } else {
                    retained != candidate_ids || plan.removes_last()
                })
            {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            }
            let before = store::decode(&store::encode(&plan.before, MANIFEST_LIMIT)?)?;
            let (mut expected, receipt) = registry::apply(
                before,
                &ConfigRequest {
                    request_id: plan.plan_id,
                    expected_revision: plan.before.revision,
                    action: ConfigAction::RemoveProfile {
                        profile_id: plan.profile_id,
                    },
                },
            )
            .map_err(|_| Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
            expected.revision = receipt.revision;
            if !same(&expected, &plan.after)? {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            }
            return Ok(());
        }
        if let UpdateChange::Expand { profiles } = &plan.change {
            let mut action = app_proxy_core::core_control::CoreAction::PrepareExpand {
                expected_revision: plan.before.revision,
                profiles: profiles.clone(),
                required: plan.profile_id,
            };
            action
                .normalize()
                .map_err(|_| Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
            let app_proxy_core::core_control::CoreAction::PrepareExpand {
                profiles: normalized,
                ..
            } = action
            else {
                unreachable!()
            };
            let mut combined = old_ids.clone();
            combined.extend(&normalized);
            combined.sort();
            combined.dedup();
            if normalized != *profiles
                || combined != candidate_ids
                || combined.len() <= old_ids.len()
                || combined.len() > 1024
                || !same(&plan.before, &plan.after)?
            {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            }
            return Ok(());
        }
        if old_ids != candidate_ids
            || plan.before.revision.checked_add(1) != Some(plan.after.revision)
        {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        if !old.profiles().iter().any(|p| p.id == plan.profile_id) {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        if let Some(edit) = subscription_edit(plan)? {
            let before = store::decode(&store::encode(&plan.before, MANIFEST_LIMIT)?)?;
            let (mut expected, receipt) = registry::apply(
                before,
                &ConfigRequest {
                    request_id: plan.plan_id,
                    expected_revision: plan.before.revision,
                    action: ConfigAction::EditSubscriptionProfile {
                        profile_id: plan.profile_id,
                        edit,
                    },
                },
            )
            .map_err(|_| Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
            expected.revision = receipt.revision;
            if !same(&expected, &plan.after)? {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            }
            return Ok(());
        }
        let mut expected: Manifest = store::decode(&store::encode(&plan.before, MANIFEST_LIMIT)?)?;
        let profile = plan
            .after
            .profiles
            .iter()
            .find(|p| p.id == plan.profile_id)
            .ok_or(Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
        let target = expected
            .profiles
            .iter_mut()
            .find(|p| p.id == plan.profile_id)
            .ok_or(Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
        let app_proxy_core::model::ProxySource::Manual {
            nodes: before_nodes,
        } = &target.source
        else {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        };
        let app_proxy_core::model::ProxySource::Manual { nodes: after_nodes } = &profile.source
        else {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        };
        if before_nodes.len() != 1 || after_nodes.len() != 1 {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        if profile.endpoint != target.endpoint
            || profile.name != target.name
            || profile.selected_node_id != target.selected_node_id
            || target.revision.checked_add(1) != Some(profile.revision)
        {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        *target = profile.clone();
        expected.revision = plan.after.revision;
        if !same(&expected, &plan.after)? {
            return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
        }
        Ok(())
    }

    fn write_core_update(&self, plan: &CoreUpdate) -> Result<()> {
        let _pins = self.core_directories(true)?;
        self.replace_bounded(PATH, &store::encode(plan, LIMIT)?, LIMIT)
    }
}

fn same(left: &Manifest, right: &Manifest) -> Result<bool> {
    Ok(store::encode(left, MANIFEST_LIMIT)? == store::encode(right, MANIFEST_LIMIT)?)
}

/// Reconstruct only the reference-valued edit represented by these two snapshots.
/// Replaying registry::apply then proves no unrelated field was modified.
fn subscription_edit(plan: &CoreUpdate) -> Result<Option<registry::SubscriptionEdit>> {
    use app_proxy_core::model::ProxySource;
    use registry::SubscriptionEdit;
    if !matches!(
        plan.change,
        UpdateChange::RefreshSubscription {} | UpdateChange::SelectSubscriptionNode {}
    ) {
        return Ok(None);
    }
    let before = plan
        .before
        .profiles
        .iter()
        .find(|p| p.id == plan.profile_id)
        .ok_or(Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
    let after = plan
        .after
        .profiles
        .iter()
        .find(|p| p.id == plan.profile_id)
        .ok_or(Error::Invalid("INVALID_CORE_UPDATE_RECORD"))?;
    let ProxySource::Subscription {
        revision,
        url_secret_id,
        ..
    } = &before.source
    else {
        return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
    };
    Ok(Some(match plan.change {
        UpdateChange::RefreshSubscription {} => {
            let ProxySource::Subscription { nodes, .. } = &after.source else {
                return Err(Error::Invalid("INVALID_CORE_UPDATE_RECORD"));
            };
            SubscriptionEdit::Refresh {
                expected_source_revision: *revision,
                expected_url_secret_id: *url_secret_id,
                nodes: nodes.clone(),
            }
        }
        UpdateChange::SelectSubscriptionNode {} => SubscriptionEdit::Select {
            expected_source_revision: *revision,
            node_id: after.selected_node_id,
        },
        _ => unreachable!(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_core::{
        core_control::{CoreAction, CoreOutcome},
        model::*,
        registry::*,
    };
    use std::{fs, os::windows::fs::OpenOptionsExt};

    fn setup() -> (tempfile::TempDir, Store, Uuid) {
        let temp = tempfile::tempdir().unwrap();
        let mut store = Store::create(&temp.path().join("store")).unwrap();
        let profile = Uuid::new_v4();
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 1,
                action: ConfigAction::CreateManualProfile {
                    profile_id: profile,
                    name: "fixture".into(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: 29001,
                    },
                    node: node(8080),
                },
            })
            .unwrap();
        let generation = store.prepare_core_generation(&[profile]).unwrap().id();
        let starting = CoreState::Starting { generation };
        store
            .transition_core_state(&CoreState::Stopped {}, starting.clone())
            .unwrap();
        store
            .transition_core_state(
                &starting,
                CoreState::Running {
                    generation,
                    process: crate::identity::current().unwrap(),
                },
            )
            .unwrap();
        (temp, store, profile)
    }
    fn node(port: u16) -> ManualProxyInput {
        ManualProxyInput {
            protocol: ManualProtocol::Http,
            host: "proxy.example".into(),
            port,
            credentials: Some(ProxyCredentialInput {
                username: "user".into(),
                password: "private-update-password".into(),
            }),
        }
    }
    fn prepare(store: &mut Store, profile: Uuid) -> UpdateImpact {
        let revision = store.load().unwrap().revision;
        let plan = store
            .prepare_core_update(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: revision,
                action: ConfigAction::UpdateManualProfile {
                    profile_id: profile,
                    node: node(8081),
                },
            })
            .unwrap();
        store.publish_core_update(plan).unwrap()
    }
    fn begin(store: &mut Store, id: Uuid) -> CoreUpdate {
        let request = Uuid::new_v4();
        store
            .begin_core_request(
                request,
                Uuid::new_v4(),
                &CoreAction::ApplyUpdate { plan_id: id },
            )
            .unwrap();
        store.start_core_update(id, request).unwrap()
    }
    fn candidate_running(store: &mut Store, plan: &CoreUpdate) {
        let down = CoreState::Down {
            generation: plan.old_generation().unwrap(),
        };
        store
            .transition_core_update(plan.plan_id, &plan.previous, down.clone())
            .unwrap();
        let starting = CoreState::Starting {
            generation: plan.candidate,
        };
        store
            .transition_core_update(plan.plan_id, &down, starting.clone())
            .unwrap();
        store
            .transition_core_update(
                plan.plan_id,
                &starting,
                CoreState::Running {
                    generation: plan.candidate,
                    process: crate::identity::current().unwrap(),
                },
            )
            .unwrap();
    }

    #[test]
    fn existing_edit_journal_and_prepared_receipts_remain_readable() {
        for committed in [false, true] {
            let (temp, mut store, profile) = setup();
            let id = Uuid::new_v4();
            let epoch = Uuid::new_v4();
            let revision = store.load().unwrap().revision;
            store
                .begin_core_request(
                    id,
                    epoch,
                    &CoreAction::PrepareUpdate {
                        expected_revision: revision,
                        profile_id: profile,
                        node: node(8081),
                    },
                )
                .unwrap();
            let plan = store
                .prepare_core_update(&ConfigRequest {
                    request_id: id,
                    expected_revision: revision,
                    action: ConfigAction::UpdateManualProfile {
                        profile_id: profile,
                        node: node(8081),
                    },
                })
                .unwrap();
            let impact = store.publish_core_update(plan).unwrap();
            store
                .finish_core_request(id, epoch, CoreOutcome::Prepared { impact })
                .unwrap();
            if committed {
                let plan = begin(&mut store, id);
                candidate_running(&mut store, &plan);
                store.commit_core_update(id).unwrap();
            }
            // Exact prior Rust journal shape: schema 1, no change discriminant.
            let mut legacy: serde_json::Value =
                serde_json::from_slice(&fs::read(store.root().join(PATH)).unwrap()).unwrap();
            legacy["schema_version"] = 1.into();
            legacy.as_object_mut().unwrap().remove("change");
            store
                .replace_bounded(PATH, &serde_json::to_vec(&legacy).unwrap(), LIMIT)
                .unwrap();
            let receipt =
                fs::read_to_string(store.root().join(format!("state/core-requests/{id}.json")))
                    .unwrap();
            assert!(receipt.contains("changed_profile"));
            assert!(!receipt.contains("added_profiles"));
            drop(store);
            let mut store = Store::open(&temp.path().join("store")).unwrap();
            assert!(store.core_request_status(id).unwrap().is_some());
            store.resolve_core_update_request(id, None).unwrap();
            assert!(!store.has_unresolved_core_requests().unwrap());
            assert!(store.ensure_core_update_idle().is_ok());
            let added = Uuid::new_v4();
            store
                .apply_config(&ConfigRequest {
                    request_id: Uuid::new_v4(),
                    expected_revision: store.load().unwrap().revision,
                    action: ConfigAction::CreateManualProfile {
                        profile_id: added,
                        name: "added".into(),
                        endpoint: Endpoint {
                            host: "127.0.0.1".parse().unwrap(),
                            port: 29002,
                        },
                        node: node(8082),
                    },
                })
                .unwrap();
            let plan = store
                .prepare_core_expansion(
                    Uuid::new_v4(),
                    store.load().unwrap().revision,
                    &[added],
                    added,
                )
                .unwrap();
            store.publish_core_update(plan).unwrap();
        }
    }

    #[test]
    fn expansion_preserves_old_routes_and_manifest_across_commit_recovery() {
        let (temp, mut store, old) = setup();
        let added = Uuid::new_v4();
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: store.load().unwrap().revision,
                action: ConfigAction::CreateManualProfile {
                    profile_id: added,
                    name: "added".into(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: 29002,
                    },
                    node: node(8082),
                },
            })
            .unwrap();
        let before = store.load().unwrap();
        let revision = before.revision;
        assert!(matches!(
            store.prepare_core_expansion(Uuid::new_v4(), revision - 1, &[added], added),
            Err(Error::Invalid("STALE_MANIFEST_REVISION"))
        ));
        assert!(matches!(
            store.prepare_core_expansion(Uuid::new_v4(), revision, &[old], old),
            Err(Error::Invalid("CORE_PROFILE_SET_UNCHANGED"))
        ));
        assert!(
            store
                .prepare_core_expansion(Uuid::new_v4(), revision, &[added], old)
                .is_err()
        );
        let id = Uuid::new_v4();
        store
            .begin_core_request(
                id,
                Uuid::new_v4(),
                &CoreAction::PrepareExpand {
                    expected_revision: revision,
                    profiles: vec![added, added],
                    required: added,
                },
            )
            .unwrap();
        let plan = store
            .prepare_core_expansion(id, revision, &[added, added], added)
            .unwrap();
        let previous = plan.previous.clone();
        let impact = store.publish_core_update(plan).unwrap();
        assert_eq!(impact.affected_profiles, [old]);
        assert_eq!(impact.added_profiles, [added]);
        assert_eq!(impact.changed_profile, added);
        assert_eq!(store.core_state().unwrap(), previous);
        assert!(same(&store.load().unwrap(), &before).unwrap());
        // Recover a lost preparation ACK without executing it.
        store.resolve_core_update_request(id, None).unwrap();
        assert!(!store.has_unresolved_core_requests().unwrap());
        let plan = begin(&mut store, id);
        candidate_running(&mut store, &plan);
        let mut staged = store.require_core_update(id).unwrap();
        staged.phase = UpdatePhase::Committing {};
        store.write_core_update(&staged).unwrap();
        drop(store);
        let mut store = Store::open(&temp.path().join("store")).unwrap();
        assert_eq!(store.commit_core_update(id).unwrap(), revision);
        assert!(same(&store.load().unwrap(), &before).unwrap());
        store.resolve_core_update_request(id, None).unwrap();
        assert!(!store.has_unresolved_core_requests().unwrap());
        assert_eq!(
            store
                .open_core_generation(plan.candidate)
                .unwrap()
                .profiles()
                .len(),
            2
        );
    }

    #[test]
    fn expansion_rejects_snapshot_mutation_and_removal_from_candidate() {
        let (_temp, mut store, old) = setup();
        let added = Uuid::new_v4();
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: store.load().unwrap().revision,
                action: ConfigAction::CreateManualProfile {
                    profile_id: added,
                    name: "added".into(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: 29002,
                    },
                    node: node(8082),
                },
            })
            .unwrap();
        let revision = store.load().unwrap().revision;
        let mut plan = store
            .prepare_core_expansion(Uuid::new_v4(), revision, &[added], added)
            .unwrap();
        plan.after.settings.test_url = "https://altered.example".into();
        assert!(store.publish_core_update(plan).is_err());
        let mut plan = store
            .prepare_core_expansion(Uuid::new_v4(), revision, &[added], added)
            .unwrap();
        plan.candidate = store.prepare_core_generation(&[added]).unwrap().id();
        assert!(store.publish_core_update(plan).is_err());
        let mut plan = store
            .prepare_core_expansion(Uuid::new_v4(), revision, &[added], added)
            .unwrap();
        plan.change = UpdateChange::Expand {
            profiles: vec![old],
        };
        assert!(store.publish_core_update(plan).is_err());
        assert!(store.core_update().unwrap().is_none());
        assert_eq!(store.load().unwrap().revision, revision);
    }

    #[test]
    fn checked_plan_preserves_manifest_until_exact_revision_and_plan_confirmation() {
        let (temp, mut store, profile) = setup();
        let before = store.load().unwrap();
        let impact = prepare(&mut store, profile);
        assert!(same(&store.load().unwrap(), &before).unwrap());
        assert_eq!(impact.affected_profiles, [profile]);
        assert!(
            !fs::read_to_string(store.root().join(PATH))
                .unwrap()
                .contains("private-update-password")
        );
        drop(store);
        let mut store = Store::open(&temp.path().join("store")).unwrap();
        assert!(matches!(
            store.require_core_update(Uuid::new_v4()),
            Err(Error::Invalid("CORE_UPDATE_PLAN_CHANGED"))
        ));
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: before.revision,
                action: ConfigAction::RenameProfile {
                    profile_id: profile,
                    name: "changed while reviewing".into(),
                },
            })
            .unwrap();
        let execution = Uuid::new_v4();
        store
            .begin_core_request(
                execution,
                Uuid::new_v4(),
                &CoreAction::ApplyUpdate {
                    plan_id: impact.plan_id,
                },
            )
            .unwrap();
        assert!(matches!(
            store.start_core_update(impact.plan_id, execution),
            Err(Error::Invalid("CORE_UPDATE_PLAN_STALE"))
        ));
        assert_eq!(
            store.core_update().unwrap().unwrap().phase,
            UpdatePhase::Prepared {}
        );
    }

    #[test]
    fn last_profile_removal_requires_stop_and_recovers_commit_on_both_sides() {
        for after_manifest_write in [false, true] {
            let (temp, mut store, profile) = setup();
            let revision = store.load().unwrap().revision;
            let plan = store
                .prepare_core_update(&ConfigRequest {
                    request_id: Uuid::new_v4(),
                    expected_revision: revision,
                    action: ConfigAction::RemoveProfile {
                        profile_id: profile,
                    },
                })
                .unwrap();
            assert!(plan.removes_last());
            let impact = store.publish_core_update(plan).unwrap();
            assert_eq!(impact.removed_profiles, [profile]);
            assert_eq!(store.load().unwrap().profiles.len(), 1);
            let mut plan = begin(&mut store, impact.plan_id);
            assert!(store.commit_core_update(plan.plan_id).is_err());
            let down = CoreState::Down {
                generation: plan.old_generation().unwrap(),
            };
            store
                .transition_core_update(plan.plan_id, &plan.previous, down.clone())
                .unwrap();
            store
                .transition_core_update(plan.plan_id, &down, CoreState::Stopped {})
                .unwrap();
            plan.phase = UpdatePhase::Committing {};
            store.write_core_update(&plan).unwrap();
            if after_manifest_write {
                let mut target: Manifest =
                    store::decode(&store::encode(&plan.after, MANIFEST_LIMIT).unwrap()).unwrap();
                target.revision = revision;
                store.commit_snapshot(revision, target).unwrap();
            }
            drop(store);
            let mut store = Store::open(&temp.path().join("store")).unwrap();
            assert_eq!(
                store.commit_core_update(plan.plan_id).unwrap(),
                revision + 1
            );
            assert!(store.load().unwrap().profiles.is_empty());
            assert_eq!(store.core_state().unwrap(), CoreState::Stopped {});
            assert!(
                matches!(store.require_core_update(plan.plan_id).unwrap().result, Some(CoreOutcome::ProfileRemoved { profile_id, revision: r }) if profile_id == profile && r == revision + 1)
            );
        }
    }

    #[test]
    fn removal_rejects_stale_confirmation_and_changed_target_snapshot() {
        let (_temp, mut store, profile) = setup();
        let revision = store.load().unwrap().revision;
        let mut plan = store
            .prepare_core_update(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: revision,
                action: ConfigAction::RemoveProfile {
                    profile_id: profile,
                },
            })
            .unwrap();
        plan.after.settings.test_url = "https://different.invalid/".into();
        assert!(store.publish_core_update(plan).is_err());
        let plan = store
            .prepare_core_update(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: revision,
                action: ConfigAction::RemoveProfile {
                    profile_id: profile,
                },
            })
            .unwrap();
        let impact = store.publish_core_update(plan).unwrap();
        let mut manifest = store.load().unwrap();
        manifest.profiles[0].name = "renamed".into();
        store.commit(revision, manifest).unwrap();
        let id = Uuid::new_v4();
        store
            .begin_core_request(
                id,
                Uuid::new_v4(),
                &CoreAction::ApplyUpdate {
                    plan_id: impact.plan_id,
                },
            )
            .unwrap();
        assert!(store.start_core_update(impact.plan_id, id).is_err());
        assert!(matches!(
            store.core_state().unwrap(),
            CoreState::Running { .. }
        ));
    }

    #[test]
    fn switching_blocks_other_writes_and_commit_is_recoverable_on_both_sides() {
        for after_manifest in [false, true] {
            let (temp, mut store, profile) = setup();
            let impact = prepare(&mut store, profile);
            let plan = begin(&mut store, impact.plan_id);
            assert!(store.ensure_core_update_idle().is_err());
            assert!(
                store
                    .apply_config(&ConfigRequest {
                        request_id: Uuid::new_v4(),
                        expected_revision: plan.before.revision,
                        action: ConfigAction::RenameProfile {
                            profile_id: profile,
                            name: "blocked".into()
                        }
                    })
                    .is_err()
            );
            assert!(
                store
                    .transition_core_state(&plan.previous, CoreState::Stopped {})
                    .is_err()
            );
            candidate_running(&mut store, &plan);
            let mut staged = store.require_core_update(plan.plan_id).unwrap();
            staged.phase = UpdatePhase::Committing {};
            store.write_core_update(&staged).unwrap();
            if after_manifest {
                let mut target = staged.after;
                target.revision = plan.before.revision;
                store.commit_snapshot(plan.before.revision, target).unwrap();
            }
            drop(store);
            let mut store = Store::open(&temp.path().join("store")).unwrap();
            assert!(store.ensure_core_update_idle().is_err());
            assert_eq!(
                store.commit_core_update(plan.plan_id).unwrap(),
                plan.before.revision + 1
            );
            store
                .resolve_core_update_request(plan.plan_id, None)
                .unwrap();
            assert!(!store.has_unresolved_core_requests().unwrap());
            assert!(store.ensure_core_update_idle().is_ok());
            assert!(matches!(
                store.core_update().unwrap().unwrap().result,
                Some(CoreOutcome::Reconfigured { .. })
            ));
            assert_eq!(store.load().unwrap().revision, plan.before.revision + 1);
        }
    }

    #[test]
    fn commit_file_lock_keeps_intent_and_never_rolls_back_an_uncertain_commit() {
        let (_temp, mut store, profile) = setup();
        let impact = prepare(&mut store, profile);
        let plan = begin(&mut store, impact.plan_id);
        candidate_running(&mut store, &plan);
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(store.root().join("manifest.json"))
            .unwrap();
        assert!(store.commit_core_update(plan.plan_id).is_err());
        assert_eq!(
            store.require_core_update(plan.plan_id).unwrap().phase,
            UpdatePhase::Committing {}
        );
        assert!(store.begin_core_restore(plan.plan_id).is_err());
        drop(held);
        store.commit_core_update(plan.plan_id).unwrap();
        store
            .resolve_core_update_request(plan.plan_id, None)
            .unwrap();
        assert!(!store.has_unresolved_core_requests().unwrap());
    }

    #[test]
    fn failed_restore_retains_old_generation_and_unknown_start_cannot_claim_completion() {
        let (_temp, mut store, profile) = setup();
        let impact = prepare(&mut store, profile);
        let plan = begin(&mut store, impact.plan_id);
        let down = CoreState::Down {
            generation: plan.old_generation().unwrap(),
        };
        store
            .transition_core_update(plan.plan_id, &plan.previous, down.clone())
            .unwrap();
        let starting = CoreState::Starting {
            generation: plan.candidate,
        };
        store
            .transition_core_update(plan.plan_id, &down, starting.clone())
            .unwrap();
        store.begin_core_restore(plan.plan_id).unwrap();
        assert!(store.finish_core_restore(plan.plan_id, true).is_err());
        assert!(store.mark_core_restore_down(plan.plan_id).is_err());
        // The caller has now confirmed this fixture spawn never happened.
        store
            .transition_core_update(
                plan.plan_id,
                &starting,
                CoreState::Down {
                    generation: plan.candidate,
                },
            )
            .unwrap();
        store.mark_core_restore_down(plan.plan_id).unwrap();
        store.finish_core_restore(plan.plan_id, true).unwrap();
        assert!(same(&store.load().unwrap(), &plan.before).unwrap());
        assert_eq!(store.core_state().unwrap(), down);
        store
            .resolve_core_update_request(plan.plan_id, None)
            .unwrap();
        assert!(!store.has_unresolved_core_requests().unwrap());
    }

    #[test]
    fn damaged_plan_and_other_manifest_changes_are_rejected_without_replacing_data() {
        let (_temp, mut store, profile) = setup();
        let impact = prepare(&mut store, profile);
        let mut plan = store.require_core_update(impact.plan_id).unwrap();
        plan.after.settings.test_url = "https://changed.example".into();
        store.write_core_update(&plan).unwrap();
        let bytes = fs::read(store.root().join(PATH)).unwrap();
        assert!(store.core_update().is_err());
        assert_eq!(fs::read(store.root().join(PATH)).unwrap(), bytes);
        assert!(same(&store.load().unwrap(), &plan.before).unwrap());
    }

    #[test]
    fn update_receipts_require_matching_action_digest_and_do_not_resolve_other_work() {
        let (_temp, mut store, profile) = setup();
        let impact = prepare(&mut store, profile);
        let unrelated = Uuid::new_v4();
        store
            .begin_core_request(unrelated, Uuid::new_v4(), &CoreAction::Stop {})
            .unwrap();
        assert!(matches!(
            store.start_core_update(impact.plan_id, unrelated),
            Err(Error::Invalid("CORE_UPDATE_REQUEST_MISMATCH"))
        ));
        let plan = begin(&mut store, impact.plan_id);
        candidate_running(&mut store, &plan);
        store.commit_core_update(plan.plan_id).unwrap();
        let mut altered = store.require_core_update(plan.plan_id).unwrap();
        altered.execution_request = Some(unrelated);
        store.write_core_update(&altered).unwrap();
        let unrelated_path = store
            .root()
            .join(format!("state/core-requests/{unrelated}.json"));
        let original_path = store.root().join(format!(
            "state/core-requests/{}.json",
            plan.execution_request.unwrap()
        ));
        let before = (
            fs::read(&unrelated_path).unwrap(),
            fs::read(&original_path).unwrap(),
        );
        assert!(matches!(
            store.resolve_core_update_request(plan.plan_id, None),
            Err(Error::Invalid("CORE_UPDATE_REQUEST_MISMATCH"))
        ));
        assert_eq!(
            before,
            (
                fs::read(unrelated_path).unwrap(),
                fs::read(original_path).unwrap()
            )
        );
    }
}
