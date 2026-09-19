//! Authenticated local byte pipes. The coordinator owns request semantics.
use crate::{Error, Result, identity, last_error, wide};
use app_proxy_core::{FileIdentity, ProcessIdentity};
use serde::{Serialize, de::DeserializeOwned};
use std::os::windows::io::{AsRawHandle, OwnedHandle};
use std::ptr::null_mut;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use uuid::Uuid;
use windows_sys::Win32::Foundation::{HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::*;
use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
use windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION;
use windows_sys::Win32::System::Pipes::*;
use windows_sys::Win32::System::Threading::PROCESS_QUERY_LIMITED_INFORMATION;

pub const FRAME_LIMIT: usize = 1024 * 1024;
pub const FRAME_TIMEOUT: Duration = Duration::from_secs(5);

pub struct PeerPolicy {
    pub owner_sid: String,
    pub session_id: u32,
    pub allowed_images: Vec<FileIdentity>,
}

impl PeerPolicy {
    pub fn current(allowed_images: Vec<FileIdentity>) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let current = identity::current()?;
        if allowed_images.is_empty() {
            return Err(Error::Invalid("IPC_NO_ALLOWED_PEER"));
        }
        Ok(Self {
            owner_sid: current.user_sid,
            session_id: current.session_id,
            allowed_images,
        })
    }
}

pub fn address(store_id: Uuid, sid: &str) -> Result<String> {
    if store_id.is_nil()
        || !sid.starts_with("S-1-")
        || !sid
            .bytes()
            .all(|b| b.is_ascii_digit() || b == b'S' || b == b'-')
    {
        return Err(Error::Invalid("IPC_INVALID_ADDRESS"));
    }
    let name = format!(r"\\.\pipe\app-proxy-rust-{sid}-{store_id}");
    if name.len() > 256 {
        return Err(Error::Invalid("IPC_INVALID_ADDRESS"));
    }
    Ok(name)
}

pub struct Listener {
    name: String,
    policy: PeerPolicy,
    next: NamedPipeServer,
}

pub struct Connection<T> {
    stream: T,
    pub peer: ProcessIdentity,
    poisoned: bool,
    // Keep the exact process object alive; a PID is not retained as sole identity.
    _process: OwnedHandle,
}

impl Listener {
    pub fn bind(store_id: Uuid, policy: PeerPolicy) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let name = address(store_id, &policy.owner_sid)?;
        let next = server(&name, &policy.owner_sid, true)?;
        Ok(Self { name, policy, next })
    }

    pub async fn accept(&mut self) -> Result<Connection<NamedPipeServer>> {
        self.next.connect().await?;
        // Reserve the next instance before handing out this one: no unowned-name gap.
        let next = server(&self.name, &self.policy.owner_sid, false)?;
        let connected = std::mem::replace(&mut self.next, next);
        let (peer, process) = authenticate(connected.as_raw_handle(), &self.policy, true)?;
        Ok(Connection {
            stream: connected,
            peer,
            poisoned: false,
            _process: process,
        })
    }
}

pub async fn connect(
    store_id: Uuid,
    policy: &PeerPolicy,
    timeout: Duration,
) -> Result<Connection<NamedPipeClient>> {
    identity::assert_ordinary_user()?;
    let name = address(store_id, &policy.owner_sid)?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match ClientOptions::new()
            .security_qos_flags(SECURITY_IDENTIFICATION)
            .open(&name)
        {
            Ok(stream) => {
                let (peer, process) = authenticate(stream.as_raw_handle(), policy, false)?;
                return Ok(Connection {
                    stream,
                    peer,
                    poisoned: false,
                    _process: process,
                });
            }
            Err(error) if matches!(error.raw_os_error(), Some(2 | 231)) => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Invalid("IPC_CONNECT_TIMEOUT"));
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(error) => return Err(error.into()),
        }
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin> Connection<T> {
    pub async fn receive<M: DeserializeOwned>(&mut self) -> Result<M> {
        if self.poisoned {
            return Err(Error::Invalid("IPC_CONNECTION_CLOSED"));
        }
        self.poisoned = true; // A cancelled partial read also invalidates framing.
        let result = receive_frame(&mut self.stream, FRAME_TIMEOUT).await;
        self.poisoned = result.is_err();
        result
    }
    pub async fn send<M: Serialize>(&mut self, message: &M) -> Result<()> {
        if self.poisoned {
            return Err(Error::Invalid("IPC_CONNECTION_CLOSED"));
        }
        self.poisoned = true;
        let result = send_frame(&mut self.stream, message, FRAME_TIMEOUT).await;
        self.poisoned = result.is_err();
        result
    }
}

async fn receive_frame<T: AsyncRead + Unpin, M: DeserializeOwned>(
    stream: &mut T,
    timeout: Duration,
) -> Result<M> {
    tokio::time::timeout(timeout, async {
        let size = stream.read_u32_le().await? as usize;
        if size == 0 || size > FRAME_LIMIT {
            return Err(Error::Invalid("IPC_FRAME_SIZE"));
        }
        let mut bytes = vec![0; size];
        stream.read_exact(&mut bytes).await?;
        // Do not return serde's offending-value diagnostics to access logs.
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("IPC_INVALID_JSON"))
    })
    .await
    .map_err(|_| Error::Invalid("IPC_FRAME_TIMEOUT"))?
}

async fn send_frame<T: AsyncWrite + Unpin, M: Serialize>(
    stream: &mut T,
    message: &M,
    timeout: Duration,
) -> Result<()> {
    let bytes = serde_json::to_vec(message).map_err(|_| Error::Invalid("IPC_SERIALIZE_FAILED"))?;
    if bytes.is_empty() || bytes.len() > FRAME_LIMIT {
        return Err(Error::Invalid("IPC_FRAME_SIZE"));
    }
    tokio::time::timeout(timeout, async {
        stream.write_u32_le(bytes.len() as u32).await?;
        stream.write_all(&bytes).await?;
        stream.flush().await?;
        Ok(())
    })
    .await
    .map_err(|_| Error::Invalid("IPC_FRAME_TIMEOUT"))?
}

fn server(name: &str, sid: &str, first: bool) -> Result<NamedPipeServer> {
    let sddl = wide(std::ffi::OsStr::new(&format!("D:P(A;;GA;;;{sid})")))?;
    // SAFETY: Windows consumes the descriptor during create; freed after success or failure.
    unsafe {
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(last_error("CreatePipeAcl"));
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .in_buffer_size(65536)
            .out_buffer_size(65536)
            .create_with_security_attributes_raw(
                name,
                (&mut attributes as *mut SECURITY_ATTRIBUTES).cast(),
            );
        LocalFree(descriptor);
        Ok(result?)
    }
}

fn authenticate(
    pipe: HANDLE,
    policy: &PeerPolicy,
    server_side: bool,
) -> Result<(ProcessIdentity, OwnedHandle)> {
    // SAFETY: live connected pipe handle, PID output is correctly sized. Queries use one retained process handle.
    unsafe {
        let mut pid = 0;
        let success = if server_side {
            GetNamedPipeClientProcessId(pipe, &mut pid)
        } else {
            GetNamedPipeServerProcessId(pipe, &mut pid)
        };
        if success == 0 {
            return Err(last_error("GetPipePeer"));
        }
        let process = identity::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        identity::assert_ordinary_handle(process.as_raw_handle())?;
        let peer = identity::inspect_handle(process.as_raw_handle())?;
        if peer.user_sid != policy.owner_sid {
            return Err(Error::Invalid("IPC_PEER_USER_MISMATCH"));
        }
        if peer.session_id != policy.session_id {
            return Err(Error::Invalid("STORE_SESSION_CONFLICT"));
        }
        if !policy.allowed_images.contains(&peer.image_file) {
            return Err(Error::Invalid("IPC_PEER_IMAGE_MISMATCH"));
        }
        Ok((peer, process))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn frame_limit_is_checked_before_body_and_errors_hide_values() {
        let (mut reader, mut writer) = tokio::io::duplex(128);
        writer.write_u32_le((FRAME_LIMIT + 1) as u32).await.unwrap();
        assert_eq!(
            receive_frame::<_, serde_json::Value>(&mut reader, Duration::from_secs(1))
                .await
                .err()
                .unwrap()
                .to_string(),
            "IPC_FRAME_SIZE"
        );
        let (mut reader, mut writer) = tokio::io::duplex(128);
        writer
            .write_all(b"\x0c\x00\x00\x00SECRET_TOKEN")
            .await
            .unwrap();
        assert_eq!(
            receive_frame::<_, serde_json::Value>(&mut reader, Duration::from_secs(1))
                .await
                .err()
                .unwrap()
                .to_string(),
            "IPC_INVALID_JSON"
        );
    }
    #[tokio::test]
    async fn partial_frame_and_slow_receiver_are_bounded() {
        let (mut reader, mut writer) = tokio::io::duplex(8);
        writer.write_u32_le(10).await.unwrap();
        assert_eq!(
            receive_frame::<_, serde_json::Value>(&mut reader, Duration::from_millis(20))
                .await
                .err()
                .unwrap()
                .to_string(),
            "IPC_FRAME_TIMEOUT"
        );
        assert_eq!(
            send_frame(&mut writer, &"a".repeat(100), Duration::from_millis(20))
                .await
                .err()
                .unwrap()
                .to_string(),
            "IPC_FRAME_TIMEOUT"
        );
    }
}
