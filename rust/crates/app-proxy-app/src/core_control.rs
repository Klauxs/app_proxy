//! Durable admission and result lookup for serialized core operations.
use crate::{configuration::Configuration, core_manager::CoreManager};
use app_proxy_core::core_control::{CoreAction, CoreOutcome, CoreRequestStatus};
use app_proxy_windows::{Error, Result, core_requests::CoreRequestPhase, core_state::CoreState};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

pub(crate) struct CoreControl {
    manager: CoreManager,
    configuration: Arc<Configuration>,
    epoch: Uuid,
    active: Arc<Mutex<HashSet<Uuid>>>,
}

impl CoreControl {
    pub fn new(root: PathBuf, configuration: Arc<Configuration>, epoch: Uuid) -> Self {
        Self {
            manager: CoreManager::new(root, configuration.clone()),
            configuration,
            epoch,
            active: Arc::new(Mutex::new(HashSet::new())),
        }
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
        let (phase, is_new) = self
            .configuration
            .lock()?
            .begin_core_request(id, self.epoch, action)?;
        if is_new {
            active.insert(id);
        }
        let job = is_new.then(|| CoreJob {
            id,
            action: action.clone(),
            active: self.active.clone(),
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
        active: &HashSet<Uuid>,
    ) -> CoreRequestStatus {
        match phase {
            CoreRequestPhase::Pending { epoch } if epoch == self.epoch && active.contains(&id) => {
                CoreRequestStatus::Pending {}
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

    pub async fn execute(&self, job: CoreJob) -> Result<()> {
        let result = match job.action.clone() {
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
        self.configuration
            .lock()?
            .finish_core_request(job.id, self.epoch, outcome)
    }

    pub fn idle_allowed(&self) -> Result<bool> {
        let store = self.configuration.lock()?;
        Ok(matches!(
            store.core_state()?,
            CoreState::Stopped {} | CoreState::Down { .. }
        ) && !store.has_unresolved_core_requests()?)
    }

    pub fn snapshot(&self) -> Result<crate::core_manager::CoreSnapshot> {
        self.manager.snapshot()
    }
}

// A non-cloneable admission token: dropping before/during execution changes a
// durable Pending to Indeterminate even within the same coordinator epoch.
pub(crate) struct CoreJob {
    id: Uuid,
    action: CoreAction,
    active: Arc<Mutex<HashSet<Uuid>>>,
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
    async fn one_admission_receipt_replay_and_unknown_cancellation() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let configuration = Arc::new(Configuration::new(Store::create(&root).unwrap()));
        let control = CoreControl::new(root.clone(), configuration.clone(), Uuid::new_v4());
        let id = Uuid::new_v4();
        let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
        assert!(matches!(status, CoreRequestStatus::Pending {}));
        assert!(!control.idle_allowed().unwrap());
        for _ in 0..3 {
            let (status, job) = control.accept(id, &CoreAction::Stop {}).unwrap();
            assert!(job.is_none());
            assert!(matches!(status, CoreRequestStatus::Pending {}));
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
