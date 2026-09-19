//! Durable admission and result lookup for serialized core operations.
use crate::{configuration::Configuration, core_manager::CoreManager};
use app_proxy_core::core_control::{CoreAction, CoreOutcome, CoreRequestStatus};
use app_proxy_windows::{Error, Result, core_requests::CoreRequestPhase, core_state::CoreState};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub(crate) struct CoreControl {
    manager: Arc<CoreManager>,
    configuration: Arc<Configuration>,
    epoch: Uuid,
    active: Arc<Mutex<HashMap<Uuid, ActiveOperation>>>,
    installer: crate::core_installer::CoreInstaller,
}

impl CoreControl {
    pub fn new(root: PathBuf, configuration: Arc<Configuration>, epoch: Uuid) -> Self {
        Self {
            manager: Arc::new(CoreManager::new(root, configuration.clone())),
            installer: crate::core_installer::CoreInstaller::new(configuration.clone()),
            configuration,
            epoch,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn manager(&self) -> Arc<CoreManager> {
        self.manager.clone()
    }

    // The caller must execute every newly admitted action, even if its ACK fails.
    pub fn accept(
        &self,
        id: Uuid,
        action: &CoreAction,
    ) -> Result<(CoreRequestStatus, Option<CoreJob>)> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("CORE_CONTROL_POISONED"))?;
        if active.len() >= 32
            && !matches!(action, CoreAction::CancelInstall { .. })
            && self
                .configuration
                .lock()?
                .core_request_status(id)?
                .is_none()
        {
            return Err(Error::Invalid("CORE_OPERATION_LIMIT"));
        }
        let (phase, is_new) = self
            .configuration
            .lock()?
            .begin_core_request(id, self.epoch, action)?;
        let job = is_new.then(|| {
            let (cancel, cancellation) = tokio::sync::watch::channel(false);
            active.insert(
                id,
                ActiveOperation {
                    install: matches!(action, CoreAction::Install {}),
                    cancel,
                },
            );
            CoreJob {
                id,
                action: action.clone(),
                active: self.active.clone(),
                cancellation,
            }
        });
        Ok((self.status_for(id, phase, &active), job))
    }

    pub fn request_status(&self, id: Uuid) -> Result<Option<CoreRequestStatus>> {
        let active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("CORE_CONTROL_POISONED"))?;
        Ok(self
            .configuration
            .lock()?
            .core_request_status(id)?
            .map(|phase| self.status_for(id, phase, &active)))
    }

    fn status_for(
        &self,
        id: Uuid,
        phase: CoreRequestPhase,
        active: &HashMap<Uuid, ActiveOperation>,
    ) -> CoreRequestStatus {
        match phase {
            CoreRequestPhase::Pending { epoch }
                if epoch == self.epoch && active.contains_key(&id) =>
            {
                CoreRequestStatus::Pending {
                    progress: active[&id].install.then(|| self.installer.progress()),
                }
            }
            CoreRequestPhase::Pending { .. } => CoreRequestStatus::Indeterminate {},
            CoreRequestPhase::Complete {
                outcome,
                completed_at,
            } => CoreRequestStatus::Complete {
                outcome,
                completed_at,
            },
        }
    }

    pub async fn execute(&self, mut job: CoreJob) -> Result<()> {
        let result = match job.action.clone() {
            CoreAction::PrepareExpand {
                expected_revision,
                profiles,
                required,
            } => self
                .manager
                .prepare_expand(job.id, expected_revision, &profiles, required)
                .await
                .map(|impact| CoreOutcome::Prepared { impact }),
            CoreAction::PrepareUpdate {
                expected_revision,
                profile_id,
                node,
            } => self
                .manager
                .prepare_update(&app_proxy_core::registry::ConfigRequest {
                    request_id: job.id,
                    expected_revision,
                    action: app_proxy_core::registry::ConfigAction::UpdateManualProfile {
                        profile_id,
                        node,
                    },
                })
                .await
                .map(|impact| CoreOutcome::Prepared { impact }),
            CoreAction::PrepareSubscription {
                expected_revision,
                profile_id,
                edit,
            } => self
                .manager
                .prepare_update(&app_proxy_core::registry::ConfigRequest {
                    request_id: job.id,
                    expected_revision,
                    action: app_proxy_core::registry::ConfigAction::EditSubscriptionProfile {
                        profile_id,
                        edit,
                    },
                })
                .await
                .map(|impact| CoreOutcome::Prepared { impact }),
            CoreAction::ApplyUpdate { plan_id } | CoreAction::RecoverUpdate { plan_id } => {
                let settings = self.configuration.snapshot()?.settings;
                let probe = |endpoint| {
                    let target = settings.test_url.clone();
                    let statuses = settings.health_policy.expected_statuses.clone();
                    async move {
                        crate::proxy_health::check(&endpoint, &target, &statuses)
                            .await
                            .map(|_| ())
                            .map_err(|_| Error::Invalid("CORE_PROXY_HEALTH_FAILED"))
                    }
                };
                if matches!(job.action, CoreAction::ApplyUpdate { .. }) {
                    self.manager.apply_update(plan_id, job.id, probe).await
                } else {
                    self.manager.recover_update(plan_id, probe).await
                }
            }
            CoreAction::Start { profiles, required } => {
                let settings = self.configuration.snapshot()?.settings;
                self.manager
                    .ensure_with(&profiles, required, |endpoint| async move {
                        crate::proxy_health::check(
                            &endpoint,
                            &settings.test_url,
                            &settings.health_policy.expected_statuses,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|_| Error::Invalid("CORE_PROXY_HEALTH_FAILED"))
                    })
                    .await
                    .map(|ready| CoreOutcome::Ready {
                        generation: ready.generation,
                        process: ready.process,
                    })
            }
            CoreAction::Stop {} => self.manager.stop().await.map(|()| CoreOutcome::Stopped {}),
            CoreAction::Install {} => tokio::select! {
                biased;
                cancelled = job.cancellation.wait_for(|value| *value) => cancelled
                    .map(|_| CoreOutcome::Cancelled {})
                    .map_err(|_| Error::Invalid("CORE_CANCEL_RESULT_UNKNOWN")),
                installed = self.installer.install() => installed.map(|()| CoreOutcome::Installed { version: app_proxy_windows::singbox_install::VERSION.into() }),
            },
            CoreAction::CancelInstall { request_id } => self
                .cancel_install(request_id)
                .map(|()| CoreOutcome::CancelRequested { request_id }),
        };
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(Error::Invalid(code)) => {
                // Unknown process/journal results must never look safe to retry.
                if code.contains("UNKNOWN") || code.contains("UNCONFIRMED") {
                    CoreOutcome::Indeterminate { code: code.into() }
                } else {
                    CoreOutcome::Failed { code: code.into() }
                }
            }
            Err(_) => CoreOutcome::Indeterminate {
                code: "CORE_OPERATION_RESULT_UNKNOWN".into(),
            },
        };
        let mut store = self.configuration.lock()?;
        if let CoreAction::RecoverUpdate { plan_id } = job.action
            && matches!(
                outcome,
                CoreOutcome::Prepared { .. }
                    | CoreOutcome::Reconfigured { .. }
                    | CoreOutcome::Restored { .. }
            )
        {
            store.resolve_core_update_request(plan_id, Some(job.id))?;
        }
        let outcome = if matches!(job.action, CoreAction::ApplyUpdate { .. })
            && store
                .core_update()?
                .is_some_and(|plan| plan.active() && plan.execution_request == Some(job.id))
        {
            CoreOutcome::Indeterminate {
                code: "CORE_UPDATE_RECOVERY_REQUIRED".into(),
            }
        } else {
            outcome
        };
        let finished = store.finish_core_request(job.id, self.epoch, outcome);
        drop(store); // Release the store gate before CoreJob removes its active token.
        finished
    }

    pub fn idle_allowed(&self) -> Result<bool> {
        let store = self.configuration.lock()?;
        Ok(matches!(
            store.core_state()?,
            CoreState::Stopped {} | CoreState::Down { .. }
        ) && !store.has_unresolved_core_requests()?
            && !store
                .launch_attempts()?
                .iter()
                .any(|a| a.reserves_instance())
            && store.ensure_core_update_idle().is_ok())
    }

    pub fn snapshot(&self) -> Result<crate::core_manager::CoreSnapshot> {
        self.manager.snapshot()
    }

    fn cancel_install(&self, id: Uuid) -> Result<()> {
        let active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("CORE_CONTROL_POISONED"))?;
        let operation = active
            .get(&id)
            .filter(|op| op.install)
            .ok_or(Error::Invalid("CORE_INSTALL_NOT_ACTIVE"))?;
        operation
            .cancel
            .send(true)
            .map_err(|_| Error::Invalid("CORE_INSTALL_NOT_ACTIVE"))
    }

    #[cfg(test)]
    pub(crate) fn hold_installer(&self, gate: Arc<tokio::sync::Notify>) {
        *self.installer.test_gate.lock().unwrap() = Some(gate);
    }
}

struct ActiveOperation {
    install: bool,
    cancel: tokio::sync::watch::Sender<bool>,
}

// A non-cloneable admission token: dropping before/during execution changes a
// durable Pending to Indeterminate even within the same coordinator epoch.
pub(crate) struct CoreJob {
    id: Uuid,
    action: CoreAction,
    active: Arc<Mutex<HashMap<Uuid, ActiveOperation>>>,
    cancellation: tokio::sync::watch::Receiver<bool>,
}
impl Drop for CoreJob {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_windows::store::Store;

    #[tokio::test]
    async fn update_recovery_resolves_related_uncertainty_preserves_busy_failures_and_handles_expired_receipts()
     {
        use app_proxy_core::{model::*, registry::*};
        use std::{fs, os::windows::fs::OpenOptionsExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let profile = Uuid::new_v4();
        let node = ManualProxyInput {
            protocol: ManualProtocol::Http,
            host: "proxy.example".into(),
            port: 8080,
            credentials: None,
        };
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 1,
                action: ConfigAction::CreateManualProfile {
                    profile_id: profile,
                    name: "fixture".into(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: 29010,
                    },
                    node: node.clone(),
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
                    process: app_proxy_windows::identity::current().unwrap(),
                },
            )
            .unwrap();
        let mut next = node;
        next.port = 8081;
        let prepare_id = Uuid::new_v4();
        store
            .begin_core_request(
                prepare_id,
                Uuid::new_v4(),
                &CoreAction::PrepareUpdate {
                    expected_revision: 2,
                    profile_id: profile,
                    node: next.clone(),
                },
            )
            .unwrap();
        let draft = store
            .prepare_core_update(&ConfigRequest {
                request_id: prepare_id,
                expected_revision: 2,
                action: ConfigAction::UpdateManualProfile {
                    profile_id: profile,
                    node: next,
                },
            })
            .unwrap();
        let plan_id = store.publish_core_update(draft).unwrap().plan_id;
        let configuration = Arc::new(Configuration::new(store));
        let control = CoreControl::new(root.clone(), configuration.clone(), Uuid::new_v4());
        // The plan is durable but its prepare receipt was interrupted.
        let recover_prepare = Uuid::new_v4();
        let (_, job) = control
            .accept(recover_prepare, &CoreAction::RecoverUpdate { plan_id })
            .unwrap();
        control.execute(job.unwrap()).await.unwrap();
        for id in [prepare_id, recover_prepare] {
            assert!(matches!(
                control.request_status(id).unwrap(),
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Prepared { .. },
                    ..
                })
            ));
        }
        assert_eq!(
            control.snapshot().unwrap().update.unwrap().impact.plan_id,
            plan_id
        );
        let original = Uuid::new_v4();
        let (_, job) = control
            .accept(original, &CoreAction::ApplyUpdate { plan_id })
            .unwrap();
        {
            let mut store = configuration.lock().unwrap();
            let plan = store.start_core_update(plan_id, original).unwrap();
            let down = CoreState::Down { generation };
            store
                .transition_core_update(plan_id, &plan.previous, down.clone())
                .unwrap();
            let starting = CoreState::Starting {
                generation: plan.candidate,
            };
            store
                .transition_core_update(plan_id, &down, starting.clone())
                .unwrap();
            store
                .transition_core_update(
                    plan_id,
                    &starting,
                    CoreState::Running {
                        generation: plan.candidate,
                        process: app_proxy_windows::identity::current().unwrap(),
                    },
                )
                .unwrap();
        }
        let held = fs::OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join("manifest.json"))
            .unwrap();
        assert!(
            configuration
                .lock()
                .unwrap()
                .commit_core_update(plan_id)
                .is_err()
        );
        drop(job); // Simulated owner task interruption; no actual process operations.
        let mut refused = Vec::new();
        for action in [
            CoreAction::Stop {},
            CoreAction::Start {
                profiles: vec![profile],
                required: profile,
            },
            CoreAction::ApplyUpdate { plan_id },
        ] {
            let id = Uuid::new_v4();
            let (_, job) = control.accept(id, &action).unwrap();
            control.execute(job.unwrap()).await.unwrap();
            assert!(matches!(
                control.request_status(id).unwrap(),
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Failed { .. },
                    ..
                })
            ));
            refused.push(id);
        }
        let interrupted_recovery = Uuid::new_v4();
        let (_, job) = control
            .accept(interrupted_recovery, &CoreAction::RecoverUpdate { plan_id })
            .unwrap();
        control.execute(job.unwrap()).await.unwrap();
        assert!(matches!(
            control.request_status(interrupted_recovery).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Indeterminate { .. },
                ..
            })
        ));
        drop(held);
        let recovered = Uuid::new_v4();
        let (_, job) = control
            .accept(recovered, &CoreAction::RecoverUpdate { plan_id })
            .unwrap();
        control.execute(job.unwrap()).await.unwrap();
        for id in [original, interrupted_recovery, recovered] {
            assert!(matches!(
                control.request_status(id).unwrap(),
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Reconfigured { .. },
                    ..
                })
            ));
        }
        for id in refused {
            assert!(matches!(
                control.request_status(id).unwrap(),
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Failed { .. },
                    ..
                })
            ));
        }
        {
            let mut store = configuration.lock().unwrap();
            assert!(!store.has_unresolved_core_requests().unwrap());
            let running = store.core_state().unwrap();
            let CoreState::Running { generation, .. } = running else {
                panic!()
            };
            // This is a journal fixture, not a core process to terminate.
            store
                .transition_core_state(&running, CoreState::Down { generation })
                .unwrap();
        }
        assert!(control.idle_allowed().unwrap());
        let receipt_path = root.join(format!("state/core-requests/{original}.json"));
        let mut receipt: serde_json::Value =
            serde_json::from_slice(&fs::read(&receipt_path).unwrap()).unwrap();
        receipt["accepted_at"] = 1.into();
        receipt["phase"]["completed_at"] = 1.into();
        fs::write(receipt_path, serde_json::to_vec(&receipt).unwrap()).unwrap();
        let id = Uuid::new_v4();
        let (_, job) = control
            .accept(id, &CoreAction::RecoverUpdate { plan_id })
            .unwrap(); // prunes expired original
        assert!(control.request_status(original).unwrap().is_none());
        control.execute(job.unwrap()).await.unwrap();
        assert!(matches!(
            control.request_status(id).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Reconfigured { .. },
                ..
            })
        ));
        assert!(control.idle_allowed().unwrap());
    }

    #[tokio::test]
    async fn cancellation_is_durable_does_not_start_download_and_cannot_cancel_other_actions() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
        let control = CoreControl::new(root.clone(), configuration, Uuid::new_v4());
        let install = Uuid::new_v4();
        let (_, install_job) = control.accept(install, &CoreAction::Install {}).unwrap();
        assert!(matches!(
            control.request_status(install).unwrap(),
            Some(CoreRequestStatus::Pending { progress: Some(_) })
        ));
        let cancel = Uuid::new_v4();
        let (_, cancel_job) = control
            .accept(
                cancel,
                &CoreAction::CancelInstall {
                    request_id: install,
                },
            )
            .unwrap();
        control.execute(cancel_job.unwrap()).await.unwrap();
        control.execute(install_job.unwrap()).await.unwrap();
        assert!(matches!(
            control.request_status(install).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Cancelled {},
                ..
            })
        ));
        assert!(
            control
                .accept(install, &CoreAction::Install {})
                .unwrap()
                .1
                .is_none()
        );
        assert!(!root.join("bin").exists());
        assert!(control.idle_allowed().unwrap());

        let stop = Uuid::new_v4();
        let (_, stop_job) = control.accept(stop, &CoreAction::Stop {}).unwrap();
        let cancel = Uuid::new_v4();
        let (_, cancel_job) = control
            .accept(cancel, &CoreAction::CancelInstall { request_id: stop })
            .unwrap();
        control.execute(cancel_job.unwrap()).await.unwrap();
        assert!(
            matches!(control.request_status(cancel).unwrap(), Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_INSTALL_NOT_ACTIVE")
        );
        control.execute(stop_job.unwrap()).await.unwrap();
        assert!(matches!(
            control.request_status(stop).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Stopped {},
                ..
            })
        ));
    }

    #[tokio::test]
    async fn cancelling_a_shared_install_participant_preserves_other_authorized_requests() {
        for cancel_leader in [false, true] {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().join("store");
            let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
            let control = Arc::new(CoreControl::new(root, configuration, Uuid::new_v4()));
            let release = Arc::new(tokio::sync::Notify::new());
            control.hold_installer(release.clone());
            let first = Uuid::new_v4();
            let second = Uuid::new_v4();
            let (_, job) = control.accept(first, &CoreAction::Install {}).unwrap();
            let worker = control.clone();
            let first_task = tokio::spawn(async move { worker.execute(job.unwrap()).await });
            tokio::task::yield_now().await;
            let (_, job) = control.accept(second, &CoreAction::Install {}).unwrap();
            let worker = control.clone();
            let second_task = tokio::spawn(async move { worker.execute(job.unwrap()).await });
            tokio::task::yield_now().await;
            let (cancelled, surviving, cancelled_task, surviving_task) = if cancel_leader {
                (first, second, first_task, second_task)
            } else {
                (second, first, second_task, first_task)
            };
            control.cancel_install(cancelled).unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(2), cancelled_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(matches!(
                control.request_status(cancelled).unwrap(),
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Cancelled {},
                    ..
                })
            ));
            assert!(matches!(
                control.request_status(surviving).unwrap(),
                Some(CoreRequestStatus::Pending { .. })
            ));
            release.notify_one();
            tokio::time::timeout(std::time::Duration::from_secs(2), surviving_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                matches!(control.request_status(surviving).unwrap(), Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_DOWNLOAD_FAILED")
            );
        }
    }

    #[tokio::test]
    async fn background_admission_is_bounded_but_replays_and_cancellation_remain_available() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
        let control = CoreControl::new(root, configuration, Uuid::new_v4());
        let mut jobs = Vec::new();
        let mut ids = Vec::new();
        for _ in 0..32 {
            let id = Uuid::new_v4();
            jobs.push(
                control
                    .accept(id, &CoreAction::Install {})
                    .unwrap()
                    .1
                    .unwrap(),
            );
            ids.push(id);
        }
        assert!(matches!(
            control.accept(Uuid::new_v4(), &CoreAction::Stop {}),
            Err(Error::Invalid("CORE_OPERATION_LIMIT"))
        ));
        assert!(
            control
                .accept(ids[0], &CoreAction::Install {})
                .unwrap()
                .1
                .is_none()
        );
        let job = control
            .accept(
                Uuid::new_v4(),
                &CoreAction::CancelInstall { request_id: ids[0] },
            )
            .unwrap()
            .1
            .unwrap();
        control.execute(job).await.unwrap();
        control.execute(jobs.remove(0)).await.unwrap();
        assert!(matches!(
            control.request_status(ids[0]).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Cancelled {},
                ..
            })
        ));
        assert!(
            control
                .accept(Uuid::new_v4(), &CoreAction::Stop {})
                .unwrap()
                .1
                .is_some()
        );
    }

    #[tokio::test]
    async fn one_admission_receipt_replay_and_unknown_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
        let control = CoreControl::new(root.clone(), configuration.clone(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
        assert!(matches!(status, CoreRequestStatus::Pending { .. }));
        assert!(!control.idle_allowed().unwrap());
        for _ in 0..3 {
            let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
            assert!(job.is_none());
            assert!(matches!(status, CoreRequestStatus::Pending { .. }));
        }
        control.execute(job.unwrap()).await.unwrap();
        assert!(control.idle_allowed().unwrap());
        let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
        assert!(job.is_none());
        assert!(matches!(
            status,
            CoreRequestStatus::Complete {
                outcome: CoreOutcome::Stopped {},
                ..
            }
        ));
        let interrupted = Uuid::new_v4();
        let (_, job) = control.accept(interrupted, &CoreAction::Stop {}).unwrap();
        drop(job); // Lost task before execution, still in the same owner epoch.
        assert!(matches!(
            control.request_status(interrupted).unwrap(),
            Some(CoreRequestStatus::Indeterminate {})
        ));
        assert!(
            !control
                .accept(interrupted, &CoreAction::Stop {})
                .unwrap()
                .1
                .is_some()
        );
        drop(control);
        drop(configuration);
        let configuration = Arc::new(Configuration::new(Store::open(&root).unwrap()));
        let recovered = CoreControl::new(root, configuration, Uuid::new_v4());
        assert!(matches!(
            recovered.request_status(interrupted).unwrap(),
            Some(CoreRequestStatus::Indeterminate {})
        ));
        assert!(matches!(
            recovered.request_status(id).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Stopped {},
                ..
            })
        ));
        assert!(!recovered.idle_allowed().unwrap());
    }

    #[tokio::test]
    async fn failed_receipt_write_is_indeterminate_without_reexecution() {
        use std::{fs::OpenOptions, os::windows::fs::OpenOptionsExt};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
        let control = CoreControl::new(root.clone(), configuration, Uuid::new_v4());
        let id = Uuid::new_v4();
        let (_, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
        let held = OpenOptions::new()
            .read(true)
            .share_mode(1)
            .open(root.join(format!("state/core-requests/{id}.json")))
            .unwrap();
        assert!(control.execute(job.unwrap()).await.is_err());
        drop(held);
        let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
        assert!(job.is_none());
        assert!(matches!(status, CoreRequestStatus::Indeterminate {}));
    }
}
