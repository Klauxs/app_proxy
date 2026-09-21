//! Process-start hints only. Neither an event nor a session authorizes process
//! control. Privileged deployment and the authenticated event pipe live above
//! this platform layer; this API never requests elevation or changes group ACLs.
use crate::{Error, Result, identity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    mem::{offset_of, size_of},
    ptr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use uuid::Uuid;
use windows_sys::{
    Win32::{Foundation::*, System::Diagnostics::Etw::*},
    core::GUID,
};

const PROVIDER: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
const QUEUE_LIMIT: usize = 1024;
// At most 128 * 260 Unicode scalars plus metadata, safely below the pipe's
// 1 MiB JSON budget even for four-byte characters or escaped ASCII.
pub(crate) const BATCH_LIMIT: usize = 128;
const RUNNING: u32 = u32::MAX;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessStartHint {
    pub pid: u32,
    pub creation_time: u64,
    pub image_name: String,
    pub event_time: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EventBatch {
    pub epoch: Uuid,
    pub sequence: u64,
    pub hints: Vec<ProcessStartHint>,
    pub full_scan_required: bool,
    pub dropped: u64,
    pub decode_failures: u64,
    pub etw_events_lost: u32,
    pub etw_buffers_lost: u32,
    pub ended: Option<u32>,
}

#[derive(Default)]
struct Queue {
    hints: VecDeque<ProcessStartHint>,
    sequence: u64,
    full_scan: bool,
    dropped: u64,
    decode_failures: u64,
}
struct State {
    ready: Arc<tokio::sync::Notify>,
    queue: Mutex<Queue>,
    stop: AtomicBool,
    events_lost: AtomicU32,
    buffers_lost: AtomicU32,
    consumer: Mutex<Option<PROCESSTRACE_HANDLE>>,
    ended: AtomicU32,
}
impl State {
    fn new() -> Self {
        Self {
            ready: Arc::new(tokio::sync::Notify::new()),
            queue: Mutex::new(Queue {
                full_scan: true,
                ..Queue::default()
            }),
            stop: AtomicBool::new(false),
            events_lost: AtomicU32::new(0),
            buffers_lost: AtomicU32::new(0),
            consumer: Mutex::new(None),
            ended: AtomicU32::new(RUNNING),
        }
    }
    fn close_consumer(&self) -> Result<()> {
        let mut consumer = self.consumer.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = *consumer {
            // SAFETY: the publication/cancellation lock owns this handle and
            // serializes controller and worker closure, including startup timeout.
            let result = unsafe { CloseTrace(handle) };
            if result != ERROR_CTX_CLOSE_PENDING {
                win(result, "CloseProcessTrace")?;
            }
            *consumer = None;
        }
        Ok(())
    }
    fn lost(&self, events: u32, buffers: u32) {
        let old_events = self.events_lost.swap(events, Ordering::AcqRel);
        let old_buffers = self.buffers_lost.swap(buffers, Ordering::AcqRel);
        if events != old_events || buffers != old_buffers {
            self.queue
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .full_scan = true;
        }
    }
    fn push(&self, hint: ProcessStartHint) {
        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(old) = queue
            .hints
            .iter_mut()
            .find(|h| h.pid == hint.pid && h.image_name.eq_ignore_ascii_case(&hint.image_name))
        {
            *old = hint;
        } else if queue.hints.len() == QUEUE_LIMIT {
            queue.dropped = queue.dropped.saturating_add(1);
            queue.full_scan = true;
        } else {
            queue.hints.push_back(hint);
        }
        drop(queue);
        self.ready.notify_one();
    }
    fn failed_decode(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        queue.decode_failures = queue.decode_failures.saturating_add(1);
        queue.full_scan = true;
        drop(queue);
        self.ready.notify_one();
    }
    fn drain(&self, epoch: Uuid) -> Result<EventBatch> {
        let mut queue = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        queue.sequence = queue
            .sequence
            .checked_add(1)
            .ok_or(Error::Invalid("ETW_SEQUENCE_EXHAUSTED"))?;
        let ended = self.ended.load(Ordering::Acquire);
        let count = queue.hints.len().min(BATCH_LIMIT);
        let hints = queue.hints.drain(..count).collect();
        Ok(EventBatch {
            epoch,
            sequence: queue.sequence,
            hints,
            full_scan_required: std::mem::take(&mut queue.full_scan) || ended != RUNNING,
            dropped: queue.dropped,
            decode_failures: queue.decode_failures,
            etw_events_lost: self.events_lost.load(Ordering::Acquire),
            etw_buffers_lost: self.buffers_lost.load(Ordering::Acquire),
            // Deliver queued hints before sealing the stream.
            ended: (ended != RUNNING && queue.hints.is_empty()).then_some(ended),
        })
    }
}

#[repr(C)]
struct Properties {
    value: EVENT_TRACE_PROPERTIES,
    name: [u16; 1024],
    file: [u16; 1024],
}
impl Properties {
    fn new(guid: GUID) -> Self {
        let mut result = Self {
            value: EVENT_TRACE_PROPERTIES::default(),
            name: [0; 1024],
            file: [0; 1024],
        };
        result.value.Wnode.BufferSize = size_of::<Self>() as u32;
        result.value.Wnode.Guid = guid;
        result.value.Wnode.ClientContext = 1; // QPC; ProcessTrace converts to system time.
        result.value.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        result.value.BufferSize = 64;
        result.value.MinimumBuffers = 2;
        result.value.MaximumBuffers = 4;
        result.value.FlushTimer = 1;
        result.value.LogFileMode = EVENT_TRACE_REAL_TIME_MODE
            | EVENT_TRACE_NO_PER_PROCESSOR_BUFFERING
            | EVENT_TRACE_INDEPENDENT_SESSION_MODE;
        result.value.LoggerNameOffset = offset_of!(Self, name) as u32;
        result
    }
}

struct Session {
    handle: CONTROLTRACE_HANDLE,
    guid: GUID,
    name: Vec<u16>,
    stopped: bool,
}
impl Session {
    fn query(&self) -> Result<Properties> {
        let mut properties = Properties::new(self.guid);
        properties.value.LogFileNameOffset = offset_of!(Properties, file) as u32;
        // SAFETY: buffers are aligned, sized, and include both returned strings.
        let result = unsafe {
            ControlTraceW(
                self.handle,
                ptr::null(),
                &mut properties.value,
                EVENT_TRACE_CONTROL_QUERY,
            )
        };
        win(result, "QueryOwnedTrace")?;
        if !same_guid(properties.value.Wnode.Guid, self.guid)
            || properties.name.get(..self.name.len()) != Some(self.name.as_slice())
            || properties.value.LogFileMode & EVENT_TRACE_REAL_TIME_MODE == 0
            || properties.file[0] != 0
        {
            return Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"));
        }
        Ok(properties)
    }
    fn flush(&self) -> Result<Properties> {
        // Never flush by name alone: authenticate the current session first,
        // just as stop does. A replacement session must remain untouched.
        let mut properties = self.query()?;
        // SAFETY: the owned handle was checked above and sized buffers remain
        // live. This runs on the controller, never inside an ETW callback.
        let code = unsafe {
            ControlTraceW(
                self.handle,
                ptr::null(),
                &mut properties.value,
                EVENT_TRACE_CONTROL_FLUSH,
            )
        };
        win(code, "FlushOwnedTrace")?;
        Ok(properties)
    }
    fn stop(&mut self) -> Result<()> {
        if self.stopped {
            return Ok(());
        }
        let mut properties = match self.query() {
            Ok(p) => p,
            Err(Error::Windows {
                code: ERROR_WMI_INSTANCE_NOT_FOUND,
                ..
            }) => {
                self.stopped = true;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        // Stop by the original control handle, never just by a possibly reused name.
        // SAFETY: same owned handle and valid query buffer checked above.
        win(
            // SAFETY: verified original control handle and retained output buffer.
            unsafe {
                ControlTraceW(
                    self.handle,
                    ptr::null(),
                    &mut properties.value,
                    EVENT_TRACE_CONTROL_STOP,
                )
            },
            "StopOwnedTrace",
        )?;
        self.stopped = true;
        Ok(())
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

pub struct ProcessListener {
    session: Session,
    worker: Option<thread::JoinHandle<()>>,
    state: Arc<State>,
    epoch: Uuid,
}
impl Session {
    fn name(store_id: Uuid) -> Result<Vec<u16>> {
        if store_id.is_nil() {
            return Err(Error::Invalid("INVALID_ETW_SCOPE"));
        }
        let caller = identity::current()?;
        let scope = format!("{:x}", Sha256::digest(caller.user_sid.as_bytes()));
        Ok(format!(
            "AppProxy-Process-{}-{}-{}",
            &scope[..16],
            store_id,
            caller.session_id
        )
        .encode_utf16()
        .chain(Some(0))
        .collect())
    }
    fn start(store_id: Uuid, epoch: Uuid) -> Result<Self> {
        if store_id.is_nil() || epoch.is_nil() {
            return Err(Error::Invalid("INVALID_ETW_SCOPE"));
        }
        let name = Self::name(store_id)?;
        let guid = GUID::from_u128(epoch.as_u128());
        let mut properties = Properties::new(guid);
        let mut handle = CONTROLTRACE_HANDLE::default();
        // SAFETY: output and property/string buffers remain live throughout call.
        let result = unsafe { StartTraceW(&mut handle, name.as_ptr(), &mut properties.value) };
        if result == ERROR_ALREADY_EXISTS {
            // Query for diagnostics only; a name collision never grants ownership.
            let mut existing = Properties::new(GUID::default());
            existing.value.LogFileNameOffset = offset_of!(Properties, file) as u32;
            // SAFETY: valid query buffers; zero handle selects the exact fixed name.
            unsafe {
                ControlTraceW(
                    CONTROLTRACE_HANDLE::default(),
                    name.as_ptr(),
                    &mut existing.value,
                    EVENT_TRACE_CONTROL_QUERY,
                )
            };
            return Err(Error::Invalid("ETW_SESSION_CONFLICT"));
        }
        win(result, "StartProcessTrace")?;
        Ok(Self {
            handle,
            guid,
            name,
            stopped: false,
        })
    }
}

/// Only the protected deployment journal may supply this epoch. Its exclusive
/// writer handle must outlive recovery and the next trace. A name alone never
/// authorizes recovery, and the public start API still refuses all collisions.
pub(crate) fn recover_owned(store_id: Uuid, epoch: Uuid) -> Result<()> {
    if epoch.is_nil() {
        return Err(Error::Invalid("INVALID_ETW_SCOPE"));
    }
    let name = Session::name(store_id)?;
    let mut properties = Properties::new(GUID::default());
    properties.value.LogFileNameOffset = offset_of!(Properties, file) as u32;
    // SAFETY: valid fixed-name query and sized output. This call is read-only.
    let result = unsafe {
        ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            name.as_ptr(),
            &mut properties.value,
            EVENT_TRACE_CONTROL_QUERY,
        )
    };
    if result == ERROR_WMI_INSTANCE_NOT_FOUND {
        return Ok(());
    }
    win(result, "QueryPreviousTrace")?;
    if !same_guid(
        properties.value.Wnode.Guid,
        GUID::from_u128(epoch.as_u128()),
    ) {
        return Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"));
    }
    // WNODE_HEADER documents HistoricalContext as the output session handle.
    // Retain that handle, then query and verify GUID/name/mode again before stop.
    // SAFETY: successful ControlTrace filled the HistoricalContext union member.
    let handle = unsafe { properties.value.Wnode.Anonymous1.HistoricalContext };
    if handle == 0 || handle == u64::MAX {
        return Err(Error::Invalid("ETW_INVALID_RECOVERY_HANDLE"));
    }
    let mut session = Session {
        handle: CONTROLTRACE_HANDLE { Value: handle },
        guid: GUID::from_u128(epoch.as_u128()),
        name,
        stopped: false,
    };
    session.stop()
}
impl ProcessListener {
    /// Native permission checks apply. The production caller must first verify
    /// its protected installation/authorization; no user-supplied provider/path.
    pub fn start(store_id: Uuid, epoch: Uuid) -> Result<Self> {
        let session = Session::start(store_id, epoch)?;
        let handle = session.handle;
        let guid = session.guid;
        let name = session.name.clone();
        let filter = EVENT_FILTER_EVENT_ID {
            FilterIn: true,
            Reserved: 0,
            Count: 1,
            Events: [1],
        };
        let mut descriptor = EVENT_FILTER_DESCRIPTOR {
            Ptr: (&filter as *const EVENT_FILTER_EVENT_ID) as u64,
            Size: size_of::<EVENT_FILTER_EVENT_ID>() as u32,
            Type: EVENT_FILTER_TYPE_EVENT_ID,
        };
        let parameters = ENABLE_TRACE_PARAMETERS {
            Version: ENABLE_TRACE_PARAMETERS_VERSION_2,
            SourceId: guid,
            EnableFilterDesc: &mut descriptor,
            FilterDescCount: 1,
            ..Default::default()
        };
        // SAFETY: only the fixed kernel process provider and process-start filter;
        // ETW copies the bounded filter during this synchronous enable call.
        win(
            // SAFETY: the fixed provider/filter/parameters live through EnableTraceEx2.
            unsafe {
                EnableTraceEx2(
                    handle,
                    &PROVIDER,
                    EVENT_CONTROL_CODE_ENABLE_PROVIDER,
                    TRACE_LEVEL_INFORMATION as u8,
                    0x10,
                    0,
                    1000,
                    &parameters,
                )
            },
            "EnableProcessTrace",
        )?;
        let state = Arc::new(State::new());
        let worker_state = state.clone();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("app-proxy-etw".into())
            .spawn(move || {
                let mut name = name;
                let mut logfile = EVENT_TRACE_LOGFILEW {
                    LoggerName: name.as_mut_ptr(),
                    Context: Arc::as_ptr(&worker_state).cast_mut().cast(),
                    BufferCallback: Some(buffer_callback),
                    ..Default::default()
                };
                logfile.Anonymous1.ProcessTraceMode =
                    PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
                logfile.Anonymous2.EventRecordCallback = Some(event_callback);
                // SAFETY: logfile, name, and callback state live until ProcessTrace returns.
                let consumer = unsafe { OpenTraceW(&mut logfile) };
                if consumer.Value == u64::MAX {
                    let error = std::io::Error::last_os_error().raw_os_error().unwrap_or(1) as u32;
                    worker_state.ended.store(error, Ordering::Release);
                    let _ = sender.send(Err(error));
                    return;
                }
                *worker_state
                    .consumer
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(consumer);
                if worker_state.stop.load(Ordering::Acquire) || sender.send(Ok(())).is_err() {
                    let _ = worker_state.close_consumer();
                    return;
                }
                // SAFETY: valid consumer handle. Controller may CloseTrace concurrently;
                // its callback state stays owned here until all callbacks finish.
                let result = unsafe { ProcessTrace(&consumer, 1, ptr::null(), ptr::null()) };
                worker_state.ended.store(result, Ordering::Release);
                worker_state.ready.notify_one();
                let _ = worker_state.close_consumer();
            })?;
        match receiver.recv_timeout(Duration::from_secs(3)) {
            Ok(Ok(())) => {}
            other => {
                state.stop.store(true, Ordering::Release);
                let _ = state.close_consumer();
                return Err(match other {
                    Ok(Err(code)) => Error::Windows {
                        operation: "OpenProcessTrace",
                        code,
                    },
                    _ => Error::Invalid("ETW_CONSUMER_START_TIMEOUT"),
                });
            }
        };
        Ok(Self {
            session,
            worker: Some(worker),
            state,
            epoch,
        })
    }
    pub fn drain(&self) -> Result<EventBatch> {
        if !self.session.stopped {
            // Force delivery on the listener's bounded heartbeat rather than
            // waiting up to the integer-second ETW FlushTimer.
            return self.drain_after_query(self.session.flush());
        }
        self.state.drain(self.epoch)
    }
    pub(crate) fn ready(&self) -> Arc<tokio::sync::Notify> {
        self.state.ready.clone()
    }
    /// Callback delivery already happened; consume without another native flush.
    pub(crate) fn drain_ready(&self) -> Result<EventBatch> {
        self.state.drain(self.epoch)
    }
    fn drain_after_query(&self, query: Result<Properties>) -> Result<EventBatch> {
        match query {
            Ok(properties) => self.state.lost(
                properties.value.EventsLost,
                properties.value.RealTimeBuffersLost,
            ),
            Err(Error::Windows {
                code: ERROR_WMI_INSTANCE_NOT_FOUND,
                ..
            }) => {
                // Preserve a known ProcessTrace end code; absence is itself
                // enough to require fallback even before the worker returns.
                let _ = self.state.ended.compare_exchange(
                    RUNNING,
                    ERROR_WMI_INSTANCE_NOT_FOUND,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
            }
            Err(error) => return Err(error),
        }
        self.state.drain(self.epoch)
    }
    pub fn stop(&mut self) -> Result<()> {
        self.state.stop.store(true, Ordering::Release);
        let session_result = self.session.stop();
        let close_result = self.state.close_consumer();
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.worker.as_ref().is_some_and(|w| !w.is_finished()) {
            if Instant::now() >= deadline {
                return Err(Error::Invalid("ETW_CONSUMER_STOP_TIMEOUT"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        if let Some(worker) = self.worker.take() {
            worker
                .join()
                .map_err(|_| Error::Invalid("ETW_CONSUMER_PANICKED"))?;
        }
        session_result?;
        close_result
    }
}
impl Drop for ProcessListener {
    fn drop(&mut self) {
        self.state.stop.store(true, Ordering::Release);
        let _ = self.state.close_consumer();
        // Session drop only stops our verified trace; detached worker retains state.
    }
}

fn win(code: u32, operation: &'static str) -> Result<()> {
    if code == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(Error::Windows { operation, code })
    }
}
fn same_guid(a: GUID, b: GUID) -> bool {
    a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
}

unsafe extern "system" fn event_callback(record: *mut EVENT_RECORD) {
    if record.is_null() {
        return;
    }
    // SAFETY: ETW supplies this record and the Context passed to OpenTrace.
    let record = unsafe { &*record };
    if record.UserContext.is_null() {
        return;
    }
    // SAFETY: Arc<State> is retained on the ProcessTrace thread through callbacks.
    let state = unsafe { &*record.UserContext.cast::<State>() };
    if state.stop.load(Ordering::Acquire)
        || !same_guid(record.EventHeader.ProviderId, PROVIDER)
        || record.EventHeader.EventDescriptor.Id != 1
    {
        return;
    }
    let outcome = std::panic::catch_unwind(|| {
        if record.UserData.is_null() || record.UserDataLength == 0 {
            return Err(Error::Invalid("INVALID_PROCESS_EVENT"));
        }
        state.push(decode(record)?);
        Ok(())
    });
    if !matches!(outcome, Ok(Ok(()))) {
        state.failed_decode();
    }
}
unsafe extern "system" fn buffer_callback(logfile: *mut EVENT_TRACE_LOGFILEW) -> u32 {
    if logfile.is_null() {
        return 0;
    }
    // SAFETY: ETW's live logfile has the retained Context from OpenTrace.
    let logfile = unsafe { &*logfile };
    if logfile.Context.is_null() {
        return 0;
    }
    // SAFETY: ProcessTrace's worker retains the Arc behind this original Context.
    let state = unsafe { &*logfile.Context.cast::<State>() };
    // EVENT_TRACE_LOGFILE.EventsLost is reserved, not an event-loss counter.
    // drain() samples EVENT_TRACE_PROPERTIES from the verified session instead.
    u32::from(!state.stop.load(Ordering::Acquire))
}

fn property(record: &EVENT_RECORD, name: &str, limit: u32) -> Result<Vec<u8>> {
    let name: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let descriptor = PROPERTY_DATA_DESCRIPTOR {
        PropertyName: name.as_ptr() as u64,
        ArrayIndex: u32::MAX,
        Reserved: 0,
    };
    let mut size = 0;
    // SAFETY: ETW record and descriptor/name remain live during both TDH calls.
    win(
        // SAFETY: record and descriptor/name remain live; size is valid output.
        unsafe { TdhGetPropertySize(record, 0, ptr::null(), 1, &descriptor, &mut size) },
        "ProcessEventPropertySize",
    )?;
    if size == 0 || size > limit {
        return Err(Error::Invalid("ETW_PROPERTY_SIZE"));
    }
    let mut bytes = vec![0; size as usize];
    win(
        // SAFETY: allocated byte length equals the bounded size returned by TDH.
        unsafe {
            TdhGetProperty(
                record,
                0,
                ptr::null(),
                1,
                &descriptor,
                size,
                bytes.as_mut_ptr(),
            )
        },
        "ProcessEventProperty",
    )?;
    Ok(bytes)
}
fn decode(record: &EVENT_RECORD) -> Result<ProcessStartHint> {
    let pid = property(record, "ProcessID", 4)?;
    let pid = u32::from_le_bytes(
        pid.try_into()
            .map_err(|_| Error::Invalid("ETW_PROCESS_ID_SIZE"))?,
    );
    let created = property(record, "CreateTime", 8)?;
    let created = u64::from_le_bytes(
        created
            .try_into()
            .map_err(|_| Error::Invalid("ETW_CREATION_TIME_SIZE"))?,
    );
    let image = property(record, "ImageName", 4096)?;
    hint(pid, created, record.EventHeader.TimeStamp, &image)
}
fn hint(pid: u32, creation_time: u64, event_time: i64, bytes: &[u8]) -> Result<ProcessStartHint> {
    if pid == 0
        || creation_time == 0
        || event_time <= 0
        || bytes.len() < 2
        || bytes.len() > 4096
        || !bytes.len().is_multiple_of(2)
    {
        return Err(Error::Invalid("INVALID_PROCESS_EVENT"));
    }
    let mut units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .collect();
    if units.pop() != Some(0) || units.contains(&0) {
        return Err(Error::Invalid("INVALID_EVENT_IMAGE"));
    }
    let image = String::from_utf16(&units).map_err(|_| Error::Invalid("INVALID_EVENT_IMAGE"))?;
    let name = image.rsplit(['\\', '/']).next().unwrap_or("");
    if name.is_empty() || name.chars().count() > 260 || name.chars().any(char::is_control) {
        return Err(Error::Invalid("INVALID_EVENT_IMAGE"));
    }
    Ok(ProcessStartHint {
        pid,
        creation_time,
        event_time,
        image_name: name.into(),
    })
}

#[cfg(test)]
mod tests;
