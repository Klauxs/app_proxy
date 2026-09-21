//! The shared instance execution path. Accepted work is detached from its client;
//! only a persisted cancellation can prevent a not-yet-dispatched application.
use crate::{configuration::Configuration, core_manager::CoreManager};
use app_proxy_core::{launch::*, model::*, template};
use app_proxy_windows::{
    Error, Result, identity, installation,
    instance_data::PreparedData,
    instance_process::{InstanceRelation, InstanceTarget, ProcessRole, ProxyArguments},
    instance_resource::{
        InstanceResource, ResourceOwner, ResourcePhase, ResourceRegistry, ResourceReservation,
    },
    process::{self, SpawnFailure, SpawnSpec},
    process_query,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

mod event;
mod guard;
pub use guard::{GuardObservation, GuardScan};
mod observation;
pub use observation::{InstanceObservation, RuntimeStatus};

pub struct LaunchEngine {
    configuration: Arc<Configuration>,
    manager: Arc<CoreManager>,
    resources: ResourceRegistry,
    epoch: Uuid,
    active: Mutex<HashSet<Uuid>>,
    completed: tokio::sync::Notify,
    guard_resolution: Arc<tokio::sync::Semaphore>,
    #[cfg(test)]
    before_guard_resolution: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_guard_scan: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_guard_target_read: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_dispatch: Mutex<Option<Arc<tokio::sync::Notify>>>,
    #[cfg(test)]
    after_spawn: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_guard_stop: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    before_guard_receipt: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    #[cfg(test)]
    after_guard_stop: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}
impl LaunchEngine {
    pub(crate) fn drained(&self) -> Result<bool> {
        Ok(self
            .active
            .lock()
            .map_err(|_| Error::Invalid("LAUNCH_WORKER_FAILED"))?
            .is_empty())
    }
    pub fn new(
        configuration: Arc<Configuration>,
        manager: Arc<CoreManager>,
        epoch: Uuid,
    ) -> Result<Arc<Self>> {
        Self::with_resources(configuration, manager, epoch, ResourceRegistry::open()?)
    }

    pub(crate) fn with_resources(
        configuration: Arc<Configuration>,
        manager: Arc<CoreManager>,
        epoch: Uuid,
        resources: ResourceRegistry,
    ) -> Result<Arc<Self>> {
        if epoch.is_nil() || !Arc::ptr_eq(&configuration, &manager.configuration) {
            return Err(Error::Invalid("LAUNCH_OWNER_MISMATCH"));
        }
        configuration.lock()?.recover_launches(epoch)?;
        Ok(Arc::new(Self {
            configuration,
            manager,
            resources,
            epoch,
            active: Mutex::new(HashSet::new()),
            completed: tokio::sync::Notify::new(),
            guard_resolution: Arc::new(tokio::sync::Semaphore::new(1)),
            #[cfg(test)]
            before_guard_resolution: Mutex::new(None),
            #[cfg(test)]
            after_guard_scan: Mutex::new(None),
            #[cfg(test)]
            after_guard_target_read: Mutex::new(None),
            #[cfg(test)]
            before_dispatch: Mutex::new(None),
            #[cfg(test)]
            after_spawn: Mutex::new(None),
            #[cfg(test)]
            before_guard_stop: Mutex::new(None),
            #[cfg(test)]
            before_guard_receipt: Mutex::new(None),
            #[cfg(test)]
            after_guard_stop: Mutex::new(None),
        }))
    }

    /// Must be invoked on the coordinator runtime. Return of this ACK is not
    /// success; clients query the durable attempt until it reaches a result.
    pub async fn submit(self: &Arc<Self>, request: LaunchRequest) -> Result<LaunchAttempt> {
        self.submit_at_revision(request, None).await
    }

    pub(crate) async fn submit_at_revision(
        self: &Arc<Self>,
        request: LaunchRequest,
        expected_revision: Option<u64>,
    ) -> Result<LaunchAttempt> {
        self.submit_checked(request, expected_revision, None, None)
    }

    /// Internal Guard adapter. A caller-supplied origin label on ordinary launch
    /// never grants stop permission; this path binds an exact observed target.
    pub fn submit_guard(
        self: &Arc<Self>,
        request: LaunchRequest,
        revision: u64,
        target: GuardTarget,
    ) -> Result<LaunchAttempt> {
        let _timing = app_proxy_windows::diagnostic_timing::Span::new("launch.admit", || {
            format!("{}:{}", request.request_id, target.process.pid)
        });
        self.submit_checked(request, Some(revision), Some(target), None)
    }

    fn submit_checked(
        self: &Arc<Self>,
        request: LaunchRequest,
        expected_revision: Option<u64>,
        guard_target: Option<GuardTarget>,
        mut pinned: Option<event::ObservedTarget>,
    ) -> Result<LaunchAttempt> {
        #[cfg(test)]
        if pinned.is_some()
            && let Some(hook) = self.before_guard_stop.lock().unwrap().clone()
        {
            hook();
        }
        let gate_timing =
            app_proxy_windows::diagnostic_timing::Span::new("launch.admission_gate", || {
                request.request_id.to_string()
            });
        let mut active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("LAUNCH_OWNER_FAILED"))?;
        let mut store = self.configuration.lock()?;
        drop(gate_timing);
        if let (Some(observed), Some(target)) = (pinned.as_mut(), guard_target.as_ref()) {
            if active.len() >= 32 {
                return Err(Error::Invalid("LAUNCH_OPERATION_LIMIT"));
            }
            self.stop_event_before_admission(&store, &request, target, observed)?;
        }
        // Everything below is restart bookkeeping and runs after an event stop.
        let already_stopped = pinned.as_ref().is_some_and(|p| p.stopped.is_some());
        let result = (|| {
            let _admit_timing =
                app_proxy_windows::diagnostic_timing::Span::new("launch.bookkeeping", || {
                    request.request_id.to_string()
                });
            if already_stopped
                && let Some(existing) = store.launch_attempts()?.into_iter().find(|a| {
                    a.instance_id == request.instance_id
                        && a.reserves_instance()
                        && (active.contains(&a.id)
                            || matches!(&a.phase, LaunchPhase::Confirmed { process } if process::is_running_exact(process).unwrap_or(false)))
                })
            {
                // Stop each newly observed unproxied main immediately; keep
                // the one restart/session already owned by this coordinator.
                app_proxy_windows::diagnostic_timing::mark("event.stop_coalesced", || {
                    format!("{}:{}", guard_target.as_ref().unwrap().process.pid, existing.id)
                });
                return Ok(existing);
            }
            let admit = |store: &mut app_proxy_windows::store::Store| match &guard_target {
                Some(_) if pinned.as_ref().and_then(|p| p.stopped.as_ref()).is_some() => store
                    .begin_stopped_guard_launch(
                        &request,
                        self.epoch,
                        expected_revision.unwrap(),
                        pinned.as_ref().unwrap().stopped.as_ref().unwrap(),
                    ),
                Some(target) => store.begin_guard_launch(
                    &request,
                    self.epoch,
                    expected_revision.unwrap(),
                    target.clone(),
                ),
                None => store.begin_launch_at_revision(&request, self.epoch, expected_revision),
            };
            if store.launch_request(request.request_id)?.is_some() {
                return Ok(admit(&mut store)?.attempt);
            }
            let recover_timing =
                app_proxy_windows::diagnostic_timing::Span::new("launch.recover_history", || {
                    request.request_id.to_string()
                });
            store.recover_config_requests()?;
            for attempt in store.launch_attempts()? {
                if attempt.instance_id == request.instance_id && !active.contains(&attempt.id) {
                    self.reconcile(&mut store, &attempt)?;
                }
            }
            drop(recover_timing);
            if active.len() >= 32 {
                return Err(Error::Invalid("LAUNCH_OPERATION_LIMIT"));
            }
            for prior in store.launch_attempts()? {
                if prior.instance_id == request.instance_id
                    && matches!(prior.phase, LaunchPhase::Confirmed { .. })
                    && !prior.session_exited
                    && !store.observe_launch_exit(prior.id)?
                {
                    let session = identity::current()?.session_id;
                    if prior
                        .binding
                        .as_ref()
                        .is_some_and(|b| b.session_id != session)
                    {
                        return Err(Error::Invalid("INSTANCE_RUNNING_IN_OTHER_SESSION"));
                    }
                    let current = dependency_digest(&store.load()?, request.instance_id)?;
                    if prior
                        .binding
                        .as_ref()
                        .is_none_or(|binding| current != binding.dependency_digest)
                    {
                        return Err(Error::Invalid("INSTANCE_RUNNING_WITH_OTHER_CONFIG"));
                    }
                }
            }
            let record_timing =
                app_proxy_windows::diagnostic_timing::Span::new("launch.record", || {
                    request.request_id.to_string()
                });
            let admission = admit(&mut store)?;
            drop(record_timing);
            if admission.is_new {
                active.insert(admission.attempt.id);
                let mut job = Job {
                    engine: self.clone(),
                    id: admission.attempt.id,
                    pinned,
                };
                tokio::spawn(async move {
                    // A panic/drop leaves a durable stage. Status/recovery never
                    // interprets it as proof that creation did not happen.
                    let pinned = job.pinned.take();
                    job.engine.execute(job.id, pinned).await;
                });
            }
            Ok(admission.attempt)
        })();
        result.map_err(|error| {
            if already_stopped {
                app_proxy_windows::diagnostic_timing::mark("event.post_stop_record_failed", || {
                    format!("{}:{error}", request.request_id)
                });
                Error::Invalid(app_proxy_core::error_code::GUARD_STOPPED_RECORD_FAILED)
            } else {
                error
            }
        })
    }

    pub fn status(&self, request: Uuid) -> Result<Option<LaunchAttempt>> {
        let active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("LAUNCH_OWNER_FAILED"))?;
        let mut store = self.configuration.lock()?;
        let Some(mut attempt) = store.launch_request(request)? else {
            return Ok(None);
        };
        if !active.contains(&attempt.id) {
            self.reconcile(&mut store, &attempt)?;
            attempt = store.launch_request(request)?.unwrap();
        }
        if !active.contains(&attempt.id) && attempt.phase.before_spawn() {
            attempt = store.advance_launch(
                attempt.id,
                attempt.epoch,
                &attempt.phase,
                LaunchPhase::Failed {
                    code: if attempt
                        .guard_correction
                        .as_ref()
                        .is_some_and(|g| g.stop_started_at.is_some())
                    {
                        "GUARD_CORRECTION_INTERRUPTED"
                    } else {
                        "LAUNCH_EXECUTION_INTERRUPTED"
                    }
                    .into(),
                },
            )?;
        }
        if !active.contains(&attempt.id)
            && matches!(
                attempt.phase,
                LaunchPhase::SpawnRequested {} | LaunchPhase::AwaitingIdentity {}
            )
        {
            attempt = store.advance_launch(
                attempt.id,
                attempt.epoch,
                &attempt.phase,
                LaunchPhase::Indeterminate {},
            )?;
        }
        if matches!(attempt.phase, LaunchPhase::Confirmed { .. }) && !attempt.session_exited {
            store.observe_launch_exit(attempt.id)?;
            attempt = store.launch_request(request)?.unwrap();
        }
        Ok(Some(attempt))
    }

    pub fn cancel(&self, request: Uuid) -> Result<LaunchAttempt> {
        self.configuration.lock()?.request_launch_cancel(request)
    }

    pub(crate) async fn completed(&self) {
        self.completed.notified().await;
    }

    #[cfg(test)]
    pub(crate) fn hold_dispatch(&self, gate: Arc<tokio::sync::Notify>) {
        *self.before_dispatch.lock().unwrap() = Some(gate);
    }

    /// Reconcile idle attempts and observe exact exits; never create or stop an
    /// application from coordinator maintenance.
    pub(crate) fn refresh_sessions(&self) -> Result<()> {
        let ids: Vec<_> = self
            .configuration
            .lock()?
            .launch_attempts()?
            .into_iter()
            .filter(|a| a.reserves_instance() || a.resource_pending)
            .map(|a| a.id)
            .collect();
        for id in ids {
            self.status(id)?;
        }
        Ok(())
    }

    fn reconcile(
        &self,
        store: &mut app_proxy_windows::store::Store,
        attempt: &LaunchAttempt,
    ) -> Result<()> {
        if attempt.resource_pending
            || matches!(
                attempt.phase,
                LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
                    | LaunchPhase::Confirmed { .. }
            )
        {
            match self.resources.reconcile_launch(store, attempt.id) {
                // A timed-out worker may still hold the lock and finish its write.
                Err(Error::Invalid(
                    app_proxy_core::error_code::INSTANCE_RESOURCE_BUSY
                    | app_proxy_core::error_code::PACKAGE_REQUEST_BUSY,
                )) => Ok(()),
                other => other,
            }
        } else {
            Ok(())
        }
    }

    async fn execute(&self, id: Uuid, pinned: Option<event::ObservedTarget>) {
        let mut reservation = None;
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            self.prepare_and_spawn(id, &mut reservation, pinned),
        )
        .await;
        let error = match result {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error,
            Err(_) => Error::Invalid("LAUNCH_TIMEOUT"),
        };
        let _ = self.end_failed(id, reservation.as_mut(), &error);
    }

    fn end_failed(
        &self,
        id: Uuid,
        reservation: Option<&mut ResourceReservation>,
        error: &Error,
    ) -> Result<()> {
        let released = if let Some(reservation) = reservation {
            if let Some(claim) = reservation.claim().filter(|c| {
                c.owner.attempt_id == id
                    && c.owner.epoch == self.epoch
                    && c.phase == (ResourcePhase::Reserved {})
            }) {
                reservation.release_before_spawn(claim.owner).is_ok()
            } else {
                true
            }
        } else {
            true
        };
        let mut store = self.configuration.lock()?;
        let attempt = store
            .launch_request(id)?
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        if attempt.phase.before_spawn() {
            let phase = if attempt.cancel_requested
                && released
                && attempt
                    .guard_correction
                    .as_ref()
                    .is_some_and(|g| g.stop_started_at.is_some())
            {
                LaunchPhase::Failed {
                    code: "GUARD_CANCELLED_AFTER_STOP_REQUEST".into(),
                }
            } else if attempt.cancel_requested && released {
                LaunchPhase::Cancelled {}
            } else {
                LaunchPhase::Failed {
                    code: if released {
                        if matches!(attempt.phase, LaunchPhase::PreparingProxy {})
                            && attempt
                                .guard_correction
                                .as_ref()
                                .is_some_and(|g| g.stop_confirmed)
                        {
                            "GUARD_STOPPED_PROXY_UNAVAILABLE"
                        } else {
                            error_code(error)
                        }
                    } else {
                        "INSTANCE_RESOURCE_RELEASE_FAILED"
                    }
                    .into(),
                }
            };
            store.advance_launch(id, self.epoch, &attempt.phase, phase)?;
        } else if matches!(
            attempt.phase,
            LaunchPhase::SpawnRequested {} | LaunchPhase::AwaitingIdentity {}
        ) {
            store.advance_launch(
                id,
                self.epoch,
                &attempt.phase,
                LaunchPhase::Indeterminate {},
            )?;
        }
        Ok(())
    }

    fn advance(&self, id: Uuid, expected: LaunchPhase, next: LaunchPhase) -> Result<()> {
        self.configuration
            .lock()?
            .advance_launch(id, self.epoch, &expected, next)?;
        Ok(())
    }

    async fn prepare_and_spawn(
        &self,
        id: Uuid,
        reservation: &mut Option<ResourceReservation>,
        pinned: Option<event::ObservedTarget>,
    ) -> Result<()> {
        let _timing =
            app_proxy_windows::diagnostic_timing::Span::new("launch.total", || id.to_string());
        let timing =
            app_proxy_windows::diagnostic_timing::Span::new("launch.prepare", || id.to_string());
        let from_event = pinned.is_some();
        // Event stops are already confirmed and saved at CheckingInstance.
        // Only their restart preparation remains; never stop the target again.
        if !from_event {
            self.advance(id, LaunchPhase::Accepted {}, LaunchPhase::Resolving {})?;
        }
        let (snapshot, instance_id) = {
            let mut store = self.configuration.lock()?;
            store.recover_config_requests()?;
            (
                store.load()?,
                store.launch_request(id)?.unwrap().instance_id,
            )
        };
        let (app, instance) = entries(&snapshot, instance_id)?;
        let digest = dependency_digest(&snapshot, instance_id)?;
        let (application, data, pinned, stopped) = if let Some(observed) = pinned {
            if observed.instance_id != instance_id || observed.revision != snapshot.revision {
                return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
            }
            // Revalidate registration while reusing held image/data pins.
            observed.application.verify_current()?;
            (
                observed.application,
                observed.data,
                Some(observed.pinned),
                observed.stopped,
            )
        } else {
            let locator = app.locator.clone();
            let resolving =
                app_proxy_windows::diagnostic_timing::Span::new("launch.resolve", || {
                    id.to_string()
                });
            let application = tokio::task::spawn_blocking(move || installation::resolve(&locator))
                .await
                .map_err(|_| Error::Invalid("LAUNCH_RESOLUTION_INTERRUPTED"))??;
            drop(resolving);
            let preparing =
                app_proxy_windows::diagnostic_timing::Span::new("launch.data", || id.to_string());
            app_proxy_windows::creation_guard::ensure_plain_creation(application.executable())?;
            let data = self
                .configuration
                .lock()?
                .prepare_instance_data(instance_id, application.package())?;
            drop(preparing);
            self.advance(
                id,
                LaunchPhase::Resolving {},
                LaunchPhase::CheckingInstance {},
            )?;
            (application, data, None, None)
        };
        let resources =
            app_proxy_windows::diagnostic_timing::Span::new("launch.resources", || id.to_string());
        let resource = InstanceResource::resolve(&application, data.as_ref())?;
        let resource_key = resource.digest();
        let mut acquired = self.resources.acquire(resource)?;
        acquired.reconcile(&mut *self.configuration.lock()?)?;
        let owner = ResourceOwner {
            store_id: snapshot.store_id,
            attempt_id: id,
            epoch: self.epoch,
        };
        if let Some(claim) = acquired.claim() {
            let previous = claim.owner;
            match claim.phase {
                ResourcePhase::Confirmed { .. } => acquired.release_exited(previous)?,
                ResourcePhase::Reserved {} if previous.store_id == owner.store_id => {
                    let store = self.configuration.lock()?;
                    let old = store.launch_request(previous.attempt_id)?;
                    // Exclusive lock + protected Reserved (no dispatch nonce)
                    // proves creation was never authorized, even if this store's
                    // terminal preparation receipt has since expired. A new owner
                    // also invalidates any stale, never-authorized dispatch token.
                    if old.is_none_or(|a| {
                        a.epoch == previous.epoch
                            && matches!(
                                a.phase,
                                LaunchPhase::Failed { .. } | LaunchPhase::Cancelled {}
                            )
                    }) {
                        acquired.release_before_spawn(previous)?;
                    }
                }
                _ => {}
            }
        }
        acquired.reserve(owner)?;
        *reservation = Some(acquired);
        if let Some(receipt) = stopped.as_ref() {
            reservation
                .as_mut()
                .unwrap()
                .record_observed_guard_stop(owner, receipt)?;
        }
        drop(resources);
        drop(timing);
        let attempt = self.configuration.lock()?.launch_request(id)?.unwrap();
        // Ordinary launches delegate external single-instance/data locking to
        // the application. Only Guard must establish exclusivity around a stop
        // and correction; request/owned-session deduplication remains separate.
        let guard_launch = attempt.origin == LaunchOrigin::Guard;
        let (application, data) =
            if let Some(correction) = attempt.guard_correction.filter(|c| !c.stop_confirmed) {
                self.correct_guard(
                    id,
                    application,
                    data,
                    app.template_ref,
                    (&correction.target, pinned),
                    reservation,
                )
                .await?
            } else {
                (application, data)
            };
        if guard_launch && !from_event {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match check_occupancy(&application, data.as_ref(), app.template_ref).await {
                        Ok(()) => return Ok(()),
                        Err(Error::Invalid(
                            app_proxy_core::error_code::INSTANCE_EXTERNALLY_RUNNING,
                        )) => {
                            return Err(Error::Invalid(
                                app_proxy_core::error_code::INSTANCE_EXTERNALLY_RUNNING,
                            ));
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(30)).await,
                    }
                }
            })
            .await
            .map_err(|_| Error::Invalid("GUARD_RESTART_NOT_CLEAR"))??;
        }
        if from_event {
            // This is a spawn precondition, not a reason to delay stopping a
            // positively identified unproxied process.
            app_proxy_windows::creation_guard::ensure_plain_creation(application.executable())?;
        }
        self.advance(
            id,
            LaunchPhase::CheckingInstance {},
            LaunchPhase::PreparingProxy {},
        )?;
        let network = match instance.network {
            NetworkBinding::Direct {} => LaunchNetwork::Direct {},
            NetworkBinding::Profile { profile_id } => {
                let url = snapshot.settings.test_url.clone();
                let statuses = snapshot.settings.health_policy.expected_statuses.clone();
                let manager = self.manager.clone();
                let ready = tokio::spawn(async move {
                    manager
                        .ensure_with(&[profile_id], profile_id, |endpoint| async move {
                            crate::proxy_health::check(&endpoint, &url, &statuses)
                                .await
                                .map(|_| ())
                                .map_err(|_| Error::Invalid("CORE_PROXY_HEALTH_FAILED"))
                        })
                        .await
                })
                .await
                .map_err(|_| Error::Invalid("LAUNCH_CORE_PREPARATION_INTERRUPTED"))??;
                let profile = snapshot
                    .profiles
                    .iter()
                    .find(|p| p.id == profile_id)
                    .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
                LaunchNetwork::Profile {
                    profile_id,
                    generation: ready.generation,
                    endpoint: profile.endpoint.clone(),
                }
            }
        };
        self.advance(
            id,
            LaunchPhase::PreparingProxy {},
            LaunchPhase::PreparingData {},
        )?;
        let output = {
            let store = self.configuration.lock()?;
            if dependency_digest(&store.load()?, instance_id)? != digest {
                return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
            }
            template::compile(
                &snapshot,
                instance_id,
                template::TemplatePaths {
                    executable: application.executable(),
                    instance_root: data.as_ref().map(|d| d.paths.root.as_path()),
                },
                |key| {
                    store
                        .read_secret(key)
                        .map_err(|_| ValidationError("LAUNCH_SECRET_UNAVAILABLE"))
                },
            )
            .map_err(|e| Error::Invalid(e.0))?
        };
        self.manager
            .ready_launch(
                id,
                self.epoch,
                LaunchBinding {
                    dependency_digest: digest,
                    resource_key,
                    executable: application.executable().to_owned(),
                    image: application.image().clone(),
                    session_id: identity::current()?.session_id,
                    network,
                },
            )
            .await?;
        #[cfg(test)]
        {
            let gate = self.before_dispatch.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.notified().await;
            }
        }
        if guard_launch {
            tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match check_occupancy(&application, data.as_ref(), app.template_ref).await {
                        Ok(()) => return Ok(()),
                        Err(Error::Invalid(
                            app_proxy_core::error_code::INSTANCE_EXTERNALLY_RUNNING,
                        )) => {
                            return Err(Error::Invalid(
                                app_proxy_core::error_code::INSTANCE_EXTERNALLY_RUNNING,
                            ));
                        }
                        Err(_) => tokio::time::sleep(Duration::from_millis(30)).await,
                    }
                }
            })
            .await
            .map_err(|_| Error::Invalid("GUARD_RESTART_NOT_CLEAR"))??;
        }
        application.verify_current()?;
        let dispatch = {
            let mut store = self.configuration.lock()?;
            store.recover_config_requests()?;
            if dependency_digest(&store.load()?, instance_id)? != digest {
                return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
            }
            if let Some(package) = application.package() {
                store.dispatch_package_launch(id, self.epoch, package)?
            } else {
                store.dispatch_launch(id, self.epoch)?
            }
        };
        let mut reservation = reservation.take().unwrap();
        let configuration = self.configuration.clone();
        #[cfg(test)]
        let after_spawn = self.after_spawn.lock().unwrap().clone();
        let spec = SpawnSpec {
            exe: application.executable().to_owned(),
            args: output.args,
            cwd: output.cwd,
            environment: output.environment,
        };
        tokio::task::spawn_blocking(move || {
            let _data = data;
            let spawned = match reservation.authorize_spawn(dispatch) {
                Ok(permit) if application.package().is_some() => {
                    spawn_package(&configuration, id, owner.epoch, permit, &application, spec)
                }
                Ok(permit) => {
                    process::spawn_for_attempt(permit, spec).map(|started| started.identity)
                }
                Err(error) => Err(error),
            };
            #[cfg(test)]
            if let Some(hook) = after_spawn {
                hook();
            }
            match spawned {
                Ok(identity) => {
                    reservation.confirm(owner, identity.clone())?;
                    configuration
                        .lock()?
                        .confirm_launch(id, owner.epoch, identity)?;
                    reservation.reconcile(&mut *configuration.lock()?)?;
                    Ok(())
                }
                Err(SpawnFailure::NotCreated { evidence, error }) => {
                    configuration
                        .lock()?
                        .fail_launch_not_created(id, owner.epoch, &evidence)?;
                    reservation.reconcile(&mut *configuration.lock()?)?;
                    Err(error)
                }
                Err(SpawnFailure::Indeterminate { error }) => Err(error),
            }
        })
        .await
        .map_err(|_| Error::Invalid("LAUNCH_CREATION_INTERRUPTED"))??;
        Ok(())
    }

    async fn correct_guard(
        &self,
        id: Uuid,
        application: installation::ResolvedApplication,
        data: Option<PreparedData>,
        template: Template,
        target: (
            &GuardTarget,
            Option<app_proxy_windows::native_process::PinnedProcess>,
        ),
        reservation: &mut Option<ResourceReservation>,
    ) -> Result<(installation::ResolvedApplication, Option<PreparedData>)> {
        let (target, pinned) = target;
        let _timing = app_proxy_windows::diagnostic_timing::Span::new("guard.correct", || {
            format!("{}:{}", id, target.process.pid)
        });
        let instance = InstanceTarget::new(&application, data.as_ref(), template)?;
        let endpoint = std::net::SocketAddr::new(target.endpoint.host, target.endpoint.port);
        let pinned = match pinned {
            Some(pinned) => pinned,
            None => {
                let mut pinned =
                    app_proxy_windows::native_process::PinnedProcess::open(target.process.pid)?;
                if pinned.identity() != &target.process {
                    return Err(Error::IdentityMismatch);
                }
                pinned.read_arguments()?;
                pinned
            }
        };
        #[cfg(test)]
        if let Some(hook) = self.after_guard_target_read.lock().unwrap().clone() {
            hook();
        }
        let observed = instance.inspect_pinned(&pinned, endpoint)?;
        if observed.identity != target.process
            || observed.role != ProcessRole::Main
            || observed.relation != InstanceRelation::Target
            || observed.proxy != ProxyArguments::Mismatched
        {
            return Err(Error::Invalid("GUARD_TARGET_NOT_UNPROXIED"));
        }
        #[cfg(test)]
        if let Some(hook) = self.before_guard_stop.lock().unwrap().clone() {
            hook();
        }
        let mut held = reservation.take().unwrap();
        let configuration = self.configuration.clone();
        let epoch = self.epoch;
        #[cfg(test)]
        let after_guard_stop = self.after_guard_stop.lock().unwrap().clone();
        #[cfg(test)]
        let before_guard_receipt = self.before_guard_receipt.lock().unwrap().clone();
        let stop_queued =
            app_proxy_windows::diagnostic_timing::Span::new("guard.stop_dispatch_queue", || {
                id.to_string()
            });
        let (application, data, held, result) = tokio::task::spawn_blocking(move || {
            drop(stop_queued);
            let result = (|| {
                let timing =
                    app_proxy_windows::diagnostic_timing::Span::new("guard.stop_authorize", || {
                        id.to_string()
                    });
                let dispatch = configuration.lock()?.dispatch_guard_stop(id, epoch)?;
                let permit = held.authorize_guard_stop(dispatch)?;
                drop(timing);
                let receipt =
                    app_proxy_windows::process_stop::stop_guarded_pinned(permit, &pinned)?;
                if !matches!(
                    receipt.outcome(),
                    app_proxy_windows::process_stop::StopOutcome::Exited
                        | app_proxy_windows::process_stop::StopOutcome::Forced
                ) {
                    return Err(Error::Invalid("GUARD_TARGET_EXITED_BEFORE_STOP"));
                }
                #[cfg(test)]
                if let Some(hook) = &before_guard_receipt {
                    hook();
                }
                configuration.lock()?.confirm_guard_stop(&receipt)?;
                #[cfg(test)]
                if let Some(hook) = &after_guard_stop {
                    hook();
                }
                Ok(())
            })();
            (application, data, held, result)
        })
        .await
        .map_err(|_| Error::Invalid("GUARD_STOP_INTERRUPTED"))?;
        *reservation = Some(held);
        result?;
        let store = self.configuration.lock()?;
        let attempt = store.launch_request(id)?.unwrap();
        if attempt.expected_revision != Some(store.load()?.revision) {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        if attempt.cancel_requested {
            return Err(Error::Invalid("LAUNCH_CANCEL_REQUESTED"));
        }
        Ok((application, data))
    }
}

fn spawn_package(
    configuration: &Configuration,
    id: Uuid,
    epoch: Uuid,
    permit: app_proxy_windows::instance_resource::AuthorizedSpawn<'_>,
    application: &installation::ResolvedApplication,
    spec: SpawnSpec,
) -> std::result::Result<app_proxy_core::ProcessIdentity, SpawnFailure> {
    use app_proxy_windows::package_launch::PackageOutcome;
    let unknown = |error| SpawnFailure::Indeterminate { error };
    let helper = std::env::current_exe()
        .map_err(|e| unknown(e.into()))?
        .with_file_name("app-proxy-host.exe");
    let ticket = configuration
        .lock()
        .map_err(unknown)?
        .prepare_package_launch(permit, application, &helper, spec)?;
    let started = std::time::Instant::now();
    let cancelled = || -> Result<bool> {
        Ok(configuration
            .lock()?
            .launch_request(id)?
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?
            .cancel_requested)
    };
    let already_cancelled = cancelled().map_err(unknown)?;
    let mut activations = 0;
    let mut last_activation = std::time::Instant::now();
    if !already_cancelled {
        configuration
            .lock()
            .map_err(unknown)?
            .advance_launch(
                id,
                epoch,
                &LaunchPhase::SpawnRequested {},
                LaunchPhase::AwaitingIdentity {},
            )
            .map_err(unknown)?;
        // Even an activation error may have started a helper. Only the gate and
        // its durable receipt can resolve that uncertainty.
        let activation = app_proxy_windows::package::activate_launch(
            application.package().unwrap(),
            &helper,
            &ticket.request_path(),
        );
        app_proxy_windows::diagnostic_timing::mark("package.activation", || {
            format!(
                "{id}:{}",
                activation
                    .as_ref()
                    .map(|_| "accepted".to_owned())
                    .unwrap_or_else(|e| e.to_string())
            )
        });
        activations = 1;
        last_activation = std::time::Instant::now();
    }
    loop {
        let ending = cancelled().map_err(unknown)? || started.elapsed() >= Duration::from_secs(22);
        let outcome = if ending {
            ticket.revoke()
        } else {
            ticket.outcome()
        };
        match outcome {
            Ok(PackageOutcome::Created(process)) => return Ok(process),
            Ok(PackageOutcome::NotCreated(evidence)) => {
                return Err(SpawnFailure::NotCreated {
                    evidence,
                    error: Error::Invalid("PACKAGE_APPLICATION_NOT_CREATED"),
                });
            }
            Ok(PackageOutcome::Pending) if !ending => {
                // Desktop-package activation can fail transiently, including
                // while the package is updating. Retry the SAME one-use
                // ticket, never a fresh launch; its gate prevents duplication.
                // Never retry Consuming/unknown or after cancellation/deadline.
                if activations < 3
                    && started.elapsed() < Duration::from_secs(10)
                    && last_activation.elapsed() >= Duration::from_secs(1)
                {
                    let activation = app_proxy_windows::package::activate_launch(
                        application.package().unwrap(),
                        &helper,
                        &ticket.request_path(),
                    );
                    app_proxy_windows::diagnostic_timing::mark("package.activation_retry", || {
                        format!(
                            "{id}:{}",
                            activation
                                .as_ref()
                                .map(|_| "accepted".to_owned())
                                .unwrap_or_else(|e| e.to_string())
                        )
                    });
                    activations += 1;
                    last_activation = std::time::Instant::now();
                }
            }
            Ok(PackageOutcome::Indeterminate)
            | Err(Error::Invalid(app_proxy_core::error_code::PACKAGE_REQUEST_BUSY))
                if !ending => {}
            Ok(_) | Err(Error::Invalid(app_proxy_core::error_code::PACKAGE_REQUEST_BUSY)) => {
                return Err(unknown(Error::Invalid(
                    app_proxy_core::error_code::PACKAGE_RESULT_UNKNOWN,
                )));
            }
            Err(error) => return Err(unknown(error)),
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

struct Job {
    engine: Arc<LaunchEngine>,
    id: Uuid,
    pinned: Option<event::ObservedTarget>,
}
impl Drop for Job {
    fn drop(&mut self) {
        if let Ok(mut active) = self.engine.active.lock() {
            active.remove(&self.id);
        }
        self.engine.completed.notify_one();
    }
}

fn entries(manifest: &Manifest, id: Uuid) -> Result<(&Application, &Instance)> {
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
    let app = manifest
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .ok_or(Error::Invalid("APPLICATION_NOT_FOUND"))?;
    Ok((app, instance))
}

fn dependency_digest(manifest: &Manifest, id: Uuid) -> Result<[u8; 32]> {
    // Preserve the previous manual tuple's serialization, including struct field
    // order. Converting to Value would change hashes of existing saved launches.
    #[derive(serde::Serialize)]
    #[serde(untagged)]
    enum Connection<'a> {
        AutoTest(
            (
                &'a app_proxy_core::model::Endpoint,
                Vec<&'a app_proxy_core::subscription::saved::SavedNode>,
            ),
        ),
        Manual(
            (
                &'a app_proxy_core::model::Endpoint,
                Uuid,
                &'a app_proxy_core::model::ManualProtocol,
                &'a str,
                u16,
                &'a Option<app_proxy_core::model::Credentials>,
            ),
        ),
        Subscription(
            (
                &'a app_proxy_core::model::Endpoint,
                Uuid,
                app_proxy_core::subscription::saved::ProtocolKind,
                &'a str,
                u16,
                Uuid,
            ),
        ),
    }
    let (app, instance) = entries(manifest, id)?;
    let profile = match instance.network {
        NetworkBinding::Direct {} => None,
        NetworkBinding::Profile { profile_id } => {
            let profile = manifest
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
            Some(match &profile.source {
                ProxySource::Manual { nodes } => {
                    let node = nodes
                        .iter()
                        .find(|n| n.id == profile.selected_node_id)
                        .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?;
                    Connection::Manual((
                        &profile.endpoint,
                        node.id,
                        &node.protocol,
                        &node.host,
                        node.port,
                        &node.credentials,
                    ))
                }
                ProxySource::Subscription {
                    nodes,
                    auto_test_node_ids,
                    ..
                } if !auto_test_node_ids.is_empty() => {
                    let selected = auto_test_node_ids
                        .iter()
                        .map(|id| {
                            nodes
                                .iter()
                                .find(|n| n.id == *id)
                                .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))
                        })
                        .collect::<Result<Vec<_>>>()?;
                    Connection::AutoTest((&profile.endpoint, selected))
                }
                ProxySource::Subscription { nodes, .. } => {
                    let node = nodes
                        .iter()
                        .find(|n| n.id == profile.selected_node_id)
                        .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?;
                    // Immutable secret identity covers every connection field. Source
                    // URL/revision and unselected nodes do not change this launch.
                    Connection::Subscription((
                        &profile.endpoint,
                        node.id,
                        node.protocol,
                        &node.server,
                        node.port,
                        node.secret_id,
                    ))
                }
            })
        }
    };
    Ok(Sha256::digest(serde_json::to_vec(&(
        &app.locator,
        app.template_ref,
        instance.application_id,
        &instance.data,
        &instance.args,
        &instance.env,
        &instance.cwd,
        &instance.network,
        profile,
        &manifest.settings.test_url,
        &manifest.settings.health_policy,
    ))?)
    .into())
}

async fn check_occupancy(
    application: &installation::ResolvedApplication,
    data: Option<&PreparedData>,
    template: Template,
) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(5),
        check_occupancy_inner(application, data, template),
    )
    .await
    .map_err(|_| Error::Invalid("INSTANCE_CHECK_TIMEOUT"))?
}

async fn query_when_ready<T, F, Fut>(query: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    loop {
        match query().await {
            Err(Error::Invalid(app_proxy_core::error_code::PROCESS_QUERY_BUSY)) => {
                tokio::time::sleep(Duration::from_millis(10)).await
            }
            result => return result,
        }
    }
}

async fn check_occupancy_inner(
    application: &installation::ResolvedApplication,
    data: Option<&PreparedData>,
    template: Template,
) -> Result<()> {
    let candidates =
        query_when_ready(|| process_query::application_candidates(application)).await?;
    let target = InstanceTarget::new(application, data, template)?;
    for observed in query_when_ready(|| target.inspect_candidates(&candidates, None)).await? {
        match (observed.role, observed.relation) {
            (_, InstanceRelation::Other) => {}
            (ProcessRole::Main, InstanceRelation::Target) => {
                return Err(Error::Invalid(
                    app_proxy_core::error_code::INSTANCE_EXTERNALLY_RUNNING,
                ));
            }
            (ProcessRole::Auxiliary, InstanceRelation::Target) => {
                return Err(Error::Invalid("INSTANCE_AUXILIARY_RUNNING"));
            }
            _ => return Err(Error::Invalid("INSTANCE_PROCESS_UNKNOWN")),
        }
    }
    Ok(())
}

fn error_code(error: &Error) -> &'static str {
    match error {
        Error::Invalid(code)
            if code.len() <= 128 && code.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') =>
        {
            code
        }
        _ => "LAUNCH_PREPARATION_FAILED",
    }
}

#[cfg(test)]
pub(crate) mod tests;
