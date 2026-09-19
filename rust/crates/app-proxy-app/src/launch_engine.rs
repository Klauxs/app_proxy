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
    process::{self, CreationMode, SpawnFailure, SpawnSpec},
    process_query,
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;

mod guard;
pub use guard::{GuardObservation, GuardScan};

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
        self.submit_checked(request, expected_revision, None)
    }

    /// Internal Guard adapter. A caller-supplied origin label on ordinary launch
    /// never grants stop permission; this path binds an exact observed target.
    pub fn submit_guard(
        self: &Arc<Self>,
        request: LaunchRequest,
        revision: u64,
        target: GuardTarget,
    ) -> Result<LaunchAttempt> {
        self.submit_checked(request, Some(revision), Some(target))
    }

    fn submit_checked(
        self: &Arc<Self>,
        request: LaunchRequest,
        expected_revision: Option<u64>,
        guard_target: Option<GuardTarget>,
    ) -> Result<LaunchAttempt> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| Error::Invalid("LAUNCH_OWNER_FAILED"))?;
        let mut store = self.configuration.lock()?;
        let admit = |store: &mut app_proxy_windows::store::Store| match &guard_target {
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
        store.recover_config_requests()?;
        for attempt in store.launch_attempts()? {
            if attempt.instance_id == request.instance_id && !active.contains(&attempt.id) {
                self.reconcile(&mut store, &attempt)?;
            }
        }
        if active.len() >= 32 && store.launch_request(request.request_id)?.is_none() {
            return Err(Error::Invalid("LAUNCH_OPERATION_LIMIT"));
        }
        if store.launch_request(request.request_id)?.is_none() {
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
        }
        let admission = admit(&mut store)?;
        if admission.is_new {
            active.insert(admission.attempt.id);
            let job = Job {
                engine: self.clone(),
                id: admission.attempt.id,
            };
            tokio::spawn(async move {
                // A panic/drop leaves a durable stage. Status/recovery never
                // interprets it as proof that creation did not happen.
                job.engine.execute(job.id).await;
            });
        }
        Ok(admission.attempt)
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
                Err(Error::Invalid("INSTANCE_RESOURCE_BUSY" | "PACKAGE_REQUEST_BUSY")) => Ok(()),
                other => other,
            }
        } else {
            Ok(())
        }
    }

    async fn execute(&self, id: Uuid) {
        let mut reservation = None;
        let result = tokio::time::timeout(
            Duration::from_secs(90),
            self.prepare_and_spawn(id, &mut reservation),
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
    ) -> Result<()> {
        self.advance(id, LaunchPhase::Accepted {}, LaunchPhase::Resolving {})?;
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
        let locator = app.locator.clone();
        let application = tokio::task::spawn_blocking(move || installation::resolve(&locator))
            .await
            .map_err(|_| Error::Invalid("LAUNCH_RESOLUTION_INTERRUPTED"))??;
        app_proxy_windows::ifeo::ensure_plain_creation(application.executable())?;
        let data = self
            .configuration
            .lock()?
            .prepare_instance_data(instance_id, application.package())?;
        self.advance(
            id,
            LaunchPhase::Resolving {},
            LaunchPhase::CheckingInstance {},
        )?;
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
        let correction = self
            .configuration
            .lock()?
            .launch_request(id)?
            .unwrap()
            .guard_correction;
        let (application, data) = if let Some(correction) = correction {
            self.correct_guard(
                id,
                application,
                data,
                app.template_ref,
                &correction.target,
                reservation,
            )
            .await?
        } else {
            (application, data)
        };
        check_occupancy(&application, data.as_ref(), app.template_ref).await?;
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
        check_occupancy(&application, data.as_ref(), app.template_ref).await?;
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
            mode: CreationMode::Normal,
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
        target: &GuardTarget,
        reservation: &mut Option<ResourceReservation>,
    ) -> Result<(installation::ResolvedApplication, Option<PreparedData>)> {
        let instance = InstanceTarget::new(&application, data.as_ref(), template)?;
        let endpoint = std::net::SocketAddr::new(target.endpoint.host, target.endpoint.port);
        let observed =
            query_when_ready(|| instance.inspect_proxy(&target.process, endpoint)).await?;
        if observed.role != ProcessRole::Main
            || observed.relation != InstanceRelation::Target
            || observed.proxy != ProxyArguments::Mismatched
        {
            return Err(Error::Invalid("GUARD_TARGET_NOT_UNPROXIED"));
        }
        let candidates =
            query_when_ready(|| process_query::application_candidates(&application)).await?;
        let mut auxiliaries = Vec::new();
        for candidate in candidates {
            if candidate == target.process {
                continue;
            }
            let observed = query_when_ready(|| instance.inspect(&candidate)).await?;
            match (observed.role, observed.relation) {
                (_, InstanceRelation::Other) => {}
                (ProcessRole::Auxiliary, InstanceRelation::Target) => auxiliaries.push(candidate),
                _ => return Err(Error::Invalid("GUARD_INSTANCE_NOT_EXCLUSIVE")),
            }
        }
        // Refresh the exact main after capturing auxiliaries; no inference from
        // orphaned parent PIDs is needed after the main exits.
        let observed =
            query_when_ready(|| instance.inspect_proxy(&target.process, endpoint)).await?;
        if observed.role != ProcessRole::Main
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
        let (application, data, held, result) = tokio::task::spawn_blocking(move || {
            let result = (|| {
                let dispatch = configuration.lock()?.dispatch_guard_stop(id, epoch)?;
                let permit = held.authorize_guard_stop(dispatch)?;
                let receipt = app_proxy_windows::process_stop::stop_guarded(permit)?;
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
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            let mut running = false;
            for auxiliary in &auxiliaries {
                running |= process::is_running_exact(auxiliary)?;
            }
            if !running {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Invalid("GUARD_AUXILIARY_STILL_RUNNING"));
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
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
        let _ = app_proxy_windows::package::activate_launch(
            application.package().unwrap(),
            &helper,
            &ticket.request_path(),
        );
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
            Ok(PackageOutcome::Pending | PackageOutcome::Indeterminate)
            | Err(Error::Invalid("PACKAGE_REQUEST_BUSY"))
                if !ending => {}
            Ok(_) | Err(Error::Invalid("PACKAGE_REQUEST_BUSY")) => {
                return Err(unknown(Error::Invalid("PACKAGE_RESULT_UNKNOWN")));
            }
            Err(error) => return Err(unknown(error)),
        }
        std::thread::sleep(Duration::from_millis(30));
    }
}

struct Job {
    engine: Arc<LaunchEngine>,
    id: Uuid,
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
    let (app, instance) = entries(manifest, id)?;
    let profile = match instance.network {
        NetworkBinding::Direct {} => None,
        NetworkBinding::Profile { profile_id } => {
            let profile = manifest
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
            let ProxySource::Manual { nodes } = &profile.source;
            let node = nodes
                .iter()
                .find(|n| n.id == profile.selected_node_id)
                .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?;
            Some((
                &profile.endpoint,
                node.id,
                &node.protocol,
                &node.host,
                node.port,
                &node.credentials,
            ))
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
            Err(Error::Invalid("PROCESS_QUERY_BUSY")) => {
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
    for process in candidates {
        let observed = query_when_ready(|| target.inspect(&process)).await?;
        match (observed.role, observed.relation) {
            (_, InstanceRelation::Other) => {}
            (ProcessRole::Main, InstanceRelation::Target) => {
                return Err(Error::Invalid("INSTANCE_EXTERNALLY_RUNNING"));
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
