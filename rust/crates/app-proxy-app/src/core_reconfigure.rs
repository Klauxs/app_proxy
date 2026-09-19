use crate::core_manager::{CoreManager, ReadyCore, wait_listeners};
use app_proxy_core::{
    core_control::{CoreOutcome, UpdateImpact},
    model::Endpoint,
    registry::ConfigRequest,
};
use app_proxy_windows::{
    Error, Result,
    core_process::CoreProcess,
    core_state::CoreState,
    core_update::{CoreUpdate, UpdatePhase},
    singbox_binary::{self, CoreBinary},
};
use std::future::Future;
use uuid::Uuid;

#[cfg(test)]
mod tests;

impl CoreManager {
    pub(crate) async fn prepare_update(&self, request: &ConfigRequest) -> Result<UpdateImpact> {
        let _gate = self.gate.lock().await;
        let plan = self.configuration.lock()?.prepare_core_update(request)?;
        self.check_and_publish_update(plan).await
    }

    pub(crate) async fn prepare_expand(
        &self,
        id: Uuid,
        expected_revision: u64,
        profiles: &[Uuid],
        required: Uuid,
    ) -> Result<UpdateImpact> {
        let _gate = self.gate.lock().await;
        let plan = self.configuration.lock()?.prepare_core_expansion(
            id,
            expected_revision,
            profiles,
            required,
        )?;
        self.check_and_publish_update(plan).await
    }

    async fn check_and_publish_update(&self, plan: CoreUpdate) -> Result<UpdateImpact> {
        let CoreState::Running { ref process, .. } = plan.previous else {
            unreachable!()
        };
        let old = CoreProcess::attach(process)?;
        if !old.is_running()? {
            return Err(Error::Invalid("CORE_PROCESS_DOWN"));
        }
        let binary = singbox_binary::inspect_recorded(process).await?;
        let (candidate, original) = {
            let store = self.configuration.lock()?;
            (
                store.open_core_generation(plan.candidate)?,
                store.open_core_generation(plan.old_generation()?)?,
            )
        };
        binary.check_config(candidate.config_path()).await?;
        binary.check_config(original.config_path()).await?;
        if !old.listeners_verified(
            &original
                .profiles()
                .iter()
                .map(|p| p.endpoint.clone())
                .collect::<Vec<_>>(),
        )? {
            return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
        }
        self.configuration.lock()?.publish_core_update(plan)
    }

    pub(crate) async fn apply_update<F, Fut>(
        &self,
        id: Uuid,
        execution_request: Uuid,
        probe: F,
    ) -> Result<CoreOutcome>
    where
        F: Fn(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _gate = self.gate.lock().await;
        let prepared = self.configuration.lock()?.require_core_update(id)?;
        if prepared.phase != (UpdatePhase::Prepared {}) {
            return Err(Error::Invalid("CORE_UPDATE_ALREADY_APPLIED"));
        }
        let CoreState::Running { ref process, .. } = prepared.previous else {
            unreachable!()
        };
        let old = CoreProcess::attach(process)?;
        let binary = singbox_binary::inspect_recorded(process).await?;
        let (candidate, original) = {
            let store = self.configuration.lock()?;
            (
                store.open_core_generation(prepared.candidate)?,
                store.open_core_generation(prepared.old_generation()?)?,
            )
        };
        // Repeat checks after the user's think time, before stopping anything.
        binary.check_config(candidate.config_path()).await?;
        binary.check_config(original.config_path()).await?;
        if !old.is_running()? {
            return Err(Error::Invalid("CORE_PROCESS_DOWN"));
        }
        let plan = self
            .configuration
            .lock()?
            .start_core_update(id, execution_request)?;
        old.stop()?;
        self.configuration.lock()?.transition_core_update(
            id,
            &plan.previous,
            CoreState::Down {
                generation: plan.old_generation()?,
            },
        )?;
        match self.launch_update(&plan, false, &binary, &probe).await {
            Ok(ready) => {
                let revision = self.configuration.lock()?.commit_core_update(id)?;
                Ok(CoreOutcome::Reconfigured {
                    generation: ready.generation,
                    process: ready.process,
                    revision,
                })
            }
            Err(_) => self.restore_update(&plan, &binary, &probe).await,
        }
    }

    pub(crate) async fn recover_update<F, Fut>(&self, id: Uuid, probe: F) -> Result<CoreOutcome>
    where
        F: Fn(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let _gate = self.gate.lock().await;
        let plan = self.configuration.lock()?.require_core_update(id)?;
        if let Some(result) = plan.result {
            return Ok(result);
        }
        match plan.phase {
            UpdatePhase::Prepared {} => Ok(CoreOutcome::Prepared {
                impact: self.configuration.lock()?.core_update_impact(&plan)?,
            }),
            UpdatePhase::Committing {} => {
                // A durable commit intent means all candidate health checks had
                // already passed. Complete pure writes; never spawn on this path.
                let mut store = self.configuration.lock()?;
                let revision = store.commit_core_update(id)?;
                let CoreState::Running {
                    generation,
                    process,
                } = store.core_state()?
                else {
                    return Err(Error::Invalid("CORE_COMMIT_STATE_UNKNOWN"));
                };
                Ok(CoreOutcome::Reconfigured {
                    generation,
                    process,
                    revision,
                })
            }
            UpdatePhase::Switching {} | UpdatePhase::Restoring {} => {
                let CoreState::Running { ref process, .. } = plan.previous else {
                    unreachable!()
                };
                let binary = singbox_binary::inspect_recorded(process).await?;
                self.restore_update(&plan, &binary, &probe).await
            }
            _ => Err(Error::Invalid("CORE_UPDATE_RECOVERY_NOT_REQUIRED")),
        }
    }

    async fn launch_update<F, Fut>(
        &self,
        plan: &CoreUpdate,
        restoring: bool,
        binary: &CoreBinary,
        probe: &F,
    ) -> Result<ReadyCore>
    where
        F: Fn(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let generation = if restoring {
            plan.old_generation()?
        } else {
            plan.candidate
        };
        let candidate = self
            .configuration
            .lock()?
            .open_core_generation(generation)?;
        let endpoints: Vec<_> = candidate
            .profiles()
            .iter()
            .map(|p| p.endpoint.clone())
            .collect();
        let reservations: Vec<_> = endpoints
            .iter()
            .map(|e| std::net::TcpListener::bind((e.host, e.port)))
            .collect::<std::io::Result<_>>()
            .map_err(|_| Error::Invalid("CORE_PORT_OCCUPIED"))?;
        let starting = CoreState::Starting { generation };
        {
            let mut store = self.configuration.lock()?;
            let previous = store.core_state()?;
            store.transition_core_update(plan.plan_id, &previous, starting.clone())?;
        }
        drop(reservations);
        let core = match CoreProcess::spawn(binary, &candidate) {
            Ok(core) => core,
            Err(error) => {
                if matches!(error, Error::Io(_)) {
                    self.configuration.lock()?.transition_core_update(
                        plan.plan_id,
                        &starting,
                        CoreState::Down { generation },
                    )?;
                }
                return Err(error);
            }
        };
        let running = CoreState::Running {
            generation,
            process: core.identity().clone(),
        };
        if self
            .configuration
            .lock()?
            .transition_core_update(plan.plan_id, &starting, running)
            .is_err()
        {
            core.stop()?;
            self.configuration.lock()?.transition_core_update(
                plan.plan_id,
                &starting,
                CoreState::Down { generation },
            )?;
            return Err(Error::Invalid("CORE_UPDATE_IDENTITY_WRITE_FAILED"));
        }
        wait_listeners(&core, &endpoints).await?;
        if restoring {
            // Restore the shared process if at least one old route still works;
            // a previously broken unrelated route must not take every app down.
            let mut ordered = endpoints.clone();
            if let Some(changed) = candidate
                .profiles()
                .iter()
                .position(|p| p.id == plan.profile_id)
            {
                ordered.swap(0, changed);
            }
            let healthy = any_old_route_healthy(&ordered, probe).await;
            if !healthy {
                return Err(Error::Invalid("CORE_PROXY_HEALTH_FAILED"));
            }
        } else {
            let endpoint = candidate
                .profiles()
                .iter()
                .find(|p| p.id == plan.profile_id)
                .ok_or(Error::Invalid("PROFILE_NOT_ACTIVE"))?
                .endpoint
                .clone();
            probe(endpoint).await?;
        }
        if !core.listeners_verified(&endpoints)? {
            return Err(Error::Invalid("CORE_LISTENER_OWNER_UNCONFIRMED"));
        }
        Ok(ReadyCore {
            generation,
            process: core.identity().clone(),
        })
    }

    async fn restore_update<F, Fut>(
        &self,
        plan: &CoreUpdate,
        binary: &CoreBinary,
        probe: &F,
    ) -> Result<CoreOutcome>
    where
        F: Fn(Endpoint) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let state = self.configuration.lock()?.core_state()?;
        if matches!(state, CoreState::Starting { .. }) {
            return Err(Error::Invalid("CORE_UPDATE_START_UNKNOWN"));
        }
        let plan = self
            .configuration
            .lock()?
            .begin_core_restore(plan.plan_id)?;
        if let CoreState::Running {
            generation,
            ref process,
        } = state
        {
            if let Some(core) = CoreProcess::recover(process)? {
                core.stop()?;
            }
            self.configuration.lock()?.transition_core_update(
                plan.plan_id,
                &state,
                CoreState::Down { generation },
            )?;
        }
        let original = self
            .configuration
            .lock()?
            .open_core_generation(plan.old_generation()?)?;
        binary.check_config(original.config_path()).await?;
        let restored = self.launch_update(&plan, true, binary, probe).await;
        if restored.is_err() {
            let state = self.configuration.lock()?.core_state()?;
            match &state {
                CoreState::Starting { .. } => {
                    return Err(Error::Invalid("CORE_UPDATE_START_UNKNOWN"));
                }
                CoreState::Running {
                    generation,
                    process,
                } => {
                    if let Some(core) = CoreProcess::recover(process)? {
                        core.stop()?;
                    }
                    self.configuration.lock()?.transition_core_update(
                        plan.plan_id,
                        &state,
                        CoreState::Down {
                            generation: *generation,
                        },
                    )?;
                }
                CoreState::Down { generation } if *generation != plan.old_generation()? => {
                    // Failure before spawn (e.g. an externally occupied port)
                    // still retains the original recovery set, without adopting it.
                    self.configuration
                        .lock()?
                        .mark_core_restore_down(plan.plan_id)?;
                }
                CoreState::Down { .. } => {}
                _ => return Err(Error::Invalid("CORE_RESTORE_STATE_UNKNOWN")),
            }
        }
        let core_down = restored.is_err();
        self.configuration
            .lock()?
            .finish_core_restore(plan.plan_id, core_down)?;
        Ok(CoreOutcome::Restored { core_down })
    }
}

// Bound concurrent probes without detached tasks. Each production probe has its
// own timeout; every old route gets a chance before declaring the core down.
async fn any_old_route_healthy<F, Fut>(endpoints: &[Endpoint], probe: &F) -> bool
where
    F: Fn(Endpoint) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    use std::task::Poll;
    for chunk in endpoints.chunks(4) {
        let mut probes: Vec<_> = chunk
            .iter()
            .map(|e| Some(Box::pin(probe(e.clone()))))
            .collect();
        let healthy = std::future::poll_fn(|context| {
            let mut pending = false;
            for slot in &mut probes {
                if let Some(future) = slot {
                    match future.as_mut().poll(context) {
                        Poll::Ready(Ok(())) => return Poll::Ready(true),
                        Poll::Ready(Err(_)) => *slot = None,
                        Poll::Pending => pending = true,
                    }
                }
            }
            if pending {
                Poll::Pending
            } else {
                Poll::Ready(false)
            }
        })
        .await;
        if healthy {
            return true;
        }
    }
    false
}
