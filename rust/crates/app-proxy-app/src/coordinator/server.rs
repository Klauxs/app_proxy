//! The single owner of a store: accepts authenticated connections, admits
//! durable work, and decides when the process may exit.
use super::protocol::*;
use super::*;

pub(super) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
pub(super) const MAX_CLIENTS: usize = 16;

pub(super) struct Shared {
    pub(super) root: PathBuf,
    pub(super) login_jobs: Arc<tokio::sync::Semaphore>,
    pub(super) login_queries: Arc<tokio::sync::Semaphore>,
    pub(super) shortcut_jobs: Arc<tokio::sync::Semaphore>,
    pub(super) identity: Status,
    pub(super) configuration: Arc<Configuration>,
    pub(super) core: CoreControl,
    pub(super) launch: Arc<crate::launch_engine::LaunchEngine>,
    pub(super) guard_queries: Arc<tokio::sync::Semaphore>,
    pub(super) guard_monitor: Arc<crate::guard_monitor::Monitor>,
    pub(super) subscription: Arc<crate::subscription_preview::PreviewService>,
    pub(super) jobs: AtomicUsize,
    pub(super) job_finished: tokio::sync::Notify,
}

impl Shared {
    pub(super) fn new(root: PathBuf, store: store::Store, identity: Status) -> Result<Self> {
        let resources = app_proxy_windows::instance_resource::ResourceRegistry::open()?;
        Self::with_resources(root, store, identity, resources)
    }

    /// The per-user resource registry is the one dependency that lives outside
    /// the store directory, so tests supply an isolated one.
    pub(super) fn with_resources(
        root: PathBuf,
        store: store::Store,
        identity: Status,
        resources: app_proxy_windows::instance_resource::ResourceRegistry,
    ) -> Result<Self> {
        let configuration = Arc::new(Configuration::new(store));
        let core = CoreControl::new(root.clone(), configuration.clone(), identity.epoch);
        let launch = crate::launch_engine::LaunchEngine::with_resources(
            configuration.clone(),
            core.manager(),
            identity.epoch,
            resources,
        )?;
        Ok(Self {
            root,
            login_jobs: Arc::new(tokio::sync::Semaphore::new(1)),
            login_queries: Arc::new(tokio::sync::Semaphore::new(1)),
            shortcut_jobs: Arc::new(tokio::sync::Semaphore::new(1)),
            subscription: crate::subscription_preview::PreviewService::new(
                configuration.clone(),
                core.manager(),
            ),
            identity,
            guard_monitor: crate::guard_monitor::Monitor::new(
                configuration.clone(),
                launch.clone(),
            ),
            configuration,
            core,
            launch,
            guard_queries: Arc::new(tokio::sync::Semaphore::new(1)),
            jobs: AtomicUsize::new(0),
            job_finished: tokio::sync::Notify::new(),
        })
    }
    pub(super) fn idle_allowed(&self) -> Result<bool> {
        // A temporarily unreadable completion record must not tear down the
        // coordinator or its owned proxy while application state is unresolved.
        if self.launch.refresh_sessions().is_err() {
            return Ok(false);
        }
        Ok(self.jobs.load(Ordering::SeqCst) == 0
            && !self.subscription.keeps_alive()?
            && self.core.idle_allowed()?
            && self
                .configuration
                .snapshot()?
                .instances
                .iter()
                .all(|i| i.guard.desired != app_proxy_core::model::Desired::Enabled))
    }
    pub(super) fn update(&self, manifest: &Manifest) -> Status {
        Status {
            revision: manifest.revision,
            applications: manifest.applications.len(),
            instances: manifest.instances.len(),
            profiles: manifest.profiles.len(),
            ..self.identity.clone()
        }
    }
    pub(super) fn execute(&self, request: Request) -> Result<Reply> {
        match request.operation {
            Operation::LoginApply { request: login } => {
                if request.request_id != login.id {
                    return Err(Error::Invalid("INVALID_LOGIN_REQUEST"));
                }
                Ok(Reply::LoginRequest {
                    status: Some(crate::login_tasks::apply(
                        &self.configuration,
                        &self.root,
                        &login,
                    )?),
                })
            }
            Operation::LoginResume { request_id } => Ok(Reply::LoginRequest {
                status: Some(crate::login_tasks::resume(&self.configuration, request_id)?),
            }),
            Operation::LoginRequest { request_id } => Ok(Reply::LoginRequest {
                status: self
                    .configuration
                    .lock()?
                    .login_request_status(request_id)?,
            }),
            Operation::LoginStatus {} => Ok(Reply::LoginStatus {
                status: crate::login_tasks::status(&self.configuration, &self.root)?,
            }),
            Operation::InstanceSettings { instance_id } => Ok(Reply::InstanceSettings {
                settings: crate::instance_settings::summary(
                    &self.configuration.snapshot()?,
                    instance_id,
                )?,
            }),
            Operation::ShortcutApply { request: shortcut } => {
                if request.request_id != shortcut.id {
                    return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
                }
                Ok(Reply::ShortcutRequest {
                    status: Some(crate::shortcuts::apply(
                        &self.configuration,
                        &self.root,
                        &shortcut,
                    )?),
                })
            }
            Operation::ShortcutResume { request_id } => Ok(Reply::ShortcutRequest {
                status: Some(self.configuration.lock()?.resume_shortcut(request_id)?),
            }),
            Operation::ShortcutRequest { request_id } => Ok(Reply::ShortcutRequest {
                status: self
                    .configuration
                    .lock()?
                    .shortcut_request_status(request_id)?,
            }),
            Operation::ShortcutStatus { instance_id } => Ok(Reply::ShortcutStatus {
                status: crate::shortcuts::status(&self.configuration, instance_id)?,
            }),
            Operation::ShortcutCheck { instance_id } => Ok(Reply::ShortcutCheck {
                check: self.configuration.lock()?.check_shortcut(instance_id)?,
            }),
            Operation::SubscriptionNodes {
                profile_id,
                offset,
                expected_revision,
            } => Ok(Reply::SubscriptionNodes {
                page: crate::subscription_preview::saved_page(
                    &self.configuration.snapshot()?,
                    profile_id,
                    offset,
                    expected_revision,
                )?,
            }),
            Operation::SubscriptionPreview { id, request } => {
                self.subscription.begin(id, request)?;
                Ok(Reply::SubscriptionPreview {
                    page: self.subscription.page(id, 0)?,
                })
            }
            Operation::SubscriptionPreviewPage { id, offset } => Ok(Reply::SubscriptionPreview {
                page: self.subscription.page(id, offset)?,
            }),
            Operation::SubscriptionPreviewClose { id } => {
                self.subscription.close(id)?;
                Ok(Reply::SubscriptionPreviewClosed {})
            }
            Operation::SubscriptionStage {
                preview_id,
                stage_id,
                request,
            } => Ok(Reply::SubscriptionStaged {
                staged: Box::new(self.subscription.stage(preview_id, stage_id, request)?),
            }),
            Operation::Catalog {
                offset,
                expected_revision,
            } => Ok(Reply::Catalog {
                page: catalog_page(self.configuration.snapshot()?, offset, expected_revision)?,
            }),
            Operation::Status {} => Ok(Reply::Status {
                status: self.update(&self.configuration.snapshot()?),
            }),
            Operation::Configure {
                expected_revision,
                action,
            } => {
                let outcome = self.configuration.apply(&ConfigRequest {
                    request_id: request.request_id,
                    expected_revision,
                    action,
                });
                // Even a failed receipt write may have committed the manifest.
                self.update(&self.configuration.snapshot()?);
                Ok(Reply::Configured { outcome: outcome? })
            }
            Operation::RequestStatus { request_id } => Ok(Reply::RequestStatus {
                status: self.configuration.request_status(request_id)?,
            }),
            Operation::CoreRequestStatus { request_id } => Ok(Reply::CoreRequestStatus {
                status: self.core.request_status(request_id)?,
            }),
            Operation::CoreStatus {} => Ok(Reply::CoreStatus {
                snapshot: self.core.snapshot()?,
            }),
            // `handle` routes these four before any blocking work is scheduled.
            Operation::ControlCore { .. }
            | Operation::Launch { .. }
            | Operation::GuardStatus { .. }
            | Operation::RuntimeStatus { .. } => Err(Error::Invalid("OPERATION_NOT_ROUTED")),
            Operation::LaunchStatus { request_id } => Ok(Reply::LaunchStatus {
                attempt: self.launch.status(request_id)?,
            }),
            Operation::CancelLaunch { request_id } => Ok(Reply::LaunchStatus {
                attempt: Some(self.launch.cancel(request_id)?),
            }),
        }
    }
}

pub async fn serve(root: PathBuf) -> Result<()> {
    serve_expected(root, None).await
}

pub async fn serve_expected(root: PathBuf, expected_store: Option<Uuid>) -> Result<()> {
    identity::assert_ordinary_user()?;
    app_proxy_windows::setup::ensure_available()?;
    let owned = store::Store::open_expected(&root, expected_store)?;
    let manifest = owned.load()?;
    let current = identity::current()?;
    let (cli, host) = binaries()?;
    let policy = ipc::PeerPolicy::current(vec![
        identity::file_identity(&cli)?,
        identity::file_identity(&host)?,
    ])?;
    let listener = ipc::Listener::bind(manifest.store_id, policy)?;
    let snapshot = Status {
        store_id: manifest.store_id,
        revision: manifest.revision,
        epoch: Uuid::new_v4(),
        coordinator_pid: current.pid,
        session_id: current.session_id,
        applications: manifest.applications.len(),
        instances: manifest.instances.len(),
        profiles: manifest.profiles.len(),
        phase: "bootstrap".into(),
    };
    let shared = Arc::new(Shared::new(root, owned, snapshot)?);
    serve_connections(listener, shared, IDLE_TIMEOUT).await
}

pub(super) async fn serve_connections(
    mut listener: ipc::Listener,
    shared: Arc<Shared>,
    idle_timeout: Duration,
) -> Result<()> {
    let mut guard_listener = Some(shared.guard_monitor.start()?);
    let program_directory = app_proxy_windows::setup::current_directory()?;
    let mut update_check = tokio::time::interval(Duration::from_millis(250));
    update_check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut draining = false;
    let mut idle_allowed = shared.idle_allowed()?;
    let mut clients = tokio::task::JoinSet::new();
    let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = update_check.tick() => {
                if app_proxy_windows::setup::requested_at(&program_directory)? {
                    draining = true;
                    // Revoke guard dispatch before waiting for already accepted work.
                    drop(guard_listener.take());
                }
                if draining && clients.is_empty() && shared.jobs.load(Ordering::SeqCst) == 0
                    && shared.core.drained()? && shared.launch.drained()? {
                    break;
                }
            }
            accepted = listener.accept(), if !draining && clients.len() < MAX_CLIENTS => {
                match accepted {
                    Ok(connection) => {
                        idle_deadline = tokio::time::Instant::now() + idle_timeout;
                        let shared = shared.clone();
                        clients.spawn(async move { handle(connection, shared).await });
                    }
                    Err(ipc::AcceptError::Peer(_)) => {},
                    Err(ipc::AcceptError::Listener(error)) => return Err(error),
                }
            }
            _ = clients.join_next(), if !clients.is_empty() => {
                if clients.is_empty() {
                    let shared = shared.clone();
                    // All short connections are done. Read once so a delayed
                    // status response cannot overwrite a newer guard decision.
                    idle_allowed = tokio::task::spawn_blocking(move || shared.idle_allowed()).await
                        .map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))??;
                    idle_deadline = tokio::time::Instant::now() + idle_timeout;
                }
            },
            _ = shared.job_finished.notified() => {
                let owner = shared.clone();
                idle_allowed = tokio::task::spawn_blocking(move || owner.idle_allowed()).await
                    .map_err(|_| Error::Invalid("CORE_CONTROL_WORKER_FAILED"))??;
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
            },
            _ = shared.launch.completed() => {
                let owner = shared.clone();
                idle_allowed = tokio::task::spawn_blocking(move || owner.idle_allowed()).await
                    .map_err(|_| Error::Invalid("LAUNCH_WORKER_FAILED"))??;
                idle_deadline = tokio::time::Instant::now() + idle_timeout;
            },
            _ = maintenance.tick(), if !idle_allowed && clients.is_empty() => {
                let owner = shared.clone();
                idle_allowed = tokio::task::spawn_blocking(move || owner.idle_allowed()).await
                    .map_err(|_| Error::Invalid("LAUNCH_WORKER_FAILED"))??;
                if idle_allowed { idle_deadline = tokio::time::Instant::now() + idle_timeout; }
            },
            _ = tokio::time::sleep_until(idle_deadline), if idle_allowed && clients.is_empty() => break,
        }
    }
    drop(listener);
    drop(shared);
    Ok(())
}

/// One request per connection. Every operation belongs to one class:
/// a bounded observation, durable work admitted before the reply, or a blocking
/// store operation. Work that outlives the reply starts only after it is sent.
pub(super) async fn handle(
    mut connection: ipc::Connection<NamedPipeServer>,
    shared: Arc<Shared>,
) -> Result<()> {
    let Some(request) = handshake(&mut connection, &shared).await? else {
        return Ok(());
    };
    let request_id = request.request_id;
    let epoch = shared.identity.epoch;
    let (result, admitted) = match request.operation {
        Operation::RuntimeStatus { instance_id } => {
            (observe_runtime(&shared, instance_id).await?, None)
        }
        Operation::GuardStatus { instance_id } => {
            (observe_guard(&shared, instance_id).await?, None)
        }
        Operation::Launch {
            instance_id,
            origin,
            expected_revision,
        } => {
            let launch = LaunchRequest {
                request_id,
                instance_id,
                origin,
            };
            (
                admit_launch(&shared, launch, expected_revision).await?,
                None,
            )
        }
        Operation::ControlCore { action } => admit_core(&shared, request_id, action).await?,
        operation => {
            let request = Request {
                operation,
                ..request
            };
            (run_blocking(&shared, request).await?, None)
        }
    };
    let sent = connection
        .send(&Response {
            request_id,
            epoch,
            result,
        })
        .await;
    // Long work has its own lifetime, not an IPC slot: queries and cancellation
    // must remain available while all admitted installers are waiting.
    if let Some(job) = admitted {
        shared.jobs.fetch_add(1, Ordering::SeqCst);
        let completed = JobCompletion(shared.clone());
        let runtime = tokio::runtime::Handle::current();
        tokio::task::spawn_blocking(move || {
            let _completed = completed;
            let _ = runtime.block_on(shared.core.execute(job));
        });
    }
    sent
}

/// `None` means the peer was told why it was rejected and nothing else follows.
async fn handshake(
    connection: &mut ipc::Connection<NamedPipeServer>,
    shared: &Shared,
) -> Result<Option<Request>> {
    let status = &shared.identity;
    let request: Hello = connection.receive().await?;
    let rejection = if request.protocol_major != PROTOCOL_MAJOR {
        Some("PROTOCOL_VERSION_MISMATCH")
    } else if request.store_id != status.store_id {
        Some("STORE_ID_MISMATCH")
    } else if request.session_id != status.session_id {
        Some("STORE_SESSION_CONFLICT")
    } else if request.epoch.is_some() {
        Some("INVALID_CLIENT_HELLO")
    } else {
        None
    };
    if let Some(code) = rejection {
        connection
            .send(&Welcome::Rejected { code: code.into() })
            .await?;
        return Ok(None);
    }
    connection
        .send(&Welcome::Ready {
            hello: hello(status.store_id, status.session_id, Some(status.epoch)),
        })
        .await?;
    let request: Request = connection.receive().await?;
    app_proxy_windows::setup::ensure_available()?;
    if request.protocol_major != PROTOCOL_MAJOR || request.request_id.is_nil() {
        return Err(Error::Invalid("INVALID_RPC_REQUEST"));
    }
    Ok(Some(request))
}

fn reply(result: Result<Reply>) -> Reply {
    result.unwrap_or_else(|error| Reply::Error {
        code: safe_error(error),
    })
}

fn busy(code: &str) -> Reply {
    Reply::Error { code: code.into() }
}

/// Bounded observation. Without the single query permit the engine answers
/// from what it already knows instead of queueing another native scan.
async fn observe_runtime(shared: &Arc<Shared>, instance_id: Uuid) -> Result<Reply> {
    let permit = shared.guard_queries.clone().try_acquire_owned().ok();
    let shared = shared.clone();
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let allowed = permit.is_some();
        let _permit = permit;
        runtime.block_on(shared.launch.observe_instance(instance_id, allowed))
    })
    .await
    .map_err(|_| Error::Invalid("RUNTIME_STATUS_WORKER_FAILED"))?;
    Ok(reply(result.map(|status| Reply::RuntimeStatus {
        status: Box::new(status),
    })))
}

async fn observe_guard(shared: &Arc<Shared>, instance_id: Uuid) -> Result<Reply> {
    let permit = shared.guard_queries.clone().try_acquire_owned().ok();
    let shared = shared.clone();
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let scan_allowed = permit.is_some();
        let _permit = permit;
        let mut status = runtime.block_on(crate::guard_control::status(
            &shared.configuration,
            &shared.launch,
            instance_id,
            scan_allowed,
        ))?;
        shared.guard_monitor.update_status(&mut status);
        Ok::<_, Error>(status)
    })
    .await
    .map_err(|_| Error::Invalid("GUARD_STATUS_WORKER_FAILED"))?;
    Ok(reply(result.map(|status| Reply::GuardStatus {
        status: Box::new(status),
    })))
}

/// The engine owns accepted work before the reply is sent; a failed send cannot
/// discard it or consume a connection slot during proxy preparation.
async fn admit_launch(
    shared: &Arc<Shared>,
    request: LaunchRequest,
    expected_revision: Option<u64>,
) -> Result<Reply> {
    let engine = shared.launch.clone();
    let runtime = tokio::runtime::Handle::current();
    let admitted = tokio::task::spawn_blocking(move || {
        runtime.block_on(engine.submit_at_revision(request, expected_revision))
    })
    .await
    .map_err(|_| Error::Invalid("LAUNCH_WORKER_FAILED"))?;
    Ok(reply(admitted.map(|attempt| Reply::LaunchStatus {
        attempt: Some(attempt),
    })))
}

async fn admit_core(
    shared: &Arc<Shared>,
    request_id: Uuid,
    action: CoreAction,
) -> Result<(Reply, Option<crate::core_control::CoreJob>)> {
    let worker = shared.clone();
    let admitted = tokio::task::spawn_blocking(move || worker.core.accept(request_id, &action))
        .await
        .map_err(|_| Error::Invalid("CORE_CONTROL_WORKER_FAILED"))?;
    Ok(match admitted {
        Ok((status, job)) => (
            Reply::CoreRequestStatus {
                status: Some(status),
            },
            job,
        ),
        Err(error) => (reply(Err(error)), None),
    })
}

/// Store operations run on a blocking thread and are never cancelled when the
/// client disconnects: the job stays registered until it completes, which also
/// prevents an idle exit.
async fn run_blocking(shared: &Arc<Shared>, request: Request) -> Result<Reply> {
    let request_id = request.request_id;
    let login_mutation = matches!(
        &request.operation,
        Operation::LoginApply { .. } | Operation::LoginResume { .. }
    );
    if login_mutation {
        let shared = shared.clone();
        let replay_request = match &request.operation {
            Operation::LoginApply { request } => Some(request.clone()),
            _ => None,
        };
        let id = match &request.operation {
            Operation::LoginApply { request } => request.id,
            Operation::LoginResume { request_id } => *request_id,
            _ => unreachable!(),
        };
        let replay = tokio::task::spawn_blocking(move || {
            if replay_request.is_some() && request_id != id {
                return Err(Error::Invalid("INVALID_LOGIN_REQUEST"));
            }
            crate::login_tasks::replay(&shared.configuration, id, replay_request.as_ref())
        })
        .await
        .map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))?;
        match replay {
            Ok(None) => {}
            other => return Ok(reply(other.map(|status| Reply::LoginRequest { status }))),
        }
    }
    let login_status = matches!(&request.operation, Operation::LoginStatus {});
    let login_slot = if login_status {
        Some(shared.login_queries.clone())
    } else if login_mutation {
        Some(shared.login_jobs.clone())
    } else {
        None
    };
    let login_permit = match login_slot {
        Some(slot) => match slot.try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => return Ok(busy("GUARD_LOGIN_BUSY")),
        },
        None => None,
    };
    let shortcut_permit = if matches!(
        &request.operation,
        Operation::ShortcutApply { .. } | Operation::ShortcutResume { .. }
    ) {
        match shared.shortcut_jobs.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => return Ok(busy("SHORTCUT_OPERATION_BUSY")),
        }
    } else {
        None
    };
    shared.jobs.fetch_add(1, Ordering::SeqCst);
    let completed = JobCompletion(shared.clone());
    let owner = shared.clone();
    let worker = tokio::task::spawn_blocking(move || {
        let _completed = completed;
        let _login_permit = login_permit;
        let _permit = shortcut_permit;
        owner.execute(request)
    });
    let result = if login_status {
        match tokio::time::timeout(Duration::from_secs(3), worker).await {
            Ok(result) => result.map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))?,
            Err(_) => Err(Error::Invalid("GUARD_LOGIN_CHECK_TIMEOUT")),
        }
    } else {
        worker
            .await
            .map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))?
    };
    Ok(reply(result))
}

pub(super) struct JobCompletion(pub(super) Arc<Shared>);
impl Drop for JobCompletion {
    fn drop(&mut self) {
        self.0.jobs.fetch_sub(1, Ordering::SeqCst);
        self.0.job_finished.notify_one();
    }
}
