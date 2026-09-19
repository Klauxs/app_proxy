//! Ordinary-user diagnostics. Desired configuration never serves as proof that
//! an elevated listener or machine-wide IFEO registration has been installed.
use crate::{
    configuration::Configuration,
    launch_engine::{GuardScan, LaunchEngine},
};
use app_proxy_core::model::{Desired, InstanceData};
use app_proxy_windows::{Error, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardPhase {
    Disabled,
    NeedsAuthorization,
    Blocked,
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentState {
    NotApplicable,
    NeedsAuthorization,
    Unverified,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardStatus {
    pub instance_id: Uuid,
    pub revision: u64,
    pub desired: Desired,
    pub phase: GuardPhase,
    pub listener: ComponentState,
    pub ifeo: ComponentState,
    pub scan: Option<GuardScan>,
    pub diagnostic: Option<String>,
}

pub async fn status(
    configuration: &Configuration,
    launch: &LaunchEngine,
    id: Uuid,
    scan_allowed: bool,
) -> Result<GuardStatus> {
    let scan = if scan_allowed {
        Some(launch.observe_guard(id).await?)
    } else {
        None
    };
    let manifest = configuration.snapshot()?;
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
    if scan
        .as_ref()
        .is_some_and(|s| s.revision != manifest.revision)
    {
        return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
    }
    let registered_ifeo = manifest
        .integrations
        .ifeo
        .iter()
        .any(|i| i.default_instance_id == id);
    let enabled = instance.guard.desired == Desired::Enabled;
    // No privileged installer is connected yet. Existing metadata is not live
    // component evidence and must never upgrade these states to active.
    let listener = if !enabled {
        ComponentState::NotApplicable
    } else if manifest.integrations.guard_login_task.is_some() {
        ComponentState::Unverified
    } else {
        ComponentState::NeedsAuthorization
    };
    let ifeo = if registered_ifeo {
        ComponentState::Unverified
    } else if enabled && matches!(instance.data, InstanceData::Original {}) {
        ComponentState::NeedsAuthorization
    } else {
        ComponentState::NotApplicable
    };
    let phase = if listener == ComponentState::Unverified || ifeo == ComponentState::Unverified {
        GuardPhase::Blocked
    } else if enabled {
        GuardPhase::NeedsAuthorization
    } else {
        GuardPhase::Disabled
    };
    Ok(GuardStatus {
        instance_id: id,
        revision: manifest.revision,
        desired: instance.guard.desired,
        phase,
        listener,
        ifeo,
        scan,
        diagnostic: if !scan_allowed {
            Some("GUARD_SCAN_BUSY".into())
        } else {
            None
        },
    })
}

#[cfg(test)]
mod tests {
    use crate::launch_engine::GuardObservation;

    #[test]
    fn guard_observation_wire_rejects_unknown_fields_in_all_empty_states() {
        for state in ["absent", "disabled"] {
            let valid = serde_json::json!({ "state": state });
            let observed: GuardObservation = serde_json::from_value(valid.clone()).unwrap();
            assert_eq!(serde_json::to_value(observed).unwrap(), valid);
            assert!(
                serde_json::from_value::<GuardObservation>(
                    serde_json::json!({"state":state,"unexpected":true})
                )
                .is_err()
            );
        }
        assert!(
            serde_json::from_value::<GuardObservation>(
                serde_json::json!({"state":"blocked","code":"GUARD_SCAN_BUSY","unexpected":true})
            )
            .is_err()
        );
    }
}
