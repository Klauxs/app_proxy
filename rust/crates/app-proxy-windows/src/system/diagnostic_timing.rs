//! Opt-in ordinary-user timing. No command lines, environment contents or app
//! data are recorded. A bounded background writer never blocks process control.
use serde::Serialize;
use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    sync::{
        OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, SyncSender},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static SINK: OnceLock<Option<SyncSender<Record>>> = OnceLock::new();
static DROPPED: AtomicU64 = AtomicU64::new(0);

#[derive(Serialize)]
struct Record {
    host_pid: u32,
    stage: &'static str,
    key: String,
    start_filetime: u64,
    end_filetime: u64,
    elapsed_us: u64,
    dropped_total: u64,
}

fn timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| (d.as_nanos() / 100) as u64 + 116_444_736_000_000_000)
        .unwrap_or(0)
}

fn sink() -> Option<&'static SyncSender<Record>> {
    SINK.get_or_init(|| {
        let path = std::env::var_os("APP_PROXY_TIMING_FILE")?;
        // Never let an elevated listener write to a caller-selected file.
        crate::identity::assert_ordinary_user().ok()?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let initial = file.metadata().ok()?.len();
        if initial >= 32 * 1024 * 1024 {
            return None;
        }
        let (send, receive) = mpsc::sync_channel::<Record>(2048);
        std::thread::Builder::new()
            .name("diagnostic-timing".into())
            .spawn(move || {
                let mut writer = BufWriter::new(file);
                let mut size = initial;
                loop {
                    match receive.recv_timeout(Duration::from_millis(50)) {
                        Ok(record) => {
                            let Ok(mut line) = serde_json::to_vec(&record) else {
                                break;
                            };
                            line.push(b'\n');
                            size += line.len() as u64;
                            if size > 32 * 1024 * 1024 || writer.write_all(&line).is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if writer.flush().is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                let _ = writer.flush();
            })
            .ok()?;
        Some(send)
    })
    .as_ref()
}

pub struct Span(Option<Active>);
struct Active {
    sender: &'static SyncSender<Record>,
    stage: &'static str,
    key: String,
    wall: u64,
    start: Instant,
}
impl Span {
    pub fn new(stage: &'static str, key: impl FnOnce() -> String) -> Self {
        Self(sink().map(|sender| Active {
            sender,
            stage,
            key: key(),
            wall: timestamp(),
            start: Instant::now(),
        }))
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if let Some(active) = self.0.take() {
            let record = Record {
                host_pid: std::process::id(),
                stage: active.stage,
                key: active.key,
                start_filetime: active.wall,
                end_filetime: timestamp(),
                elapsed_us: active.start.elapsed().as_micros() as u64,
                dropped_total: DROPPED.load(Ordering::Relaxed),
            };
            if active.sender.try_send(record).is_err() {
                DROPPED.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

pub fn mark(stage: &'static str, key: impl FnOnce() -> String) {
    drop(Span::new(stage, key));
}
