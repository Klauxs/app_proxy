//! Ordinary coordinator's listener lifecycle. Events only request a rescan;
//! neither a PID hint nor a scheduled-task receipt grants process authority.
use crate::{configuration::Configuration, launch_engine::LaunchEngine};
use app_proxy_core::model::Desired;
use app_proxy_windows::{
    Error, Result,
    etw::EventBatch,
    event_pipe::EventReceiver,
    guard_deployment::{CoordinatorImage, Deployment},
    guard_task, identity,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Notify, Semaphore, watch},
    task::JoinHandle,
    time::Instant,
};
use uuid::Uuid;

mod scan;

const RETRY: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(2);
const FULL_SCAN: Duration = Duration::from_secs(30);
const PREPARE_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    Disabled,
    NeedsAuthorization,
    Starting,
    Etw,
    Polling,
    Blocked,
}
#[derive(Clone)]
pub(crate) struct Snapshot {
    pub phase: Phase,
    pub generation: Option<Uuid>,
    pub epoch: Option<Uuid>,
    pub sequence: u64,
    pub diagnostic: Option<String>,
    received: Option<Instant>,
}
impl Snapshot {
    fn fresh(&self) -> Self {
        let mut snapshot = self.clone();
        if snapshot.phase == Phase::Etw
            && snapshot
                .received
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(5))
        {
            snapshot.phase = Phase::Polling;
            snapshot.diagnostic = Some("GUARD_LISTENER_HEARTBEAT_STALE".into());
        }
        snapshot
    }
    fn new(phase: Phase, diagnostic: Option<String>) -> Self {
        Self {
            phase,
            generation: None,
            epoch: None,
            sequence: 0,
            diagnostic,
            received: None,
        }
    }
    fn observe(&mut self, batch: &EventBatch) -> bool {
        self.epoch = Some(batch.epoch);
        self.sequence = batch.sequence;
        self.received = Some(Instant::now());
        if let Some(code) = batch.ended {
            self.phase = Phase::Polling;
            self.diagnostic = Some(format!("GUARD_EVENT_STREAM_ENDED: {code}"));
        } else {
            self.phase = Phase::Etw;
            self.diagnostic = None;
        }
        batch.full_scan_required || !batch.hints.is_empty() || batch.ended.is_some()
    }
}

struct Authorization {
    deployment: Deployment,
    _coordinator: CoordinatorImage,
}
struct State {
    owner: Option<Uuid>,
    snapshot: Snapshot,
    authorization: Option<Arc<Authorization>>,
    scans: HashMap<Uuid, scan::Record>,
}

struct Preparation {
    pending: Arc<Mutex<bool>>,
    deadline: Instant,
}
impl Preparation {
    fn new(timeout: Duration) -> Self {
        Self {
            pending: Arc::new(Mutex::new(true)),
            deadline: Instant::now() + timeout,
        }
    }
}
impl Drop for Preparation {
    fn drop(&mut self) {
        *self.pending.lock().unwrap_or_else(|p| p.into_inner()) = false;
    }
}

pub(crate) struct Monitor {
    configuration: Arc<Configuration>,
    launch: Arc<LaunchEngine>,
    state: Mutex<State>,
    native: Arc<Semaphore>,
    scan_requested: Notify,
}
impl Monitor {
    pub fn new(configuration: Arc<Configuration>, launch: Arc<LaunchEngine>) -> Arc<Self> {
        Arc::new(Self {
            configuration,
            launch,
            state: Mutex::new(State {
                owner: None,
                snapshot: Snapshot::new(Phase::Disabled, None),
                authorization: None,
                scans: HashMap::new(),
            }),
            native: Arc::new(Semaphore::new(1)),
            scan_requested: Notify::new(),
        })
    }
    pub fn start(self: &Arc<Self>) -> Result<Service> {
        let epoch = Uuid::new_v4();
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.owner.is_some() {
                return Err(Error::Invalid("GUARD_MONITOR_ALREADY_RUNNING"));
            }
            state.owner = Some(epoch);
        }
        let (stop, stopped) = watch::channel(false);
        let owner = self.clone();
        Ok(Service {
            stop,
            owner: self.clone(),
            epoch,
            _task: Task(tokio::spawn(async move { owner.run(epoch, stopped).await })),
        })
    }
    pub fn snapshot(&self) -> Snapshot {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .snapshot
            .fresh()
    }
    fn publish(
        &self,
        owner: Uuid,
        phase: Phase,
        authorization: Option<Arc<Authorization>>,
        diagnostic: Option<String>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.owner != Some(owner) {
            return;
        }
        let mut snapshot = Snapshot::new(phase, diagnostic);
        snapshot.generation = authorization.as_ref().map(|a| a.deployment.generation());
        if !matches!((&state.authorization, &authorization), (Some(old), Some(new)) if Arc::ptr_eq(old, new))
        {
            state.scans.clear();
        }
        state.snapshot = snapshot;
        state.authorization = authorization;
    }
    fn wanted(&self) -> Result<Option<Uuid>> {
        let manifest = self.configuration.snapshot()?;
        Ok(manifest
            .instances
            .iter()
            .any(|i| i.guard.desired == Desired::Enabled)
            .then_some(manifest.store_id))
    }
    fn batch(&self, authorization: &Arc<Authorization>, batch: &EventBatch) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if !state
            .authorization
            .as_ref()
            .is_some_and(|active| Arc::ptr_eq(active, authorization))
        {
            return;
        }
        if state.snapshot.observe(batch) {
            // Coalesce bursts into one pending full scan, never an unbounded PID
            // work queue. The consumer re-reads registered instances/identities.
            self.scan_requested.notify_one();
        }
    }

    async fn native_work<T: Send + 'static>(
        &self,
        timeout: Duration,
        work: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let permit = self
            .native
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Invalid("GUARD_LISTENER_WORKER_BUSY"))?;
        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work()
        });
        tokio::time::timeout(timeout, worker)
            .await
            .map_err(|_| Error::Invalid("GUARD_LISTENER_PREPARE_TIMEOUT"))?
            .map_err(|_| Error::Invalid("GUARD_LISTENER_WORKER_FAILED"))?
    }

    fn claim_dispatch(&self, owner: Uuid, pending: &Mutex<bool>, deadline: Instant) -> Result<()> {
        // Service shutdown and dispatch serialize here. Once claimed, an OS
        // call may already have side effects and cannot be treated as cancelled.
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let mut pending = pending.lock().unwrap_or_else(|p| p.into_inner());
        if state.owner != Some(owner) || !*pending || Instant::now() >= deadline {
            return Err(Error::Invalid("GUARD_LISTENER_PREPARATION_REVOKED"));
        }
        *pending = false;
        Ok(())
    }

    async fn connect(self: Arc<Self>, owner_epoch: Uuid, store: Uuid) -> Result<Connection> {
        let configuration = self.configuration.clone();
        // The async attempt owns revocation; the blocking worker owns only the
        // gate. Timeout or cancellation must not leave a late launch queued.
        let preparation = Preparation::new(PREPARE_TIMEOUT);
        let pending = preparation.pending.clone();
        let deadline = preparation.deadline;
        let owner = self.clone();
        let (authorization, run_error) = self
            .native_work(PREPARE_TIMEOUT, move || {
                let deployment = Deployment::listener(store)?;
                let coordinator = deployment.coordinator()?;
                if identity::current()?.image_file != *coordinator.image() {
                    return Err(Error::Invalid("GUARD_COORDINATOR_CHANGED"));
                }
                guard_task::verify_registered(&deployment)?;
                let manifest = configuration.snapshot()?;
                if manifest.store_id != store
                    || !manifest
                        .instances
                        .iter()
                        .any(|i| i.guard.desired == Desired::Enabled)
                {
                    return Err(Error::Invalid("GUARD_LISTENER_CONFIG_CHANGED"));
                }
                let authorization = Arc::new(Authorization {
                    deployment,
                    _coordinator: coordinator,
                });
                // Even a RunEx error can follow a successful system side effect.
                // Keep verified deployment evidence and try authentication once;
                // never issue a second RunEx in the same attempt.
                owner.claim_dispatch(owner_epoch, &pending, deadline)?;
                let run = guard_task::run(&authorization.deployment);
                Ok((authorization, run.err().map(|e| e.to_string())))
            })
            .await?;
        let stream = EventReceiver::connect(
            store,
            vec![authorization.deployment.host_image().clone()],
            CONNECT_TIMEOUT,
        )
        .await;
        match stream {
            Ok(stream) => Ok(Connection {
                authorization,
                stream: Some(stream),
                diagnostic: None,
            }),
            Err(error) => Ok(Connection {
                authorization,
                stream: None,
                diagnostic: Some(run_error.unwrap_or_else(|| error.to_string())),
            }),
        }
    }

    async fn read(
        self: Arc<Self>,
        mut stream: EventReceiver,
        authorization: Arc<Authorization>,
    ) -> Result<()> {
        loop {
            // This receive future is never cancelled for a timer tick. Pipe
            // cancellation poisons a connection, so only shutdown drops it.
            let batch = stream.receive().await?;
            self.batch(&authorization, &batch);
            if let Some(code) = batch.ended {
                return Err(Error::Windows {
                    operation: "GuardEventStreamEnded",
                    code,
                });
            }
        }
    }

    async fn run(self: Arc<Self>, owner_epoch: Uuid, mut stopped: watch::Receiver<bool>) {
        let scanner = self.clone();
        let _scanner = Task(tokio::spawn(async move {
            scanner.scan_instances(owner_epoch).await
        }));
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut preparing: Option<Task<Result<Connection>>> = None;
        let mut reading: Option<Task<Result<()>>> = None;
        let mut scope = None;
        let mut retry_at = Instant::now();
        let mut scan_at = Instant::now();
        loop {
            tokio::select! {
                biased;
                _ = stopped.changed() => break,
                result = joined(&mut preparing), if preparing.is_some() => {
                    preparing = None;
                    if self.wanted().ok().flatten() != scope { continue; }
                    match result {
                        Ok(Ok(connection)) => {
                            let authorization = connection.authorization;
                            self.publish(owner_epoch, if connection.stream.is_some() { Phase::Starting } else { Phase::Polling }, Some(authorization.clone()), connection.diagnostic);
                            self.scan_requested.notify_one();
                            scan_at = Instant::now() + POLL;
                            if let Some(stream) = connection.stream {
                                let owner = self.clone();
                                reading = Some(Task(tokio::spawn(async move { owner.read(stream, authorization).await })));
                            }
                        }
                        other => {
                            let error = match other {
                                Ok(Err(error)) => error,
                                _ => Error::Invalid("GUARD_LISTENER_WORKER_FAILED"),
                            };
                            let phase = if matches!(error, Error::Invalid("GUARD_LISTENER_MISSING" | "GUARD_TASK_MISSING")) { Phase::NeedsAuthorization } else { Phase::Blocked };
                            self.publish(owner_epoch, phase, None, Some(error.to_string()));
                        }
                    }
                    retry_at = Instant::now() + RETRY;
                }
                result = joined(&mut reading), if reading.is_some() => {
                    reading = None;
                    let error = match result { Ok(Err(error)) => error.to_string(), _ => "GUARD_EVENT_STREAM_CLOSED".into() };
                    let authorization = self.state.lock().unwrap_or_else(|p| p.into_inner()).authorization.clone();
                    self.publish(owner_epoch, Phase::Polling, authorization, Some(error));
                    self.scan_requested.notify_one();
                    scan_at = Instant::now() + POLL;
                    retry_at = Instant::now() + RETRY;
                }
                _ = tick.tick() => {
                    let wanted = match self.wanted() {
                        Ok(wanted) => wanted,
                        Err(error) => {
                            preparing = None; reading = None; scope = None;
                            self.publish(owner_epoch, Phase::Blocked, None, Some(error.to_string()));
                            continue;
                        }
                    };
                    if wanted != scope {
                        preparing = None; reading = None; scope = wanted;
                        self.publish(owner_epoch, if wanted.is_some() { Phase::Starting } else { Phase::Disabled }, None, None);
                        retry_at = Instant::now();
                        scan_at = Instant::now();
                    }
                    let Some(store) = scope else { continue; };
                    if preparing.is_none() && reading.is_none() && Instant::now() >= retry_at {
                        let owner = self.clone();
                        preparing = Some(Task(tokio::spawn(async move { owner.connect(owner_epoch, store).await })));
                    }
                    let state = self.snapshot();
                    if state.generation.is_some() && Instant::now() >= scan_at {
                        self.scan_requested.notify_one();
                        scan_at = Instant::now() + if state.phase == Phase::Etw { FULL_SCAN } else { POLL };
                    }
                }
            }
        }
        self.publish(owner_epoch, Phase::Disabled, None, None);
    }
}

struct Connection {
    authorization: Arc<Authorization>,
    stream: Option<EventReceiver>,
    diagnostic: Option<String>,
}
struct Task<T>(JoinHandle<T>);
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn joined<T>(task: &mut Option<Task<T>>) -> std::result::Result<T, tokio::task::JoinError> {
    match task {
        Some(task) => (&mut task.0).await,
        None => std::future::pending().await,
    }
}
pub(crate) struct Service {
    stop: watch::Sender<bool>,
    owner: Arc<Monitor>,
    epoch: Uuid,
    _task: Task<()>,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        let mut state = self.owner.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.owner == Some(self.epoch) {
            state.owner = None;
            state.authorization = None;
            state.scans.clear();
            state.snapshot = Snapshot::new(Phase::Disabled, None);
        }
    }
}

#[cfg(test)]
mod tests;
