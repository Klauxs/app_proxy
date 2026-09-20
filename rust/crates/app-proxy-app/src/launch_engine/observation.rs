//! Read-only runtime evidence, independent of desired Guard configuration.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeStatus {
    pub instance_id: Uuid,
    pub revision: u64,
    pub observation: InstanceObservation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstanceObservation {
    Session {
        process: app_proxy_core::ProcessIdentity,
        network: LaunchNetwork,
        configuration_changed: Option<bool>,
    },
    Observed {
        process: app_proxy_core::ProcessIdentity,
    },
    Absent {},
    Pending {
        attempt_id: Uuid,
    },
    Unknown {
        code: String,
    },
}

impl LaunchEngine {
    /// Never creates data, contacts a proxy, modifies a receipt, enables Guard,
    /// adopts an external process or authorizes a stop.
    pub async fn observe_instance(
        &self,
        instance_id: Uuid,
        allowed: bool,
    ) -> Result<RuntimeStatus> {
        let snapshot = self.configuration.snapshot()?;
        let (application, instance) = entries(&snapshot, instance_id)?;
        let mut observation = if allowed {
            match tokio::time::timeout(
                Duration::from_secs(3),
                self.scan_instance(&snapshot, application, instance),
            )
            .await
            {
                Ok(Ok(value)) => value,
                Ok(Err(Error::Invalid(code))) => InstanceObservation::Unknown { code: code.into() },
                Ok(Err(_)) => InstanceObservation::Unknown {
                    code: "INSTANCE_OBSERVATION_FAILED".into(),
                },
                Err(_) => InstanceObservation::Unknown {
                    code: "INSTANCE_OBSERVATION_TIMEOUT".into(),
                },
            }
        } else {
            InstanceObservation::Unknown {
                code: "INSTANCE_OBSERVATION_BUSY".into(),
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.after_guard_scan.lock().unwrap().clone() {
            hook();
        }
        let store = self.configuration.lock()?;
        if store.load()?.revision != snapshot.revision {
            observation = InstanceObservation::Unknown {
                code: "LAUNCH_CONFIG_CHANGED".into(),
            };
        } else if allowed {
            let mut session_present = !matches!(observation, InstanceObservation::Session { .. });
            for attempt in store
                .launch_attempts()?
                .into_iter()
                .filter(|a| a.instance_id == instance_id && a.reserves_instance())
            {
                if matches!(attempt.phase, LaunchPhase::Confirmed { .. })
                    && store.inspect_launch_process(attempt.id)?.is_none()
                {
                    continue;
                }
                // A launch accepted during external scanning supersedes an absent
                // or external-process result. It does not retroactively confirm it.
                if !matches!(&observation, InstanceObservation::Session { process, .. } if matches!(&attempt.phase, LaunchPhase::Confirmed { process: expected } if process == expected))
                {
                    observation = InstanceObservation::Pending {
                        attempt_id: attempt.id,
                    };
                }
                session_present = true;
            }
            if !session_present {
                observation = InstanceObservation::Unknown {
                    code: "LAUNCH_SESSION_CHANGED".into(),
                };
            }
        }
        Ok(RuntimeStatus {
            instance_id,
            revision: snapshot.revision,
            observation,
        })
    }

    async fn scan_instance(
        &self,
        snapshot: &Manifest,
        application: &Application,
        instance: &Instance,
    ) -> Result<InstanceObservation> {
        let attempts = self.configuration.lock()?.launch_attempts()?;
        for attempt in attempts
            .iter()
            .filter(|a| a.instance_id == instance.id && a.reserves_instance())
        {
            if let LaunchPhase::Confirmed { process } = &attempt.phase {
                if let Some(observed) = self
                    .configuration
                    .lock()?
                    .inspect_launch_process(attempt.id)?
                {
                    if &observed != process {
                        return Err(Error::Invalid("LAUNCH_SESSION_CHANGED"));
                    }
                    let binding = attempt
                        .binding
                        .as_ref()
                        .ok_or(Error::Invalid("INSTANCE_SESSION_BINDING_MISSING"))?;
                    return Ok(InstanceObservation::Session {
                        process: process.clone(),
                        network: binding.network.clone(),
                        configuration_changed: dependency_digest(snapshot, instance.id)
                            .ok()
                            .map(|digest| binding.dependency_digest != digest),
                    });
                }
                // An exact exited session is historical, not a pending launch.
                // Leave the receipt unchanged; maintenance owns its release.
                continue;
            }
            return Ok(InstanceObservation::Pending {
                attempt_id: attempt.id,
            });
        }
        let resolved = self
            .resolve_for_observation(application.locator.clone())
            .await?;
        let candidates =
            query_when_ready(|| process_query::application_candidates(&resolved)).await?;
        if candidates.is_empty() {
            return Ok(InstanceObservation::Absent {});
        }
        let data = {
            let store = self.configuration.lock()?;
            if store.load()?.revision != snapshot.revision {
                return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
            }
            store.inspect_instance_data(instance.id, resolved.package())?
        };
        let target = InstanceTarget::new(&resolved, data.as_ref(), application.template_ref)?;
        let mut main = None;
        let mut auxiliary = false;
        for process in candidates {
            let observed = query_when_ready(|| target.inspect(&process)).await?;
            match (observed.role, observed.relation) {
                (_, InstanceRelation::Other) => {}
                (ProcessRole::Auxiliary, InstanceRelation::Target) => auxiliary = true,
                (ProcessRole::Main, InstanceRelation::Target) if main.is_none() => {
                    main = Some(observed.identity)
                }
                (ProcessRole::Main, InstanceRelation::Target) => {
                    return Err(Error::Invalid("INSTANCE_MULTIPLE_MAIN_PROCESSES"));
                }
                _ => return Err(Error::Invalid("INSTANCE_PROCESS_UNKNOWN")),
            }
        }
        match main {
            Some(process) => Ok(InstanceObservation::Observed { process }),
            None if auxiliary => Err(Error::Invalid("INSTANCE_AUXILIARY_RUNNING")),
            None => Ok(InstanceObservation::Absent {}),
        }
    }

    pub(super) async fn resolve_for_observation(
        &self,
        locator: ApplicationLocator,
    ) -> Result<installation::ResolvedApplication> {
        let permit = self
            .guard_resolution
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Invalid("GUARD_RESOLUTION_BUSY"))?;
        #[cfg(test)]
        let hook = self.before_guard_resolution.lock().unwrap().clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            installation::resolve(&locator)
        })
        .await
        .map_err(|_| Error::Invalid("GUARD_RESOLUTION_INTERRUPTED"))?
    }
}
