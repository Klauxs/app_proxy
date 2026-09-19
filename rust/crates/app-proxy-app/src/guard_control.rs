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
    let manifest = configuration.snapshot()?;
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
    let enabled = instance.guard.desired == Desired::Enabled;
    let (scan, mut listener, diagnostic) = observations(
        async {
            if scan_allowed {
                launch.observe_guard(id).await.map(Some)
            } else {
                Ok(None)
            }
        },
        async {
            if enabled {
                listener_component(manifest.store_id).await
            } else {
                (ComponentState::NotApplicable, None)
            }
        },
    )
    .await?;
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
    // A protected intent and registered task still do not prove live coverage.
    if listener == ComponentState::NeedsAuthorization
        && manifest.integrations.guard_login_task.is_some()
    {
        listener = ComponentState::Unverified;
    }
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
    if configuration.snapshot()?.revision != manifest.revision {
        return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
    }
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
            diagnostic
        },
    })
}

/// Both observations share a deadline below the five-second RPC frame budget.
/// Dropping their waits does not release permits owned by native workers.
pub(crate) async fn observations(
    scan: impl std::future::Future<Output = Result<Option<GuardScan>>>,
    listener: impl std::future::Future<Output = (ComponentState, Option<String>)>,
) -> Result<(Option<GuardScan>, ComponentState, Option<String>)> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    let (scan, listener) = tokio::join!(
        tokio::time::timeout_at(deadline, scan),
        tokio::time::timeout_at(deadline, listener),
    );
    let (scan, scan_note) = match scan {
        Ok(result) => (result?, None),
        Err(_) => (None, Some("GUARD_SCAN_TIMEOUT".into())),
    };
    let (listener, listener_note) = listener.unwrap_or_else(|_| {
        (
            ComponentState::Unverified,
            Some("GUARD_LISTENER_CHECK_UNCONFIRMED".into()),
        )
    });
    Ok((scan, listener, scan_note.or(listener_note)))
}

async fn listener_component(store: Uuid) -> (ComponentState, Option<String>) {
    use app_proxy_windows::{guard_deployment::Deployment, guard_task};
    use std::sync::{Arc, OnceLock};
    static CHECK: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let Ok(permit) = CHECK
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
        .clone()
        .try_acquire_owned()
    else {
        return (
            ComponentState::Unverified,
            Some("GUARD_LISTENER_CHECK_BUSY".into()),
        );
    };
    let worker = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let deployment = Deployment::listener(store)?;
        let _coordinator = deployment.coordinator()?;
        guard_task::verify_registered(&deployment)
    });
    match worker.await {
        Ok(Ok(())) => (
            ComponentState::Unverified,
            Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED".into()),
        ),
        Ok(Err(Error::Invalid("GUARD_LISTENER_MISSING" | "GUARD_TASK_MISSING"))) => {
            (ComponentState::NeedsAuthorization, None)
        }
        Ok(Err(error)) => (ComponentState::Unverified, Some(error.to_string())),
        _ => (
            ComponentState::Unverified,
            Some("GUARD_LISTENER_CHECK_UNCONFIRMED".into()),
        ),
    }
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
