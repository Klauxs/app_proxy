//! One serialized core lifecycle per store owner. The coordinator must retain
//! this manager and must not cancel accepted lifecycle work on IPC disconnect.
use crate::{configuration::Configuration, proxy_health};
use app_proxy_core::{ProcessIdentity, model::Endpoint};
use app_proxy_windows::{
    Error, Result,
    core_process::CoreProcess,
    core_state::{CoreProfile, CoreState},
    singbox_binary,
};
use serde::{Deserialize, Serialize};
use std::{future::Future, path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::Mutex;
use uuid::Uuid;

pub struct CoreManager {
    root: PathBuf,
    pub(crate) configuration: Arc<Configuration>,
    pub(crate) gate: Mutex<()>,
}
#[derive(Serialize)]
pub struct ReadyCore {
    pub generation: Uuid,
    pub process: ProcessIdentity,
}

/// Process/listener observation only; this is not a fresh proxy health check.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreSnapshot {
    pub recorded: CoreState,
    pub observed: CoreObserved,
    pub profiles: Vec<CoreProfile>,
    pub update: Option<CoreUpdateSummary>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreUpdateSummary {
    pub phase: app_proxy_windows::core_update::UpdatePhase,
    pub impact: app_proxy_core::core_control::UpdateImpact,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreObserved {
    Stopped,
    Down,
    Listening,
    Indeterminate,
}

impl CoreManager {
    /// Download through a currently verified owned route only. The lifecycle
    /// gate keeps our stop/reconfigure from replacing it during this request.
    pub(crate) async fn download_subscription(
        &self,
        profile_id: Uuid,
        target: &str,
    ) -> Result<crate::subscription_download::Result<crate::subscription_download::Downloaded>>
    {
        let _gate = self.gate.lock().await;
        let (process, endpoint, endpoints) = {
            let store = self.configuration.lock()?;
            store.ensure_core_update_idle()?;
            let CoreState::Running {
                generation,
                process,
            } = store.core_state()?
            else {
                return Err(Error::Invalid("SUBSCRIPTION_DOWNLOAD_PROXY_NOT_READY"));
            };
            let active = store.open_core_generation(generation)?;
            if !store.core_generation_is_current(&active)? {
                return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
            }
            let endpoint = active
                .profiles()
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or(Error::Invalid("SUBSCRIPTION_DOWNLOAD_PROXY_NOT_READY"))?
                .endpoint
                .clone();
            let endpoints: Vec<_> = active
                .profiles()
                .iter()
                .map(|p| p.endpoint.clone())
                .collect();
            (process, endpoint, endpoints)
        };
        let core = CoreProcess::attach(&process)?;
        if !core.listeners_verified(&endpoints)? {
            return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
        }
        let result = crate::subscription_download::download(target, Some(&endpoint)).await;
        if !core.listeners_verified(&endpoints)? {
            return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
        }
        Ok(result)
    }
    pub fn new(root: PathBuf, configuration: Arc<Configuration>) -> Self {
        Self {
            root,
            configuration,
            gate: Mutex::new(()),
        }
    }

    /// Publish a durable launch permission under the same gate used by stop and
    /// reconfiguration. The launch engine must use this entry, not call the store
    /// transition while another core lifecycle operation may be in flight.
    pub async fn ready_launch(
        &self,
        id: Uuid,
        epoch: Uuid,
        binding: app_proxy_core::launch::LaunchBinding,
    ) -> Result<app_proxy_core::launch::LaunchAttempt> {
        let _gate = self.gate.lock().await;
        let mut store = self.configuration.lock()?;
        if let app_proxy_core::launch::LaunchNetwork::Profile { generation, .. } = binding.network {
            let CoreState::Running {
                generation: current,
                process,
            } = store.core_state()?
            else {
                return Err(Error::Invalid("LAUNCH_CORE_CHANGED"));
            };
            if generation != current {
                return Err(Error::Invalid("LAUNCH_CORE_CHANGED"));
            }
            let core = CoreProcess::attach(&process)?;
            let endpoints: Vec<_> = store
                .open_core_generation(generation)?
                .profiles()
                .iter()
                .map(|p| p.endpoint.clone())
                .collect();
            if !core.listeners_verified(&endpoints)? {
                return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
            }
        }
        store.ready_launch(id, epoch, binding)
    }

    pub async fn ensure(
        &self,
        profiles: &[Uuid],
        required: Uuid,
        target: &str,
    ) -> Result<ReadyCore> {
        self.ensure_with(profiles, required, |endpoint| async move {
            proxy_health::check(&endpoint, target, &[200, 204])
                .await
                .map(|_| ())
                .map_err(|error| Error::Invalid(error.code()))
        })
        .await
    }

    pub(crate) async fn ensure_with<F, Fut>(
        &self,
        profiles: &[Uuid],
        required: Uuid,
        probe: F,
    ) -> Result<ReadyCore>
    where
        F: FnOnce(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        self.ensure_request_with(None, profiles, required, probe)
            .await
    }

    pub(crate) async fn ensure_request_with<F, Fut>(
        &self,
        request_id: Option<Uuid>,
        profiles: &[Uuid],
        required: Uuid,
        probe: F,
    ) -> Result<ReadyCore>
    where
        F: FnOnce(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if !profiles.contains(&required) || required.is_nil() {
            return Err(Error::Invalid("REQUIRED_PROFILE_MISSING"));
        }
        let _gate = self.gate.lock().await;
        let mut requested_profiles = profiles.to_vec();
        let mut state = {
            let mut store = self.configuration.lock()?;
            store.ensure_core_update_idle()?;
            store.recover_config_requests()?;
            store.core_state()?
        };
        if let CoreState::Running {
            generation,
            process,
        } = &state
            && CoreProcess::recover(process)?.is_none()
        {
            let down = CoreState::Down {
                generation: *generation,
            };
            self.configuration
                .lock()?
                .transition_core_state(&state, down.clone())?;
            state = down;
        }
        match state {
            CoreState::Starting { .. } => Err(Error::Invalid("CORE_START_RESULT_UNKNOWN")),
            CoreState::Running {
                generation,
                ref process,
            } => {
                let running = CoreProcess::attach(process)?;
                if !running.is_running()? {
                    return Err(Error::Invalid("CORE_PROCESS_DOWN"));
                }
                let active = {
                    let store = self.configuration.lock()?;
                    let active = store.open_core_generation(generation)?;
                    if profiles
                        .iter()
                        .any(|id| !active.profiles().iter().any(|p| p.id == *id))
                        || !store.core_generation_is_current(&active)?
                    {
                        return Err(Error::Invalid("CORE_RECONFIGURE_REQUIRES_CONFIRMATION"));
                    }
                    active
                };
                let endpoints: Vec<_> = active
                    .profiles()
                    .iter()
                    .map(|p| p.endpoint.clone())
                    .collect();
                if !running.listeners_verified(&endpoints)? {
                    return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
                }
                let endpoint = active
                    .profiles()
                    .iter()
                    .find(|p| p.id == required)
                    .ok_or(Error::Invalid("REQUIRED_PROFILE_MISSING"))?
                    .endpoint
                    .clone();
                // A runtime failure preserves both the shared core and all apps.
                probe(endpoint).await?;
                if !self
                    .configuration
                    .lock()?
                    .core_generation_is_current(&active)?
                {
                    return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
                }
                if !running.listeners_verified(&endpoints)? {
                    return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
                }
                Ok(ReadyCore {
                    generation,
                    process: process.clone(),
                })
            }
            previous @ (CoreState::Stopped {} | CoreState::Down { .. }) => {
                // Keep the recovery set durable across missing binaries, occupied
                // ports, failed health checks and subsequent owner restarts.
                if let CoreState::Down { generation } = &previous {
                    let active = self
                        .configuration
                        .lock()?
                        .open_core_generation(*generation)?;
                    requested_profiles.extend(active.profiles().iter().map(|p| p.id));
                    requested_profiles.sort();
                    requested_profiles.dedup();
                }
                let candidate = self
                    .configuration
                    .lock()?
                    .prepare_core_generation(&requested_profiles)?;
                let binary = singbox_binary::discover(&self.root)
                    .await?
                    .ok_or(Error::Invalid("CORE_BINARY_MISSING"))?;
                binary.check_config(candidate.config_path()).await?;
                let endpoints: Vec<_> = candidate
                    .profiles()
                    .iter()
                    .map(|p| p.endpoint.clone())
                    .collect();
                // Availability only. Ownership is checked after our spawn; a bind
                // race never authorizes adoption or termination of its winner.
                let reservations: Vec<_> = endpoints
                    .iter()
                    .map(|e| std::net::TcpListener::bind((e.host, e.port)))
                    .collect::<std::io::Result<_>>()
                    .map_err(|_| Error::Invalid("CORE_PORT_OCCUPIED"))?;
                let starting = CoreState::Starting {
                    generation: candidate.id(),
                };
                let prepared = {
                    let mut store = self.configuration.lock()?;
                    if !store.core_generation_is_current(&candidate)? {
                        return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
                    }
                    let mut prepared = CoreProcess::prepare(&mut store, &binary, &candidate)?;
                    if let Some(id) = request_id {
                        prepared = prepared.bind_request(
                            &mut store,
                            id,
                            app_proxy_core::core_control::CoreAction::Start {
                                profiles: profiles.to_vec(),
                                required,
                            },
                        )?;
                    }
                    store.transition_core_state(&previous, starting.clone())?;
                    prepared
                };
                drop(reservations);
                // No await between spawn and the identity journal. If anything
                // fails before identity is durable, Starting blocks blind retry.
                // Even an IO error can follow successful native creation.
                // Only the durable witness can prove that no core survives.
                let core = prepared.spawn(&binary, &candidate)?;
                let running = CoreState::Running {
                    generation: candidate.id(),
                    process: core.identity().clone(),
                };
                let recorded = self
                    .configuration
                    .lock()?
                    .transition_core_state(&starting, running.clone());
                if let Err(error) = recorded {
                    core.stop()?;
                    self.configuration.lock()?.transition_core_state(
                        &starting,
                        CoreState::Down {
                            generation: candidate.id(),
                        },
                    )?;
                    return Err(error);
                }
                let result = async {
                    wait_listeners(&core, &endpoints).await?;
                    let endpoint = candidate
                        .profiles()
                        .iter()
                        .find(|p| p.id == required)
                        .expect("required compiled profile")
                        .endpoint
                        .clone();
                    probe(endpoint).await?;
                    if !self
                        .configuration
                        .lock()?
                        .core_generation_is_current(&candidate)?
                    {
                        return Err(Error::Invalid("CORE_CONFIG_CHANGED"));
                    }
                    if !core.listeners_verified(&endpoints)? {
                        return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
                    }
                    Ok(ReadyCore {
                        generation: candidate.id(),
                        process: core.identity().clone(),
                    })
                }
                .await;
                if result.is_err() {
                    core.stop()?;
                    self.configuration.lock()?.transition_core_state(
                        &running,
                        CoreState::Down {
                            generation: candidate.id(),
                        },
                    )?;
                }
                result
            }
        }
    }

    /// Explicit reconciliation records process ownership, never health or a new spawn.
    pub(crate) async fn recover_start(
        &self,
        expected_generation: Uuid,
    ) -> Result<app_proxy_core::core_control::CoreOutcome> {
        let _gate = self.gate.lock().await;
        let mut store = self.configuration.lock()?;
        store.ensure_core_update_idle()?;
        store.ensure_core_launch_idle()?;
        let mut state = store.core_state()?;
        if !matches!(&state, CoreState::Starting { generation } | CoreState::Running { generation, .. } | CoreState::Down { generation } if *generation == expected_generation)
        {
            return Err(Error::Invalid("CORE_START_GENERATION_CHANGED"));
        }
        if let CoreState::Starting { generation } = state {
            let observed = store.inspect_core_start(generation)?;
            let next = match observed {
                Some(core) if core.is_running()? => CoreState::Running {
                    generation,
                    process: core.identity().clone(),
                },
                _ => CoreState::Down { generation },
            };
            store.transition_core_state(&state, next.clone())?;
            state = next;
        }
        let (generation, process) = match state {
            CoreState::Running {
                generation,
                process,
            } => {
                let alive = match CoreProcess::recover(&process)? {
                    Some(core) => core.is_running()?,
                    None => false,
                };
                if alive {
                    (generation, Some(process))
                } else {
                    store.transition_core_state(
                        &CoreState::Running {
                            generation,
                            process,
                        },
                        CoreState::Down { generation },
                    )?;
                    (generation, None)
                }
            }
            CoreState::Down { generation } => (generation, None),
            _ => return Err(Error::Invalid("CORE_START_RECOVERY_NOT_REQUIRED")),
        };
        store.resolve_core_start_request(generation, process.clone())?;
        Ok(app_proxy_core::core_control::CoreOutcome::Reconciled {
            generation,
            process,
        })
    }

    pub async fn stop(&self) -> Result<()> {
        let _gate = self.gate.lock().await;
        self.configuration.lock()?.ensure_core_update_idle()?;
        self.configuration.lock()?.ensure_core_launch_idle()?;
        let state = self.configuration.lock()?.core_state()?;
        match &state {
            CoreState::Stopped {} => Ok(()),
            CoreState::Down { .. } => self
                .configuration
                .lock()?
                .transition_core_state(&state, CoreState::Stopped {}),
            CoreState::Starting { .. } => Err(Error::Invalid("CORE_START_RESULT_UNKNOWN")),
            CoreState::Running { process, .. } => {
                if let Some(core) = CoreProcess::recover(process)? {
                    core.stop()?;
                }
                self.configuration
                    .lock()?
                    .transition_core_state(&state, CoreState::Stopped {})
            }
        }
    }

    pub fn state(&self) -> Result<CoreState> {
        self.configuration.lock()?.core_state()
    }

    pub fn snapshot(&self) -> Result<CoreSnapshot> {
        let store = self.configuration.lock()?;
        let recorded = store.core_state()?;
        let mut profiles = Vec::new();
        if let CoreState::Starting { generation }
        | CoreState::Down { generation }
        | CoreState::Running { generation, .. } = &recorded
        {
            profiles = store.open_core_generation(*generation)?.profiles().to_vec();
        }
        let observed = match &recorded {
            CoreState::Stopped {} => CoreObserved::Stopped,
            CoreState::Down { .. } => CoreObserved::Down,
            CoreState::Starting { .. } => CoreObserved::Indeterminate,
            CoreState::Running { process, .. } => match CoreProcess::recover(process) {
                Ok(None) => CoreObserved::Down,
                Ok(Some(core)) => {
                    let endpoints: Vec<_> = profiles.iter().map(|p| p.endpoint.clone()).collect();
                    match core.listeners_verified(&endpoints) {
                        Ok(true) => CoreObserved::Listening,
                        _ => CoreObserved::Indeterminate,
                    }
                }
                Err(_) => CoreObserved::Indeterminate,
            },
        };
        Ok(CoreSnapshot {
            recorded,
            observed,
            profiles,
            update: store
                .core_update()?
                .map(|plan| -> Result<CoreUpdateSummary> {
                    let impact = store.core_update_impact(&plan)?;
                    Ok(CoreUpdateSummary {
                        phase: plan.phase,
                        impact,
                    })
                })
                .transpose()?,
        })
    }
}

pub(crate) async fn wait_listeners(core: &CoreProcess, endpoints: &[Endpoint]) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        if !core.is_running()? {
            return Err(Error::Invalid("CORE_EXITED_BEFORE_READY"));
        }
        if core.listeners_verified(endpoints)? {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("CORE_LISTENER_TIMEOUT"));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
}
