//! Read-only per-instance scan. A correction is a hint, never stop authority;
//! submit_guard repeats attribution, configuration and resource checks.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardScan {
    pub instance_id: Uuid,
    pub revision: u64,
    pub observation: GuardObservation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum GuardObservation {
    Disabled {},
    Pending {
        attempt_id: Uuid,
    },
    Session {
        process: app_proxy_core::ProcessIdentity,
        network: LaunchNetwork,
    },
    Absent {},
    Compliant {
        process: app_proxy_core::ProcessIdentity,
    },
    Correction {
        target: GuardTarget,
    },
    Blocked {
        code: String,
    },
}

impl LaunchEngine {
    /// Does not contact a proxy, create data, adopt a process, or enable Guard.
    /// Listener authorization and its lifecycle are separate from this evidence.
    pub async fn observe_guard(&self, instance_id: Uuid) -> Result<GuardScan> {
        let snapshot = self.configuration.snapshot()?;
        let (application, instance) = entries(&snapshot, instance_id)?;
        let observation = if instance.guard.desired != Desired::Enabled {
            GuardObservation::Disabled {}
        } else {
            match tokio::time::timeout(
                Duration::from_secs(5),
                self.scan_guard(&snapshot, application, instance),
            )
            .await
            {
                Ok(Ok(observation)) => observation,
                Ok(Err(Error::Invalid(code))) => GuardObservation::Blocked { code: code.into() },
                Ok(Err(_)) => GuardObservation::Blocked {
                    code: "GUARD_OBSERVATION_FAILED".into(),
                },
                Err(_) => GuardObservation::Blocked {
                    code: "GUARD_SCAN_TIMEOUT".into(),
                },
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.after_guard_scan.lock().unwrap().clone() {
            hook();
        }
        // The scan can overlap an edit or a newly accepted launch. Old evidence
        // must never be presented as a correction for the replacement state.
        let store = self.configuration.lock()?;
        let observation = if store.load()?.revision != snapshot.revision {
            GuardObservation::Blocked {
                code: "LAUNCH_CONFIG_CHANGED".into(),
            }
        } else if matches!(observation, GuardObservation::Correction { .. }) {
            if let Some(attempt) = store
                .launch_attempts()?
                .into_iter()
                .find(|a| a.instance_id == instance_id && a.reserves_instance())
            {
                GuardObservation::Pending {
                    attempt_id: attempt.id,
                }
            } else {
                observation
            }
        } else {
            observation
        };
        Ok(GuardScan {
            instance_id,
            revision: snapshot.revision,
            observation,
        })
    }

    async fn scan_guard(
        &self,
        snapshot: &Manifest,
        application: &Application,
        instance: &Instance,
    ) -> Result<GuardObservation> {
        let attempts = self.configuration.lock()?.launch_attempts()?;
        if let Some(attempt) = attempts
            .iter()
            .find(|a| a.instance_id == instance.id && a.reserves_instance())
        {
            if let LaunchPhase::Confirmed { process } = &attempt.phase {
                // The binding is historical. In particular, a later profile or
                // direct-to-proxy edit cannot revoke this running session.
                if process::is_running_exact(process)? {
                    return Ok(GuardObservation::Session {
                        process: process.clone(),
                        network: attempt
                            .binding
                            .as_ref()
                            .ok_or(Error::Invalid("GUARD_SESSION_BINDING_MISSING"))?
                            .network
                            .clone(),
                    });
                }
            }
            return Ok(GuardObservation::Pending {
                attempt_id: attempt.id,
            });
        }
        let NetworkBinding::Profile { profile_id } = instance.network else {
            return Ok(GuardObservation::Disabled {});
        };
        let endpoint = snapshot
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?
            .endpoint
            .clone();
        let locator = application.locator.clone();
        let permit = self
            .guard_resolution
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Invalid("GUARD_RESOLUTION_BUSY"))?;
        #[cfg(test)]
        let hook = self.before_guard_resolution.lock().unwrap().clone();
        let resolved = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            #[cfg(test)]
            if let Some(hook) = hook {
                hook();
            }
            installation::resolve(&locator)
        })
        .await
        .map_err(|_| Error::Invalid("GUARD_RESOLUTION_INTERRUPTED"))??;
        let candidates =
            query_when_ready(|| process_query::application_candidates(&resolved)).await?;
        if candidates.is_empty() {
            return Ok(GuardObservation::Absent {});
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
            let observed = query_when_ready(|| {
                target.inspect_proxy(
                    &process,
                    std::net::SocketAddr::new(endpoint.host, endpoint.port),
                )
            })
            .await?;
            match (observed.role, observed.relation) {
                (_, InstanceRelation::Other) => {}
                (ProcessRole::Auxiliary, InstanceRelation::Target) => auxiliary = true,
                (ProcessRole::Main, InstanceRelation::Target) if main.is_none() => {
                    main = Some(observed)
                }
                (ProcessRole::Main, InstanceRelation::Target) => {
                    return Err(Error::Invalid("GUARD_MULTIPLE_MAIN_PROCESSES"));
                }
                _ => return Err(Error::Invalid("INSTANCE_PROCESS_UNKNOWN")),
            }
        }
        match main {
            Some(main) => match main.proxy {
                ProxyArguments::Matching => Ok(GuardObservation::Compliant {
                    process: main.identity,
                }),
                ProxyArguments::Mismatched => Ok(GuardObservation::Correction {
                    target: GuardTarget {
                        process: main.identity,
                        endpoint,
                    },
                }),
                ProxyArguments::Unknown => Err(Error::Invalid("GUARD_PROXY_ARGUMENTS_UNKNOWN")),
            },
            None if auxiliary => Err(Error::Invalid("GUARD_AUXILIARY_STILL_RUNNING")),
            None => Ok(GuardObservation::Absent {}),
        }
    }
}
