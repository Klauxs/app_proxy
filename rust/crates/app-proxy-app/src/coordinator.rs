//! Authenticated coordinator RPC; configuration writes use durable request records.
use crate::configuration::{CatalogPage, Configuration, catalog_page};
use app_proxy_core::{
    model::Manifest,
    registry::{ConfigAction, ConfigRequest},
};
use app_proxy_windows::config_transaction::{ConfigOutcome, ConfigRequestStatus};
use app_proxy_windows::{Error, Result, identity, ipc, process, store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::windows::named_pipe::NamedPipeServer;
use uuid::Uuid;

const PROTOCOL_MAJOR: u32 = 2;
const PROTOCOL_MINOR: u32 = 1;
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CLIENTS: usize = 16;

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
    Status { status: Status },
    Catalog { page: CatalogPage },
    Configured { outcome: ConfigOutcome },
    RequestStatus { status: Option<ConfigRequestStatus> },
    Error { code: String },
}

struct Shared {
    identity: Status,
    configuration: Configuration,
}

impl Shared {
    fn idle_allowed(&self) -> Result<bool> {
        Ok(self
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
    let mut listener = ipc::Listener::bind(manifest.store_id, policy)?;
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
    // Desired guards keep the owner alive, even while their implementation is not yet available.
    let mut idle_allowed = !manifest
        .instances
        .iter()
        .any(|i| i.guard.desired == app_proxy_core::model::Desired::Enabled);
    let shared = Arc::new(Shared {
        identity: snapshot,
        configuration: Configuration::new(owned),
    });
    let mut clients = tokio::task::JoinSet::new();
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
    loop {
        tokio::select! {
            accepted = listener.accept(), if clients.len() < MAX_CLIENTS => {
                match accepted {
                    Ok(connection) => {
                        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
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
                    // All accepted work is done. Read once here so a delayed
                    // status response cannot overwrite a newer guard decision.
                    idle_allowed = tokio::task::spawn_blocking(move || shared.idle_allowed()).await
                        .map_err(|_| Error::Invalid("CONFIGURATION_WORKER_FAILED"))??;
                    idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
                }
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
    // Do not cancel accepted work when the client disconnects. The handler stays
    // registered until blocking preparation/commit completes, preventing idle exit.
    let result = tokio::task::spawn_blocking(move || shared.execute(request))
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
            "REQUEST_ID_CONFLICT" => "REQUEST_ID_CONFLICT",
            "INVALID_REQUEST_ID" => "INVALID_REQUEST_ID",
            "CONFIG_REQUEST_PENDING" => "CONFIG_REQUEST_PENDING",
            "CATALOG_CHANGED" => "CATALOG_CHANGED",
            "CATALOG_ENTRY_TOO_LARGE" => "CATALOG_ENTRY_TOO_LARGE",
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
        (
            temp,
            Arc::new(Shared {
                identity: status,
                configuration: Configuration::new(store),
            }),
        )
    }
    fn policy() -> ipc::PeerPolicy {
        ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
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
