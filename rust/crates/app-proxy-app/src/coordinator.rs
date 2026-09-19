//! Bootstrap coordinator: authenticated status RPC and one owner per store.
//! Mutating operations are added only together with their durable request journal.
use app_proxy_windows::{Error, Result, identity, ipc, process, store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::windows::named_pipe::NamedPipeServer;
use uuid::Uuid;

const PROTOCOL_MAJOR: u32 = 1;
const PROTOCOL_MINOR: u32 = 0;
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
#[serde(rename_all = "snake_case")]
enum Operation {
    Status,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Response {
    request_id: Uuid,
    result: Status,
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
    let base =
        std::env::var_os("LOCALAPPDATA").ok_or(Error::Invalid("LOCAL_APP_DATA_UNAVAILABLE"))?;
    let path = PathBuf::from(base).join("AppProxyRust");
    if !path.is_absolute() {
        return Err(Error::Invalid("LOCAL_APP_DATA_NOT_ABSOLUTE"));
    }
    Ok(path)
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
    let snapshot = Arc::new(Status {
        store_id: manifest.store_id,
        revision: manifest.revision,
        epoch: Uuid::new_v4(),
        coordinator_pid: current.pid,
        session_id: current.session_id,
        applications: manifest.applications.len(),
        instances: manifest.instances.len(),
        profiles: manifest.profiles.len(),
        phase: "bootstrap".into(),
    });
    // Desired guards keep the owner alive, even while their implementation is not yet available.
    let idle_allowed = !manifest
        .instances
        .iter()
        .any(|i| i.guard.desired == app_proxy_core::model::Desired::Enabled);
    let mut clients = tokio::task::JoinSet::new();
    let mut idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
    loop {
        tokio::select! {
            accepted = listener.accept(), if clients.len() < MAX_CLIENTS => {
                match accepted {
                    Ok(connection) => {
                        idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT;
                        let snapshot = snapshot.clone();
                        clients.spawn(async move { handle(connection, snapshot).await });
                    }
                    Err(ipc::AcceptError::Peer(_)) => {},
                    Err(ipc::AcceptError::Listener(error)) => return Err(error),
                }
            }
            _ = clients.join_next(), if !clients.is_empty() => {
                if clients.is_empty() { idle_deadline = tokio::time::Instant::now() + IDLE_TIMEOUT; }
            },
            _ = tokio::time::sleep_until(idle_deadline), if idle_allowed && clients.is_empty() => break,
        }
    }
    drop(listener);
    drop(owned);
    Ok(())
}

async fn handle(
    mut connection: ipc::Connection<NamedPipeServer>,
    status: Arc<Status>,
) -> Result<()> {
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
    match request.operation {
        Operation::Status => {
            connection
                .send(&Response {
                    request_id: request.request_id,
                    result: (*status).clone(),
                })
                .await
        }
    }
}

async fn query(store_id: Uuid, policy: &ipc::PeerPolicy, wait: Duration) -> Result<Status> {
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
    let request_id = Uuid::new_v4();
    connection
        .send(&Request {
            protocol_major: PROTOCOL_MAJOR,
            request_id,
            operation: Operation::Status,
        })
        .await?;
    let response: Response = connection.receive().await?;
    if response.request_id != request_id
        || response.result.store_id != store_id
        || Some(response.result.epoch) != server.epoch
        || response.result.coordinator_pid != connection.peer.pid
        || response.result.session_id != connection.peer.session_id
    {
        return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
    }
    Ok(response.result)
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

    fn snapshot(id: Uuid) -> Arc<Status> {
        let current = identity::current().unwrap();
        Arc::new(Status {
            store_id: id,
            revision: 1,
            epoch: Uuid::new_v4(),
            coordinator_pid: current.pid,
            session_id: current.session_id,
            applications: 0,
            instances: 0,
            profiles: 0,
            phase: "bootstrap".into(),
        })
    }
    fn policy() -> ipc::PeerPolicy {
        ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
    }

    #[tokio::test]
    async fn handshake_rejects_major_store_session_and_client_epoch() {
        for expected in [
            "PROTOCOL_VERSION_MISMATCH",
            "STORE_ID_MISMATCH",
            "STORE_SESSION_CONFLICT",
            "INVALID_CLIENT_HELLO",
        ] {
            let id = Uuid::new_v4();
            let status = snapshot(id);
            let mut listener = ipc::Listener::bind(id, policy()).unwrap();
            let mut request = hello(id, status.session_id, None);
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
        let id = Uuid::new_v4();
        let status = snapshot(id);
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
