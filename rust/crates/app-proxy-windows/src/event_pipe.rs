//! One-way privileged process hints. This endpoint has no command receiver.
//! Deployment must supply verified, pinned helper/coordinator image identities;
//! a pipe connection alone does not prove installation or authorize Guard actions.
use crate::{Error, Result, etw::EventBatch, identity, ipc, last_error, wide};
use app_proxy_core::{FileIdentity, ProcessIdentity};
use serde::{Deserialize, Serialize};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr,
    time::Duration,
};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Foundation::{HANDLE, LocalFree},
    Security::{Authorization::*, *},
    Storage::FileSystem::SECURITY_IDENTIFICATION,
    System::{Pipes::*, Threading::*},
};

const VERSION: u32 = 2;
const LOGON_GROUP: u32 = windows_sys::Win32::System::SystemServices::SE_GROUP_LOGON_ID as u32;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    version: u32,
    store_id: Uuid,
    batch: EventBatch,
}

struct Policy {
    owner_sid: String,
    session_id: u32,
    logon_sid: String,
    allowed_images: Vec<FileIdentity>,
    elevated_peer: bool,
}
impl Policy {
    fn current(images: Vec<FileIdentity>, elevated_self: bool) -> Result<Self> {
        if images.is_empty() {
            return Err(Error::Invalid("IPC_NO_ALLOWED_PEER"));
        }
        let current = identity::current()?;
        // SAFETY: the current process pseudo handle is valid and not closed.
        let logon_sid = unsafe { token_context(GetCurrentProcess(), elevated_self)? };
        Ok(Self {
            owner_sid: current.user_sid,
            session_id: current.session_id,
            logon_sid,
            allowed_images: images,
            elevated_peer: !elevated_self,
        })
    }
}

fn address(store: Uuid, policy: &Policy) -> Result<String> {
    // Reuse validation of UUID and SID without sharing the ordinary RPC name.
    ipc::address(store, &policy.owner_sid)?;
    Ok(format!(
        r"\\.\pipe\app-proxy-rust-events-{}-{}-{}",
        policy.owner_sid, store, policy.session_id
    ))
}

/// Only the elevated listener can create this outbound endpoint. There is no
/// receive method, command envelope, impersonation, or arbitrary privileged RPC.
pub struct EventListener {
    name: String,
    store_id: Uuid,
    policy: Policy,
    next: NamedPipeServer,
}
impl EventListener {
    pub fn bind(store: Uuid, coordinator_images: Vec<FileIdentity>) -> Result<Self> {
        Self::bind_policy(store, Policy::current(coordinator_images, true)?)
    }
    fn bind_policy(store: Uuid, policy: Policy) -> Result<Self> {
        let name = address(store, &policy)?;
        let next = server(&name, &policy.logon_sid, true)?;
        Ok(Self {
            name,
            store_id: store,
            policy,
            next,
        })
    }
    pub async fn accept(&mut self) -> std::result::Result<EventSender, ipc::AcceptError> {
        self.next
            .connect()
            .await
            .map_err(|e| ipc::AcceptError::Listener(e.into()))?;
        let next = server(&self.name, &self.policy.logon_sid, false)
            .map_err(ipc::AcceptError::Listener)?;
        let stream = std::mem::replace(&mut self.next, next);
        let (peer, process) = authenticate(stream.as_raw_handle(), &self.policy, true)
            .map_err(ipc::AcceptError::Peer)?;
        Ok(EventSender {
            stream,
            store_id: self.store_id,
            peer,
            _process: process,
            cursor: Cursor::default(),
            poisoned: false,
        })
    }
}

pub struct EventSender {
    stream: NamedPipeServer,
    store_id: Uuid,
    pub peer: ProcessIdentity,
    _process: OwnedHandle,
    cursor: Cursor,
    poisoned: bool,
}
impl EventSender {
    pub async fn send(&mut self, mut batch: EventBatch) -> Result<()> {
        if self.poisoned {
            return Err(Error::Invalid("IPC_CONNECTION_CLOSED"));
        }
        self.poisoned = true;
        self.cursor.observe(&mut batch)?;
        ipc::send_frame(
            &mut self.stream,
            &Frame {
                version: VERSION,
                store_id: self.store_id,
                batch,
            },
            ipc::FRAME_TIMEOUT,
        )
        .await?;
        self.poisoned = false;
        Ok(())
    }
}

/// Ordinary coordinator's read-only connection. Errors, cancellation or missed
/// heartbeats close the stream; the caller must mark coverage degraded and scan.
pub struct EventReceiver {
    stream: NamedPipeClient,
    store_id: Uuid,
    pub peer: ProcessIdentity,
    _process: OwnedHandle,
    cursor: Cursor,
    poisoned: bool,
}
impl EventReceiver {
    /// `helper_images` must come from verified protected deployment, never from
    /// the pipe peer's claimed path or ordinary manifest registration metadata.
    pub async fn connect(
        store: Uuid,
        helper_images: Vec<FileIdentity>,
        timeout: Duration,
    ) -> Result<Self> {
        Self::connect_policy(store, Policy::current(helper_images, false)?, timeout).await
    }
    async fn connect_policy(store: Uuid, policy: Policy, timeout: Duration) -> Result<Self> {
        let name = address(store, &policy)?;
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match ClientOptions::new()
                .write(false)
                .security_qos_flags(SECURITY_IDENTIFICATION)
                .open(&name)
            {
                Ok(stream) => {
                    let (peer, process) = authenticate(stream.as_raw_handle(), &policy, false)?;
                    return Ok(Self {
                        stream,
                        store_id: store,
                        peer,
                        _process: process,
                        cursor: Cursor::default(),
                        poisoned: false,
                    });
                }
                Err(error) if matches!(error.raw_os_error(), Some(2 | 231)) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(Error::Invalid(
                            app_proxy_core::error_code::IPC_CONNECT_TIMEOUT,
                        ));
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    pub async fn receive(&mut self) -> Result<EventBatch> {
        if self.poisoned {
            return Err(Error::Invalid("IPC_CONNECTION_CLOSED"));
        }
        self.poisoned = true;
        let frame: Frame = ipc::receive_frame(&mut self.stream, ipc::FRAME_TIMEOUT).await?;
        if frame.version != VERSION || frame.store_id != self.store_id {
            return Err(Error::Invalid("EVENT_PROTOCOL_MISMATCH"));
        }
        let mut batch = frame.batch;
        self.cursor.observe(&mut batch)?;
        self.poisoned = false;
        Ok(batch)
    }
}

#[derive(Default)]
struct Cursor {
    previous: Option<(Uuid, u64, u64, u64, u32, u32)>,
    ended: bool,
}
impl Cursor {
    fn observe(&mut self, batch: &mut EventBatch) -> Result<()> {
        if self.ended
            || batch.epoch.is_nil()
            || batch.sequence == 0
            || batch.hints.len() > crate::etw::BATCH_LIMIT
            || batch.hints.iter().any(|hint| {
                hint.pid == 0
                    || hint.event_time <= 0
                    || hint.creation_time == 0
                    || hint.image_name.is_empty()
                    || hint.image_name.chars().count() > 260
                    || hint
                        .image_name
                        .chars()
                        .any(|c| c.is_control() || c == '/' || c == '\\')
            })
        {
            return Err(Error::Invalid("INVALID_EVENT_BATCH"));
        }
        let counters = (
            batch.dropped,
            batch.decode_failures,
            batch.etw_events_lost,
            batch.etw_buffers_lost,
        );
        if let Some((epoch, sequence, dropped, decode, events, buffers)) = self.previous {
            if epoch != batch.epoch || batch.sequence <= sequence {
                return Err(Error::Invalid("EVENT_SEQUENCE_MISMATCH"));
            }
            batch.full_scan_required |= sequence.checked_add(1) != Some(batch.sequence)
                || counters != (dropped, decode, events, buffers);
        } else {
            // Every connection starts with an unknown coverage interval, even
            // when reconnecting to the same epoch and consecutive sequence.
            batch.full_scan_required = true;
        }
        batch.full_scan_required |= batch.ended.is_some();
        self.previous = Some((
            batch.epoch,
            batch.sequence,
            counters.0,
            counters.1,
            counters.2,
            counters.3,
        ));
        self.ended = batch.ended.is_some();
        Ok(())
    }
}

fn server(name: &str, logon_sid: &str, first: bool) -> Result<NamedPipeServer> {
    // Read-only logon clients receive no FILE_CREATE_PIPE_INSTANCE bit (part of
    // generic write). Admin/system alone can create subsequent server instances.
    let sddl = wide(std::ffi::OsStr::new(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GR;;;{logon_sid})"
    )))?;
    // SAFETY: the descriptor is retained through pipe creation and freed once.
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(last_error("CreateEventPipeAcl"));
        }
        let mut attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let result = ServerOptions::new()
            .access_inbound(false)
            .first_pipe_instance(first)
            .reject_remote_clients(true)
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
    policy: &Policy,
    server_side: bool,
) -> Result<(ProcessIdentity, OwnedHandle)> {
    // SAFETY: connected pipe is retained; all process queries use one owned handle.
    unsafe {
        let mut pid = 0;
        let result = if server_side {
            GetNamedPipeClientProcessId(pipe, &mut pid)
        } else {
            GetNamedPipeServerProcessId(pipe, &mut pid)
        };
        if result == 0 {
            return Err(last_error("GetEventPipePeer"));
        }
        let process = identity::open(pid, PROCESS_QUERY_LIMITED_INFORMATION)?;
        let logon = token_context(process.as_raw_handle(), policy.elevated_peer)?;
        let peer = identity::inspect_handle(process.as_raw_handle())?;
        if peer.user_sid != policy.owner_sid {
            return Err(Error::Invalid("IPC_PEER_USER_MISMATCH"));
        }
        if peer.session_id != policy.session_id {
            return Err(Error::Invalid("STORE_SESSION_CONFLICT"));
        }
        if logon != policy.logon_sid {
            return Err(Error::Invalid("EVENT_LOGON_MISMATCH"));
        }
        if !policy.allowed_images.contains(&peer.image_file) {
            return Err(Error::Invalid("IPC_PEER_IMAGE_MISMATCH"));
        }
        Ok((peer, process))
    }
}

unsafe fn token_context(process: HANDLE, elevated: bool) -> Result<String> {
    // SAFETY: caller retains a query process handle. Token buffers are aligned,
    // bounded and owned until conversion of the single logon SID completes.
    unsafe {
        let mut token = ptr::null_mut();
        if OpenProcessToken(process, TOKEN_QUERY, &mut token) == 0 {
            return Err(last_error("OpenEventToken"));
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut elevation = TOKEN_ELEVATION::default();
        let mut length = 0;
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenElevation,
            (&mut elevation as *mut TOKEN_ELEVATION).cast(),
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut length,
        ) == 0
        {
            return Err(last_error("EventTokenElevation"));
        }
        if (elevation.TokenIsElevated != 0) != elevated {
            return Err(Error::Invalid("EVENT_ELEVATION_MISMATCH"));
        }
        if IsTokenRestricted(token.as_raw_handle()) != 0 {
            return Err(Error::Invalid("RESTRICTED_TOKEN_UNSUPPORTED"));
        }
        GetTokenInformation(
            token.as_raw_handle(),
            TokenLogonSid,
            ptr::null_mut(),
            0,
            &mut length,
        );
        if !(size_of::<TOKEN_GROUPS>() as u32..=65536).contains(&length) {
            return Err(Error::Invalid("INVALID_LOGON_SID"));
        }
        let mut buffer = vec![0u64; (length as usize).div_ceil(8)];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenLogonSid,
            buffer.as_mut_ptr().cast(),
            length,
            &mut length,
        ) == 0
        {
            return Err(last_error("EventTokenLogonSid"));
        }
        let groups = &*buffer.as_ptr().cast::<TOKEN_GROUPS>();
        if groups.GroupCount != 1 || groups.Groups[0].Attributes & LOGON_GROUP != LOGON_GROUP {
            return Err(Error::Invalid("INVALID_LOGON_SID"));
        }
        crate::security_ffi::sid_string(
            groups.Groups[0].Sid,
            "ConvertEventLogonSid",
            "INVALID_LOGON_SID",
        )
    }
}

#[cfg(test)]
mod tests;
