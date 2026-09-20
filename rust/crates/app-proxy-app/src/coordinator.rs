//! Authenticated coordinator RPC; configuration writes use durable request records.
use crate::configuration::{CatalogPage, Configuration, catalog_page};
use crate::core_control::CoreControl;
use app_proxy_core::core_control::{CoreAction, CoreRequestStatus};
use app_proxy_core::launch::{LaunchAttempt, LaunchOrigin, LaunchRequest};
use app_proxy_core::{
    model::Manifest,
    registry::{ConfigAction, ConfigRequest},
};
use app_proxy_windows::config_transaction::{ConfigOutcome, ConfigRequestStatus};
use app_proxy_windows::{Error, Result, identity, ipc, process, store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::net::windows::named_pipe::NamedPipeServer;
use uuid::Uuid;

const PROTOCOL_MAJOR: u32 = 2;
const PROTOCOL_MINOR: u32 = 17;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CLIENTS: usize = 16;

#[cfg(test)]
mod launch_tests;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protocol_major: u32,
    protocol_minor: u32,
    version: String,
    store_id: Uuid,
    session_id: u32,
    epoch: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum Welcome {
    Ready { hello: Hello },
    Rejected { code: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    protocol_major: u32,
    request_id: Uuid,
    operation: Operation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    InstanceSettings {
        instance_id: Uuid,
    },
    ShortcutApply {
        request: app_proxy_windows::shortcuts::journal::Request,
    },
    ShortcutResume {
        request_id: Uuid,
    },
    ShortcutRequest {
        request_id: Uuid,
    },
    ShortcutStatus {
        instance_id: Uuid,
    },
    RuntimeStatus {
        instance_id: Uuid,
    },
    SubscriptionNodes {
        profile_id: Uuid,
        offset: usize,
        expected_revision: Option<u64>,
    },
    SubscriptionPreview {
        id: Uuid,
        request: crate::subscription_preview::PreviewRequest,
    },
    SubscriptionPreviewPage {
        id: Uuid,
        offset: usize,
    },
    SubscriptionPreviewClose {
        id: Uuid,
    },
    SubscriptionStage {
        preview_id: Uuid,
        stage_id: Uuid,
        request: crate::subscription_preview::StageRequest,
    },
    Status {},
    Catalog {
        offset: usize,
        expected_revision: Option<u64>,
    },
    Configure {
        expected_revision: u64,
        action: ConfigAction,
    },
    RequestStatus {
        request_id: Uuid,
    },
    ControlCore {
        action: CoreAction,
    },
    CoreRequestStatus {
        request_id: Uuid,
    },
    CoreStatus {},
    Launch {
        instance_id: Uuid,
        origin: LaunchOrigin,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<u64>,
    },
    LaunchStatus {
        request_id: Uuid,
    },
    CancelLaunch {
        request_id: Uuid,
    },
    GuardStatus {
        instance_id: Uuid,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    request_id: Uuid,
    epoch: Uuid,
    result: Reply,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    InstanceSettings {
        settings: crate::instance_settings::Summary,
    },
    ShortcutRequest {
        status: Option<app_proxy_windows::shortcuts::journal::Status>,
    },
    ShortcutStatus {
        status: crate::shortcuts::InstanceStatus,
    },
    RuntimeStatus {
        status: Box<crate::launch_engine::RuntimeStatus>,
    },
    SubscriptionNodes {
        page: crate::subscription_preview::SavedPage,
    },
    SubscriptionPreview {
        page: crate::subscription_preview::PreviewPage,
    },
    SubscriptionStaged {
        staged: Box<app_proxy_windows::subscription_stage::StagedSubscription>,
    },
    SubscriptionPreviewClosed {},
    Status {
        status: Status,
    },
    Catalog {
        page: CatalogPage,
    },
    Configured {
        outcome: ConfigOutcome,
    },
    RequestStatus {
        status: Option<ConfigRequestStatus>,
    },
    CoreRequestStatus {
        status: Option<CoreRequestStatus>,
    },
    CoreStatus {
        snapshot: crate::core_manager::CoreSnapshot,
    },
    LaunchStatus {
        attempt: Option<LaunchAttempt>,
    },
    GuardStatus {
        status: Box<crate::guard_control::GuardStatus>,
    },
    Error {
        code: String,
    },
}

struct Shared {
    root: PathBuf,
    shortcut_jobs: Arc<tokio::sync::Semaphore>,
    identity: Status,
    configuration: Arc<Configuration>,
    core: CoreControl,
    launch: Arc<crate::launch_engine::LaunchEngine>,
    guard_queries: Arc<tokio::sync::Semaphore>,
    guard_monitor: Arc<crate::guard_monitor::Monitor>,
    subscription: Arc<crate::subscription_preview::PreviewService>,
    jobs: AtomicUsize,
    job_finished: tokio::sync::Notify,
}

impl Shared {
    fn new(root: PathBuf, store: store::Store, identity: Status) -> Result<Self> {
        let configuration = Arc::new(Configuration::new(store));
        #[cfg(test)]
        let resources = app_proxy_windows::instance_resource::ResourceRegistry::for_test_at(
            &root.join("test-resources"),
        )?;
        let core = CoreControl::new(root.clone(), configuration.clone(), identity.epoch);
        #[cfg(not(test))]
        let launch = crate::launch_engine::LaunchEngine::new(
            configuration.clone(),
            core.manager(),
            identity.epoch,
        )?;
        #[cfg(test)]
        let launch = crate::launch_engine::LaunchEngine::with_resources(
            configuration.clone(),
            core.manager(),
            identity.epoch,
            resources,
        )?;
        Ok(Self {
            root,
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
    fn idle_allowed(&self) -> Result<bool> {
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
    fn update(&self, manifest: &Manifest) -> Status {
        Status {
            revision: manifest.revision,
            applications: manifest.applications.len(),
            instances: manifest.instances.len(),
            profiles: manifest.profiles.len(),
            ..self.identity.clone()
        }
    }
    fn execute(&self, request: Request) -> Result<Reply> {
        match request.operation {
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
            Operation::ControlCore { .. } => Err(Error::Invalid("CORE_CONTROL_REQUIRES_ADMISSION")),
            Operation::CoreStatus {} => Ok(Reply::CoreStatus {
                snapshot: self.core.snapshot()?,
            }),
            Operation::Launch { .. } => Err(Error::Invalid("LAUNCH_REQUIRES_ADMISSION")),
            Operation::GuardStatus { .. } => {
                Err(Error::Invalid("GUARD_STATUS_REQUIRES_ASYNC_QUERY"))
            }
            Operation::RuntimeStatus { .. } => {
                Err(Error::Invalid("RUNTIME_STATUS_REQUIRES_ASYNC_QUERY"))
            }
            Operation::LaunchStatus { request_id } => Ok(Reply::LaunchStatus {
                attempt: self.launch.status(request_id)?,
            }),
            Operation::CancelLaunch { request_id } => Ok(Reply::LaunchStatus {
                attempt: Some(self.launch.cancel(request_id)?),
            }),
        }
    }
}

fn safe_error(error: Error) -> String {
    match error {
        Error::Invalid(code) => code.into(),
        _ => "COORDINATOR_OPERATION_FAILED".into(),
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub store_id: Uuid,
    pub revision: u64,
    pub epoch: Uuid,
    pub coordinator_pid: u32,
    pub session_id: u32,
    pub applications: usize,
    pub instances: usize,
    pub profiles: usize,
    pub phase: String,
}

fn hello(store_id: Uuid, session_id: u32, epoch: Option<Uuid>) -> Hello {
    Hello {
        protocol_major: PROTOCOL_MAJOR,
        protocol_minor: PROTOCOL_MINOR,
        version: env!("CARGO_PKG_VERSION").into(),
        store_id,
        session_id,
        epoch,
    }
}

pub fn default_home() -> Result<PathBuf> {
    Ok(app_proxy_windows::instance_data::local_app_data()?.join("AppProxyRust"))
}

fn binaries() -> Result<(PathBuf, PathBuf)> {
    let current = std::env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(Error::Invalid("PROGRAM_DIRECTORY_UNAVAILABLE"))?;
    Ok((
        directory.join("app-proxy.exe"),
        directory.join("app-proxy-host.exe"),
    ))
}

pub async fn serve(root: PathBuf) -> Result<()> {
    identity::assert_ordinary_user()?;
    let owned = store::Store::open(&root)?;
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

async fn serve_connections(
    mut listener: ipc::Listener,
    shared: Arc<Shared>,
    idle_timeout: Duration,
) -> Result<()> {
    let _guard_listener = shared.guard_monitor.start()?;
    let mut idle_allowed = shared.idle_allowed()?;
    let mut clients = tokio::task::JoinSet::new();
    let mut idle_deadline = tokio::time::Instant::now() + idle_timeout;
    let mut maintenance = tokio::time::interval(Duration::from_secs(5));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            accepted = listener.accept(), if clients.len() < MAX_CLIENTS => {
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

async fn handle(
    mut connection: ipc::Connection<NamedPipeServer>,
    shared: Arc<Shared>,
) -> Result<()> {
    let status = &shared.identity;
    let request: Hello = connection.receive().await?;
    let client_minor = request.protocol_minor;
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
        return Ok(());
    }
    connection
        .send(&Welcome::Ready {
            hello: hello(status.store_id, status.session_id, Some(status.epoch)),
        })
        .await?;
    let request: Request = connection.receive().await?;
    if request.protocol_major != PROTOCOL_MAJOR || request.request_id.is_nil() {
        return Err(Error::Invalid("INVALID_RPC_REQUEST"));
    }
    let request_id = request.request_id;
    let epoch = status.epoch;
    if instance_edit_operation(&request.operation) && client_minor < 17 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "INSTANCE_EDIT_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if shortcut_operation(&request.operation) && client_minor < 16 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "SHORTCUT_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if matches!(&request.operation, Operation::SubscriptionNodes { .. }) && client_minor < 14 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "SUBSCRIPTION_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if subscription_preview_operation(&request.operation) && client_minor < 13 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "SUBSCRIPTION_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if subscription_edit_operation(&request.operation) && client_minor < 12 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "SUBSCRIPTION_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if matches!(&request.operation, Operation::Catalog { .. }) && client_minor < 11 {
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: Reply::Error {
                    code: "CATALOG_PROTOCOL_UPDATE_REQUIRED".into(),
                },
            })
            .await;
    }
    if let Operation::RuntimeStatus { instance_id } = request.operation {
        if client_minor < 15 {
            return connection
                .send(&Response {
                    request_id,
                    epoch,
                    result: Reply::Error {
                        code: "RUNTIME_PROTOCOL_UPDATE_REQUIRED".into(),
                    },
                })
                .await;
        }
        let permit = shared.guard_queries.clone().try_acquire_owned().ok();
        let runtime = tokio::runtime::Handle::current();
        let result = tokio::task::spawn_blocking(move || {
            let allowed = permit.is_some();
            let _permit = permit;
            runtime.block_on(shared.launch.observe_instance(instance_id, allowed))
        })
        .await
        .map_err(|_| Error::Invalid("RUNTIME_STATUS_WORKER_FAILED"))?;
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: match result {
                    Ok(status) => Reply::RuntimeStatus {
                        status: Box::new(status),
                    },
                    Err(error) => Reply::Error {
                        code: safe_error(error),
                    },
                },
            })
            .await;
    }
    if let Operation::GuardStatus { instance_id } = request.operation {
        if client_minor < 10 {
            return connection
                .send(&Response {
                    request_id,
                    epoch,
                    result: Reply::Error {
                        code: "GUARD_PROTOCOL_UPDATE_REQUIRED".into(),
                    },
                })
                .await;
        }
        let permit = shared.guard_queries.clone().try_acquire_owned().ok();
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
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: match result {
                    Ok(status) => Reply::GuardStatus {
                        status: Box::new(status),
                    },
                    Err(error) => Reply::Error {
                        code: safe_error(error),
                    },
                },
            })
            .await;
    }
    if let Operation::Launch {
        instance_id,
        origin,
        expected_revision,
    } = request.operation
    {
        let engine = shared.launch.clone();
        let runtime = tokio::runtime::Handle::current();
        let admitted = tokio::task::spawn_blocking(move || {
            runtime.block_on(engine.submit_at_revision(
                LaunchRequest {
                    request_id,
                    instance_id,
                    origin,
                },
                expected_revision,
            ))
        })
        .await
        .map_err(|_| Error::Invalid("LAUNCH_WORKER_FAILED"))?;
        // The engine owns accepted work before this ACK; a failed send cannot
        // discard it or consume a connection slot during proxy preparation.
        return connection
            .send(&Response {
                request_id,
                epoch,
                result: match admitted {
                    Ok(attempt) => Reply::LaunchStatus {
                        attempt: Some(attempt),
                    },
                    Err(error) => Reply::Error {
                        code: safe_error(error),
                    },
                },
            })
            .await;
    }
    if let Operation::ControlCore { action } = request.operation {
        let worker = shared.clone();
        let admitted = tokio::task::spawn_blocking(move || worker.core.accept(request_id, &action))
            .await
            .map_err(|_| Error::Invalid("CORE_CONTROL_WORKER_FAILED"))?;
        let (reply, execute) = match admitted {
            Ok((status, execute)) => (
                Reply::CoreRequestStatus {
                    status: Some(status),
                },
                execute,
            ),
            Err(error) => (
                Reply::Error {
                    code: safe_error(error),
                },
                None,
            ),
        };
        let sent = connection
            .send(&Response {
                request_id,
                epoch,
                result: reply,
            })
            .await;
        // Long work has its own lifetime, not an IPC slot: queries and cancellation
        // must remain available while all admitted installers are waiting.
        if let Some(job) = execute {
            shared.jobs.fetch_add(1, Ordering::SeqCst);
            let completed = JobCompletion(shared.clone());
            let runtime = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let _completed = completed;
                let _ = runtime.block_on(shared.core.execute(job));
            });
        }
        return sent;
    }
    // Do not cancel accepted work when the client disconnects. The handler stays
    // registered until blocking preparation/commit completes, preventing idle exit.
    let shortcut_permit = if matches!(
        &request.operation,
        Operation::ShortcutApply { .. } | Operation::ShortcutResume { .. }
    ) {
        match shared.shortcut_jobs.clone().try_acquire_owned() {
            Ok(permit) => Some(permit),
            Err(_) => {
                return connection
                    .send(&Response {
                        request_id,
                        epoch,
                        result: Reply::Error {
                            code: "SHORTCUT_OPERATION_BUSY".into(),
                        },
                    })
                    .await;
            }
        }
    } else {
        None
    };
    let result = tokio::task::spawn_blocking(move || {
        let _permit = shortcut_permit;
        shared.execute(request)
    })
    .await
    .map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))?;
    connection
        .send(&Response {
            request_id,
            epoch,
            result: result.unwrap_or_else(|e| Reply::Error {
                code: safe_error(e),
            }),
        })
        .await
}

struct JobCompletion(Arc<Shared>);
impl Drop for JobCompletion {
    fn drop(&mut self) {
        self.0.jobs.fetch_sub(1, Ordering::SeqCst);
        self.0.job_finished.notify_one();
    }
}

async fn query(store_id: Uuid, policy: &ipc::PeerPolicy, wait: Duration) -> Result<Status> {
    let request = Request {
        protocol_major: PROTOCOL_MAJOR,
        request_id: Uuid::new_v4(),
        operation: Operation::Status {},
    };
    match rpc(store_id, policy, wait, request).await? {
        Reply::Status { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

async fn rpc(
    store_id: Uuid,
    policy: &ipc::PeerPolicy,
    wait: Duration,
    request: Request,
) -> Result<Reply> {
    let mut connection = ipc::connect(store_id, policy, wait).await?;
    connection
        .send(&hello(store_id, policy.session_id, None))
        .await?;
    let welcome: Welcome = connection.receive().await?;
    let server = match welcome {
        Welcome::Ready { hello } => hello,
        Welcome::Rejected { code } => {
            return Err(Error::Invalid(match code.as_str() {
                "PROTOCOL_VERSION_MISMATCH" => "PROTOCOL_VERSION_MISMATCH",
                "STORE_SESSION_CONFLICT" => "STORE_SESSION_CONFLICT",
                "STORE_ID_MISMATCH" => "STORE_ID_MISMATCH",
                _ => "IPC_HANDSHAKE_REJECTED",
            }));
        }
    };
    if server.protocol_major != PROTOCOL_MAJOR
        || server.store_id != store_id
        || server.session_id != policy.session_id
        || server.epoch.is_none()
    {
        return Err(Error::Invalid("IPC_SERVER_HELLO_MISMATCH"));
    }
    if matches!(
        &request.operation,
        Operation::Launch { .. } | Operation::LaunchStatus { .. } | Operation::CancelLaunch { .. }
    ) && server.protocol_minor < 7
    {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if matches!(
        &request.operation,
        Operation::Launch {
            expected_revision: Some(_),
            ..
        }
    ) && server.protocol_minor < 8
    {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if matches!(&request.operation, Operation::RuntimeStatus { .. }) && server.protocol_minor < 15 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if shortcut_operation(&request.operation) && server.protocol_minor < 16 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if instance_edit_operation(&request.operation) && server.protocol_minor < 17 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if matches!(&request.operation, Operation::GuardStatus { .. }) && server.protocol_minor < 10 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if matches!(&request.operation, Operation::Catalog { .. }) && server.protocol_minor < 11 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if subscription_edit_operation(&request.operation) && server.protocol_minor < 12 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if subscription_preview_operation(&request.operation) && server.protocol_minor < 13 {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if matches!(&request.operation, Operation::SubscriptionNodes { .. })
        && server.protocol_minor < 14
    {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    let request_id = request.request_id;
    connection.send(&request).await?;
    let response: Response = connection.receive().await?;
    if response.request_id != request_id || Some(response.epoch) != server.epoch {
        return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
    }
    if let Reply::Status { status } = &response.result
        && (status.store_id != store_id
            || Some(status.epoch) != server.epoch
            || status.coordinator_pid != connection.peer.pid
            || status.session_id != connection.peer.session_id)
    {
        return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
    }
    if let Reply::Error { code } = &response.result {
        return Err(Error::Invalid(match code.as_str() {
            "INVALID_SHORTCUT_REQUEST" => "INVALID_SHORTCUT_REQUEST",
            "SHORTCUT_OPERATION_BUSY" => "SHORTCUT_OPERATION_BUSY",
            "SHORTCUT_ALREADY_REGISTERED" => "SHORTCUT_ALREADY_REGISTERED",
            "SHORTCUT_REGISTRATION_CHANGED" => "SHORTCUT_REGISTRATION_CHANGED",
            "SHORTCUT_OWNERSHIP_UNAVAILABLE" => "SHORTCUT_OWNERSHIP_UNAVAILABLE",
            "SHORTCUT_OPERATION_PENDING" => "SHORTCUT_OPERATION_PENDING",
            "SHORTCUT_REMOVAL_PENDING" => "SHORTCUT_REMOVAL_PENDING",
            "SHORTCUT_REQUEST_NOT_FOUND" => "SHORTCUT_REQUEST_NOT_FOUND",
            "SHORTCUT_PATH_OCCUPIED" => "SHORTCUT_PATH_OCCUPIED",
            "SHORTCUT_CHANGED" => "SHORTCUT_CHANGED",
            "SHORTCUT_METADATA_CONFLICT" => "SHORTCUT_METADATA_CONFLICT",
            "SHORTCUT_LOCATIONS_CONFLICT" => "SHORTCUT_LOCATIONS_CONFLICT",
            "SHORTCUT_JOURNAL_INVALID" => "SHORTCUT_JOURNAL_INVALID",
            "SHORTCUT_JOURNAL_REVISION_INVALID" => "SHORTCUT_JOURNAL_REVISION_INVALID",
            "SHORTCUT_RECORD_LIMIT" => "SHORTCUT_RECORD_LIMIT",
            "ICON_GROUP_MISSING" => "ICON_GROUP_MISSING",
            "ICON_CACHE_CONFLICT" => "ICON_CACHE_CONFLICT",
            "REQUEST_ID_CONFLICT" => "REQUEST_ID_CONFLICT",
            "INVALID_REQUEST_ID" => "INVALID_REQUEST_ID",
            "CONFIG_REQUEST_PENDING" => "CONFIG_REQUEST_PENDING",
            "CATALOG_CHANGED" => "CATALOG_CHANGED",
            "CATALOG_ENTRY_TOO_LARGE" => "CATALOG_ENTRY_TOO_LARGE",
            "SUBSCRIPTION_PREVIEW_EXPIRED" => "SUBSCRIPTION_PREVIEW_EXPIRED",
            "SUBSCRIPTION_PREVIEW_LIMIT" => "SUBSCRIPTION_PREVIEW_LIMIT",
            "SUBSCRIPTION_PREVIEW_NOT_READY" => "SUBSCRIPTION_PREVIEW_NOT_READY",
            "SUBSCRIPTION_STAGE_PENDING" => "SUBSCRIPTION_STAGE_PENDING",
            "SUBSCRIPTION_STAGE_FAILED" => "SUBSCRIPTION_STAGE_FAILED",
            "SUBSCRIPTION_STAGE_TOO_LARGE" => "SUBSCRIPTION_STAGE_TOO_LARGE",
            "SUBSCRIPTION_PREVIEW_KIND_MISMATCH" => "SUBSCRIPTION_PREVIEW_KIND_MISMATCH",
            "STALE_SUBSCRIPTION_SOURCE" => "STALE_SUBSCRIPTION_SOURCE",
            "SUBSCRIPTION_SELECTED_NODE_REMOVED" => "SUBSCRIPTION_SELECTED_NODE_REMOVED",
            "SUBSCRIPTION_URL_INVALID" => "SUBSCRIPTION_URL_INVALID",
            "SUBSCRIPTION_PROFILE_REQUIRED" => "SUBSCRIPTION_PROFILE_REQUIRED",
            "SUBSCRIPTION_REQUEST_TOO_LARGE" => "SUBSCRIPTION_REQUEST_TOO_LARGE",
            "SELECTED_NODE_NOT_FOUND" => "SELECTED_NODE_NOT_FOUND",
            "INVALID_SUBSCRIPTION_NODE" => "INVALID_SUBSCRIPTION_NODE",
            "INVALID_SUBSCRIPTION_SECRET" => "INVALID_SUBSCRIPTION_SECRET",
            "SECRET_ID_CONFLICT" => "SECRET_ID_CONFLICT",
            "STALE_MANIFEST_REVISION" => "STALE_MANIFEST_REVISION",
            "PROFILE_NOT_FOUND" => "PROFILE_NOT_FOUND",
            "LAUNCH_ATTEMPT_NOT_FOUND" => "LAUNCH_ATTEMPT_NOT_FOUND",
            "LAUNCH_OPERATION_LIMIT" => "LAUNCH_OPERATION_LIMIT",
            "LAUNCH_CONFIG_CHANGED" => "LAUNCH_CONFIG_CHANGED",
            "INSTANCE_RESOURCE_BUSY" => "INSTANCE_RESOURCE_BUSY",
            "INSTANCE_NOT_FOUND" => "INSTANCE_NOT_FOUND",
            "INSTANCE_RUNNING_WITH_OTHER_CONFIG" => "INSTANCE_RUNNING_WITH_OTHER_CONFIG",
            "INSTANCE_RUNNING_IN_OTHER_SESSION" => "INSTANCE_RUNNING_IN_OTHER_SESSION",
            _ => "COORDINATOR_OPERATION_FAILED",
        }));
    }
    Ok(response.result)
}

/// On failure retain the same request ID and query its status; never manufacture
/// a fresh ID to retry an operation whose response may have been lost.
pub async fn configure(root: PathBuf, request: ConfigRequest) -> Result<ConfigOutcome> {
    let operation = Operation::Configure {
        expected_revision: request.expected_revision,
        action: request.action,
    };
    match client_operation(root, request.request_id, operation).await? {
        Reply::Configured { outcome } => Ok(outcome),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

fn instance_edit_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::InstanceSettings { .. }
            | Operation::Configure {
                action: ConfigAction::EditInstance { .. },
                ..
            }
    )
}
pub async fn instance_settings(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::instance_settings::Summary> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::InstanceSettings { instance_id },
    )
    .await?
    {
        Reply::InstanceSettings { settings } if settings.instance_id == instance_id => Ok(settings),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
fn shortcut_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::ShortcutApply { .. }
            | Operation::ShortcutResume { .. }
            | Operation::ShortcutRequest { .. }
            | Operation::ShortcutStatus { .. }
    )
}

pub async fn shortcut_apply(
    root: PathBuf,
    request: app_proxy_windows::shortcuts::journal::Request,
) -> Result<app_proxy_windows::shortcuts::journal::Status> {
    store::describe(&root)?;
    match client_operation(root, request.id, Operation::ShortcutApply { request }).await? {
        Reply::ShortcutRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_resume(
    root: PathBuf,
    request_id: Uuid,
) -> Result<app_proxy_windows::shortcuts::journal::Status> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutResume { request_id },
    )
    .await?
    {
        Reply::ShortcutRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_request(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<app_proxy_windows::shortcuts::journal::Status>> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutRequest { request_id },
    )
    .await?
    {
        Reply::ShortcutRequest { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::shortcuts::InstanceStatus> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutStatus { instance_id },
    )
    .await?
    {
        Reply::ShortcutStatus { status } if status.instance_id == instance_id => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

fn subscription_edit_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::Configure {
            action: ConfigAction::EditSubscriptionProfile { .. },
            ..
        } | Operation::ControlCore {
            action: CoreAction::PrepareSubscription { .. }
        }
    )
}

fn subscription_preview_operation(operation: &Operation) -> bool {
    matches!(
        operation,
        Operation::SubscriptionPreview { .. }
            | Operation::SubscriptionPreviewPage { .. }
            | Operation::SubscriptionPreviewClose { .. }
            | Operation::SubscriptionStage { .. }
    )
}

pub async fn subscription_preview(
    root: PathBuf,
    id: Uuid,
    request: crate::subscription_preview::PreviewRequest,
) -> Result<crate::subscription_preview::PreviewPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreview { id, request },
    )
    .await?
    {
        Reply::SubscriptionPreview { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn subscription_nodes(
    root: PathBuf,
    profile_id: Uuid,
    offset: usize,
    expected_revision: Option<u64>,
) -> Result<crate::subscription_preview::SavedPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionNodes {
            profile_id,
            offset,
            expected_revision,
        },
    )
    .await?
    {
        Reply::SubscriptionNodes { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_preview_page(
    root: PathBuf,
    id: Uuid,
    offset: usize,
) -> Result<crate::subscription_preview::PreviewPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreviewPage { id, offset },
    )
    .await?
    {
        Reply::SubscriptionPreview { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_preview_close(root: PathBuf, id: Uuid) -> Result<()> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreviewClose { id },
    )
    .await?
    {
        Reply::SubscriptionPreviewClosed {} => Ok(()),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_stage(
    root: PathBuf,
    preview_id: Uuid,
    stage_id: Uuid,
    request: crate::subscription_preview::StageRequest,
) -> Result<app_proxy_windows::subscription_stage::StagedSubscription> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionStage {
            preview_id,
            stage_id,
            request,
        },
    )
    .await?
    {
        Reply::SubscriptionStaged { staged } => Ok(*staged),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn request_status(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<ConfigRequestStatus>> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::RequestStatus { request_id },
    )
    .await?
    {
        Reply::RequestStatus { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

/// Keep this ID if the response is lost; query it rather than submitting anew.
pub async fn control_core(
    root: PathBuf,
    request_id: Uuid,
    action: CoreAction,
) -> Result<CoreRequestStatus> {
    match client_operation(root, request_id, Operation::ControlCore { action }).await? {
        Reply::CoreRequestStatus {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn core_request_status(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<CoreRequestStatus>> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::CoreRequestStatus { request_id },
    )
    .await?
    {
        Reply::CoreRequestStatus { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn core_status(root: PathBuf) -> Result<crate::core_manager::CoreSnapshot> {
    match client_operation(root, Uuid::new_v4(), Operation::CoreStatus {}).await? {
        Reply::CoreStatus { snapshot } => Ok(snapshot),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

/// Keep the request ID across transport failures. Submission acknowledges the
/// durable attempt; only its queried phase describes the launch result.
pub async fn launch(root: PathBuf, request: LaunchRequest) -> Result<LaunchAttempt> {
    launch_at_revision(root, request, None).await
}

pub(crate) async fn launch_at_revision(
    root: PathBuf,
    request: LaunchRequest,
    expected_revision: Option<u64>,
) -> Result<LaunchAttempt> {
    match client_operation(
        root,
        request.request_id,
        Operation::Launch {
            instance_id: request.instance_id,
            origin: request.origin,
            expected_revision,
        },
    )
    .await?
    {
        Reply::LaunchStatus {
            attempt: Some(attempt),
        } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn launch_status(root: PathBuf, request_id: Uuid) -> Result<Option<LaunchAttempt>> {
    match client_operation(root, Uuid::new_v4(), Operation::LaunchStatus { request_id }).await? {
        Reply::LaunchStatus { attempt } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn cancel_launch(root: PathBuf, request_id: Uuid) -> Result<LaunchAttempt> {
    match client_operation(root, Uuid::new_v4(), Operation::CancelLaunch { request_id }).await? {
        Reply::LaunchStatus {
            attempt: Some(attempt),
        } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn runtime_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::launch_engine::RuntimeStatus> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::RuntimeStatus { instance_id },
    )
    .await?
    {
        Reply::RuntimeStatus { status } if status.instance_id == instance_id => Ok(*status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn guard_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::guard_control::GuardStatus> {
    match client_operation(root, Uuid::new_v4(), Operation::GuardStatus { instance_id }).await? {
        Reply::GuardStatus { status }
            if status.instance_id == instance_id
                && status.scan.as_ref().is_none_or(|s| {
                    s.instance_id == instance_id && s.revision == status.revision
                }) =>
        {
            Ok(*status)
        }
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn catalog(root: PathBuf) -> Result<CatalogPage> {
    let mut offset = 0;
    let mut combined: Option<CatalogPage> = None;
    loop {
        let expected_revision = combined.as_ref().map(|c| c.revision);
        let Reply::Catalog { mut page } = client_operation(
            root.clone(),
            Uuid::new_v4(),
            Operation::Catalog {
                offset,
                expected_revision,
            },
        )
        .await?
        else {
            return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
        };
        let next = page.next_offset;
        if let Some(all) = &mut combined {
            if page.revision != all.revision {
                return Err(Error::Invalid("CATALOG_CHANGED"));
            }
            all.applications.append(&mut page.applications);
            all.instances.append(&mut page.instances);
            all.profiles.append(&mut page.profiles);
        } else {
            page.next_offset = None;
            combined = Some(page);
        }
        let Some(next) = next else {
            return Ok(combined.expect("first page inserted"));
        };
        if next <= offset {
            return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
        }
        offset = next;
    }
}

async fn client_operation(root: PathBuf, request_id: Uuid, operation: Operation) -> Result<Reply> {
    let owner = status(root).await?;
    let (_, host) = binaries()?;
    let policy = ipc::PeerPolicy::current(vec![identity::file_identity(&host)?])?;
    rpc(
        owner.store_id,
        &policy,
        Duration::from_secs(1),
        Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id,
            operation,
        },
    )
    .await
}

/// Launch-on-demand status flow; no arbitrary commands or target processes are accepted.
pub async fn status(root: PathBuf) -> Result<Status> {
    identity::assert_ordinary_user()?;
    ensure_store(&root).await?;
    let descriptor = store::describe(&root)?;
    let (_, host) = binaries()?;
    let policy = ipc::PeerPolicy::current(vec![identity::file_identity(&host)?])?;
    match query(descriptor.store_id, &policy, Duration::from_millis(40)).await {
        Ok(status) => return Ok(status),
        Err(Error::Invalid("IPC_CONNECT_TIMEOUT")) => {}
        Err(error) => return Err(error),
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let _startup = loop {
        if let Some(lock) = store::try_startup_lock(&root)? {
            break lock;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("COORDINATOR_START_TIMEOUT"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    match query(descriptor.store_id, &policy, Duration::from_millis(40)).await {
        Ok(status) => return Ok(status),
        Err(Error::Invalid("IPC_CONNECT_TIMEOUT")) => {}
        Err(error) => return Err(error),
    }
    // Validate configuration before creating a background process. A held owner lock
    // is never interpreted as permission to kill or replace the existing owner.
    drop(store::Store::open(&root)?);
    let child = process::start_host(&host, &root)?;
    let result = query(descriptor.store_id, &policy, Duration::from_secs(5)).await;
    if result.is_err() && child.has_exited()? {
        return Err(Error::Invalid("COORDINATOR_START_FAILED"));
    }
    // Dropping our process handle does not terminate the coordinator or applications.
    result
}

async fn ensure_store(root: &Path) -> Result<()> {
    let attempted_create = !root.try_exists()? || std::fs::read_dir(root)?.next().is_none();
    if attempted_create {
        match store::Store::create(root) {
            Ok(store) => {
                drop(store);
                return Ok(());
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(Error::Invalid("STORE_NOT_EMPTY" | "STORE_ALREADY_OWNED")) => {}
            Err(error) => return Err(error),
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while root.join(".initializing").try_exists()?
        || (attempted_create && !root.join(".app-proxy-rust-owned.json").try_exists()?)
    {
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("STORE_INITIALIZATION_INCOMPLETE"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    store::describe(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> (tempfile::TempDir, Arc<Shared>) {
        let temp = tempfile::tempdir().unwrap();
        let store = store::Store::create(&temp.path().join("store")).unwrap();
        let id = store.load().unwrap().store_id;
        let current = identity::current().unwrap();
        let status = Status {
            store_id: id,
            revision: 1,
            epoch: Uuid::new_v4(),
            coordinator_pid: current.pid,
            session_id: current.session_id,
            applications: 0,
            instances: 0,
            profiles: 0,
            phase: "bootstrap".into(),
        };
        let shared = Arc::new(Shared::new(temp.path().join("store"), store, status).unwrap());
        (temp, shared)
    }
    fn policy() -> ipc::PeerPolicy {
        ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
    }

    async fn wait_core_result(shared: &Shared, id: Uuid) -> CoreRequestStatus {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let Some(status) = shared.core.request_status(id).unwrap()
                    && !matches!(status, CoreRequestStatus::Pending { .. })
                {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn long_installs_do_not_starve_status_cancellation_or_owner_idle_exit() {
        use app_proxy_core::core_control::CoreOutcome;
        let (_temp, shared) = snapshot();
        shared
            .core
            .hold_installer(Arc::new(tokio::sync::Notify::new()));
        let id = shared.identity.store_id;
        let listener = ipc::Listener::bind(id, policy()).unwrap();
        let owner = shared.clone();
        let server = tokio::spawn(serve_connections(
            listener,
            owner,
            Duration::from_millis(100),
        ));
        let mut installs = Vec::new();
        for _ in 0..MAX_CLIENTS {
            let request_id = Uuid::new_v4();
            installs.push(request_id);
            let reply = rpc(
                id,
                &policy(),
                Duration::from_secs(1),
                Request {
                    protocol_major: PROTOCOL_MAJOR,
                    request_id,
                    operation: Operation::ControlCore {
                        action: CoreAction::Install {},
                    },
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                reply,
                Reply::CoreRequestStatus {
                    status: Some(CoreRequestStatus::Pending { .. })
                }
            ));
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(shared.jobs.load(Ordering::SeqCst), MAX_CLIENTS);
        assert!(!server.is_finished());
        query(id, &policy(), Duration::from_secs(1)).await.unwrap();
        for request_id in &installs {
            rpc(
                id,
                &policy(),
                Duration::from_secs(1),
                Request {
                    protocol_major: PROTOCOL_MAJOR,
                    request_id: Uuid::new_v4(),
                    operation: Operation::ControlCore {
                        action: CoreAction::CancelInstall {
                            request_id: *request_id,
                        },
                    },
                },
            )
            .await
            .unwrap();
            assert!(matches!(
                wait_core_result(&shared, *request_id).await,
                CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Cancelled {},
                    ..
                }
            ));
        }
        tokio::time::timeout(Duration::from_secs(3), server)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(shared.jobs.load(Ordering::SeqCst), 0);
        assert!(shared.idle_allowed().unwrap());
    }

    fn add_request(path: PathBuf) -> Request {
        use app_proxy_core::model::*;
        Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::Configure {
                expected_revision: 1,
                action: ConfigAction::AddApplication {
                    application: Application {
                        id: Uuid::new_v4(),
                        revision: 1,
                        name: "fixture".into(),
                        locator: ApplicationLocator::Exe { path },
                        template_ref: Template::Codex,
                    },
                },
            },
        }
    }

    #[tokio::test]
    async fn core_lost_ack_still_completes_and_replay_cannot_change_action() {
        use app_proxy_core::core_control::CoreOutcome;
        let (_temp, shared) = snapshot();
        let id = shared.identity.store_id;
        let request_id = Uuid::new_v4();
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let owner = shared.clone();
        let server = tokio::spawn(async move {
            let _ = handle(listener.accept().await.unwrap(), owner.clone()).await;
            (listener, owner)
        });
        let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
            .await
            .unwrap();
        client
            .send(&hello(id, shared.identity.session_id, None))
            .await
            .unwrap();
        assert!(matches!(
            client.receive::<Welcome>().await.unwrap(),
            Welcome::Ready { .. }
        ));
        client
            .send(&Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation: Operation::ControlCore {
                    action: CoreAction::Stop {},
                },
            })
            .await
            .unwrap();
        drop(client);
        let (mut listener, owner) = server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), shared.job_finished.notified())
            .await
            .unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..3 {
                handle(listener.accept().await.unwrap(), owner.clone())
                    .await
                    .unwrap();
            }
        });
        let response = rpc(
            id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id: Uuid::new_v4(),
                operation: Operation::CoreRequestStatus { request_id },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            response,
            Reply::CoreRequestStatus {
                status: Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Stopped {},
                    ..
                })
            }
        ));
        let response = rpc(
            id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation: Operation::ControlCore {
                    action: CoreAction::Stop {},
                },
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            response,
            Reply::CoreRequestStatus {
                status: Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Stopped {},
                    ..
                })
            }
        ));
        let profile = Uuid::new_v4();
        let changed = rpc(
            id,
            &policy(),
            Duration::from_secs(1),
            Request {
                protocol_major: PROTOCOL_MAJOR,
                request_id,
                operation: Operation::ControlCore {
                    action: CoreAction::Start {
                        profiles: vec![profile],
                        required: profile,
                    },
                },
            },
        )
        .await;
        assert!(matches!(
            changed,
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn concurrent_core_requests_receive_one_durable_result() {
        let (_temp, shared) = snapshot();
        let id = shared.identity.store_id;
        let request_id = Uuid::new_v4();
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let owner = shared.clone();
        let server = tokio::spawn(async move {
            let mut handlers = tokio::task::JoinSet::new();
            for _ in 0..6 {
                handlers.spawn(handle(listener.accept().await.unwrap(), owner.clone()));
            }
            while let Some(result) = handlers.join_next().await {
                result.unwrap().unwrap();
            }
        });
        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..6 {
            clients.spawn(async move {
                rpc(
                    id,
                    &policy(),
                    Duration::from_secs(1),
                    Request {
                        protocol_major: PROTOCOL_MAJOR,
                        request_id,
                        operation: Operation::ControlCore {
                            action: CoreAction::Stop {},
                        },
                    },
                )
                .await
                .unwrap()
            });
        }
        while let Some(response) = clients.join_next().await {
            assert!(matches!(
                response.unwrap(),
                Reply::CoreRequestStatus {
                    status: Some(
                        CoreRequestStatus::Pending { .. } | CoreRequestStatus::Complete { .. }
                    )
                }
            ));
        }
        server.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), shared.job_finished.notified())
            .await
            .unwrap();
        assert!(matches!(
            shared.core.request_status(request_id).unwrap(),
            Some(CoreRequestStatus::Complete {
                outcome: app_proxy_core::core_control::CoreOutcome::Stopped {},
                ..
            })
        ));
        assert!(shared.idle_allowed().unwrap());
    }

    #[test]
    fn delayed_status_snapshots_cannot_change_current_guard_idle_policy() {
        use app_proxy_core::{model::*, registry::*};
        let (temp, shared) = snapshot();
        let path = temp.path().join("fixture.exe");
        std::fs::write(&path, b"not executed").unwrap();
        shared.execute(add_request(path)).unwrap();
        let mut manifest = shared.configuration.snapshot().unwrap();
        let app_id = manifest.applications[0].id;
        let mut example: Manifest =
            serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
        let profile = example.profiles.pop().unwrap();
        let profile_id = profile.id;
        let change = |revision, action| ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: revision,
            action,
        };
        assert!(matches!(
            shared
                .configuration
                .apply(&change(2, ConfigAction::AddProfile { profile }))
                .unwrap(),
            ConfigOutcome::Applied { .. }
        ));
        let instance_id = Uuid::new_v4();
        assert!(matches!(
            shared
                .configuration
                .apply(&change(
                    3,
                    ConfigAction::CreateInstance {
                        instance: NewInstance {
                            id: instance_id,
                            application_id: app_id,
                            name: "guarded".into(),
                            data: NewData::Original {},
                            network: NetworkBinding::Profile { profile_id },
                            guard: None,
                            args: vec![],
                            env: SavedEnvironment::default(),
                            cwd: WorkingDirectory::Application {}
                        }
                    }
                ))
                .unwrap(),
            ConfigOutcome::Applied { .. }
        ));
        shared.update(&manifest); // Delayed snapshot from before enabling Guard.
        assert!(!shared.idle_allowed().unwrap());
        manifest = shared.configuration.snapshot().unwrap();
        assert!(matches!(
            shared
                .configuration
                .apply(&change(
                    4,
                    ConfigAction::BindInstance {
                        instance_id,
                        network: NetworkBinding::Direct {},
                        guard: None
                    }
                ))
                .unwrap(),
            ConfigOutcome::Applied { .. }
        ));
        shared.update(&manifest); // Delayed snapshot from before disabling Guard.
        assert!(shared.idle_allowed().unwrap());
    }

    #[tokio::test]
    async fn concurrent_pipe_replays_commit_once_and_status_reflects_the_edit() {
        let (temp, shared) = snapshot();
        let path = temp.path().join("fixture.exe");
        std::fs::write(&path, b"not executed").unwrap();
        let id = shared.identity.store_id;
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let owner = shared.clone();
        let server = tokio::spawn(async move {
            let mut clients = tokio::task::JoinSet::new();
            for _ in 0..7 {
                let pipe = listener.accept().await.unwrap();
                clients.spawn(handle(pipe, owner.clone()));
            }
            while let Some(result) = clients.join_next().await {
                result.unwrap().unwrap();
            }
        });
        let request = add_request(path);
        let bytes = serde_json::to_vec(&request).unwrap();
        let mut clients = tokio::task::JoinSet::new();
        for _ in 0..6 {
            let request = serde_json::from_slice(&bytes).unwrap();
            clients.spawn(async move {
                rpc(id, &policy(), Duration::from_secs(1), request)
                    .await
                    .unwrap()
            });
        }
        let mut entity = None;
        while let Some(result) = clients.join_next().await {
            let Reply::Configured {
                outcome: ConfigOutcome::Applied { receipt },
            } = result.unwrap()
            else {
                panic!("request rejected")
            };
            assert_eq!(receipt.revision, 2);
            if let Some(previous) = entity {
                assert_eq!(receipt.entity_id, previous);
            }
            entity = Some(receipt.entity_id);
        }
        let status = query(id, &policy(), Duration::from_secs(1)).await.unwrap();
        assert_eq!(status.revision, 2);
        assert_eq!(status.applications, 1);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn lost_response_can_be_queried_and_changed_payload_cannot_reuse_id() {
        let (temp, shared) = snapshot();
        let path = temp.path().join("fixture.exe");
        std::fs::write(&path, b"not executed").unwrap();
        let id = shared.identity.store_id;
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let owner = shared.clone();
        let request = add_request(path.clone());
        let request_id = request.request_id;
        let server = tokio::spawn(async move {
            let _ = handle(listener.accept().await.unwrap(), owner.clone()).await;
            (listener, owner)
        });
        let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
            .await
            .unwrap();
        client
            .send(&hello(id, shared.identity.session_id, None))
            .await
            .unwrap();
        assert!(matches!(
            client.receive::<Welcome>().await.unwrap(),
            Welcome::Ready { .. }
        ));
        client.send(&request).await.unwrap();
        // Intentionally discard the response. Closing a client does not undo its
        // accepted write; the durable receipt is the authority on reconnect.
        drop(client);
        let (mut listener, owner) = server.await.unwrap();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                handle(listener.accept().await.unwrap(), owner.clone())
                    .await
                    .unwrap();
            }
        });
        let lookup = Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id: Uuid::new_v4(),
            operation: Operation::RequestStatus { request_id },
        };
        assert!(matches!(
            rpc(id, &policy(), Duration::from_secs(1), lookup)
                .await
                .unwrap(),
            Reply::RequestStatus {
                status: Some(ConfigRequestStatus::Complete {
                    outcome: ConfigOutcome::Applied { .. }
                })
            }
        ));
        let mut changed = add_request(path);
        changed.request_id = request_id;
        assert!(matches!(
            rpc(id, &policy(), Duration::from_secs(1), changed).await,
            Err(Error::Invalid("REQUEST_ID_CONFLICT"))
        ));
        server.await.unwrap();
        assert_eq!(shared.configuration.snapshot().unwrap().revision, 2);
    }

    #[tokio::test]
    async fn handshake_rejects_major_store_session_and_client_epoch() {
        for expected in [
            "PROTOCOL_VERSION_MISMATCH",
            "STORE_ID_MISMATCH",
            "STORE_SESSION_CONFLICT",
            "INVALID_CLIENT_HELLO",
        ] {
            let (_temp, status) = snapshot();
            let id = status.identity.store_id;
            let mut listener = ipc::Listener::bind(id, policy()).unwrap();
            let mut request = hello(id, status.identity.session_id, None);
            match expected {
                "PROTOCOL_VERSION_MISMATCH" => request.protocol_major += 1,
                "STORE_ID_MISMATCH" => request.store_id = Uuid::new_v4(),
                "STORE_SESSION_CONFLICT" => request.session_id += 1,
                _ => request.epoch = Some(Uuid::new_v4()),
            }
            let task = tokio::spawn(async move {
                handle(listener.accept().await.unwrap(), status)
                    .await
                    .unwrap()
            });
            let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
                .await
                .unwrap();
            client.send(&request).await.unwrap();
            match client.receive::<Welcome>().await.unwrap() {
                Welcome::Rejected { code } => assert_eq!(code, expected),
                _ => panic!("invalid hello accepted"),
            }
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn stalled_client_does_not_block_another_status_request() {
        let (_temp, status) = snapshot();
        let id = status.identity.store_id;
        let mut listener = ipc::Listener::bind(id, policy()).unwrap();
        let server = tokio::spawn(async move {
            let stalled = listener.accept().await.unwrap();
            let stalled_task = tokio::spawn(handle(stalled, status.clone()));
            handle(listener.accept().await.unwrap(), status)
                .await
                .unwrap();
            stalled_task.abort();
            let _ = stalled_task.await;
        });
        let _stalled = ipc::connect(id, &policy(), Duration::from_secs(1))
            .await
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            query(id, &policy(), Duration::from_secs(1)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(result.store_id, id);
        server.await.unwrap();
    }
}
