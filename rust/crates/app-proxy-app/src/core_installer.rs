//! Download only after explicit installation intent. Network, extraction and
//! hashes run outside the configuration gate. No application is ever launched.
use crate::configuration::Configuration;
use app_proxy_core::core_control::{CoreOutcome, InstallPhase, InstallProgress};
use app_proxy_core::error_code::{self, Certainty};
use app_proxy_windows::{Error, Result, singbox_install};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

pub(crate) struct CoreInstaller {
    configuration: Arc<Configuration>,
    generation: AtomicU64,
    attempt: Mutex<Option<std::result::Result<(), InstallFailure>>>,
    progress: std::sync::Mutex<InstallProgress>,
    #[cfg(test)]
    pub(crate) test_gate: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
}

/// Safe diagnostics only: never retain OS message text, command lines or URLs.
#[derive(Clone, Debug)]
pub(crate) struct InstallFailure {
    code: &'static str,
    phase: InstallPhase,
    operation: &'static str,
    win32: Option<u32>,
    io_kind: Option<std::io::ErrorKind>,
    downloaded: usize,
    elapsed_ms: u128,
}

impl InstallFailure {
    fn capture(error: Error, progress: InstallProgress, elapsed: Duration) -> Self {
        let (code, operation, win32, io_kind) = match error {
            Error::Invalid(code) => (code, "validation", None, None),
            Error::Windows { operation, code } => (
                "CORE_INSTALL_IO_FAILED",
                operation,
                Some(code),
                Some(std::io::Error::from_raw_os_error(code as i32).kind()),
            ),
            Error::Io(error) => (
                "CORE_INSTALL_IO_FAILED",
                "filesystem",
                error.raw_os_error().map(|c| c as u32),
                Some(error.kind()),
            ),
            _ => ("CORE_INSTALL_IO_FAILED", "internal", None, None),
        };
        Self {
            code,
            phase: progress.phase,
            operation,
            win32,
            io_kind,
            downloaded: progress.downloaded,
            elapsed_ms: elapsed.as_millis(),
        }
    }

    pub(crate) fn outcome(self) -> CoreOutcome {
        let phase = match self.phase {
            InstallPhase::CheckingExisting => "checking_existing",
            InstallPhase::Downloading => "downloading",
            InstallPhase::Verifying => "verifying",
            InstallPhase::CheckingBinary => "checking_binary",
            InstallPhase::Publishing => "publishing",
        };
        let code = format!(
            "{}; phase={phase}; operation={}; win32={}; io_kind={}; downloaded_bytes={}; elapsed_ms={}",
            self.code,
            self.operation,
            self.win32
                .map(|c| c.to_string())
                .unwrap_or_else(|| "none".into()),
            self.io_kind
                .map(|kind| format!("{kind:?}"))
                .unwrap_or_else(|| "none".into()),
            self.downloaded,
            self.elapsed_ms
        );
        match error_code::certainty(self.code) {
            Certainty::Indeterminate => CoreOutcome::Indeterminate { code },
            Certainty::Definite => CoreOutcome::Failed { code },
        }
    }
}

impl CoreInstaller {
    pub fn new(configuration: Arc<Configuration>) -> Self {
        Self {
            configuration,
            generation: AtomicU64::new(0),
            attempt: Mutex::new(None),
            progress: std::sync::Mutex::new(InstallProgress {
                phase: InstallPhase::CheckingExisting,
                downloaded: 0,
                total: singbox_install::ARCHIVE_SIZE,
            }),
            #[cfg(test)]
            test_gate: std::sync::Mutex::new(None),
        }
    }

    pub async fn install(&self) -> std::result::Result<(), InstallFailure> {
        self.run_attempt(self.perform()).await
    }

    async fn run_attempt(
        &self,
        operation: impl std::future::Future<Output = Result<()>>,
    ) -> std::result::Result<(), InstallFailure> {
        let observed = self.generation.load(Ordering::SeqCst);
        let mut last = self.attempt.lock().await;
        // Concurrent callers share the attempt that ran while they waited,
        // including failures. A later explicit retry starts a new attempt.
        if observed != self.generation.load(Ordering::SeqCst) {
            return last
                .as_ref()
                .expect("completed installation attempt")
                .clone();
        }
        let started = Instant::now();
        let result = operation
            .await
            .map_err(|error| InstallFailure::capture(error, self.progress(), started.elapsed()));
        *last = Some(result.clone());
        self.generation.fetch_add(1, Ordering::SeqCst);
        result
    }

    async fn perform(&self) -> Result<()> {
        #[cfg(test)]
        {
            let gate = self.test_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.notified().await;
                return Err(Error::Invalid("CORE_DOWNLOAD_FAILED"));
            }
        }
        self.phase(InstallPhase::CheckingExisting);
        let installer = self.configuration.lock()?.core_installation()?;
        if installer.core_is_installed()? {
            return Ok(());
        }
        self.phase(InstallPhase::Downloading);
        let bytes = download(&self.progress).await?;
        self.phase(InstallPhase::Verifying);
        let mut stage = installer.prepare_core_install(&bytes)?;
        drop(bytes);
        self.phase(InstallPhase::CheckingBinary);
        stage.validate().await?;
        self.phase(InstallPhase::Publishing);
        installer.publish_core_install(stage)
    }

    pub fn progress(&self) -> InstallProgress {
        self.progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    fn phase(&self, phase: InstallPhase) {
        let mut progress = self.progress.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(
            phase,
            InstallPhase::CheckingExisting | InstallPhase::Downloading
        ) {
            progress.downloaded = 0;
        }
        progress.phase = phase;
    }
}

async fn download(progress: &std::sync::Mutex<InstallProgress>) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            let url = attempt.url();
            if attempt.previous().len() > 3
                || url.scheme() != "https"
                || !matches!(
                    url.host_str(),
                    Some(
                        "github.com"
                            | "release-assets.githubusercontent.com"
                            | "objects.githubusercontent.com"
                    )
                )
                || !url.username().is_empty()
                || url.password().is_some()
            {
                attempt.error("CORE_DOWNLOAD_REDIRECT_REJECTED")
            } else {
                attempt.follow()
            }
        }))
        .retry(reqwest::retry::never())
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(20))
        .timeout(Duration::from_secs(600))
        .user_agent(concat!("AppProxy/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| Error::Invalid("CORE_DOWNLOAD_CLIENT_FAILED"))?;
    let response = client
        .get(singbox_install::URL)
        .send()
        .await
        .map_err(download_error)?;
    collect(response, singbox_install::ARCHIVE_SIZE, |count| {
        progress
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .downloaded = count;
    })
    .await
}

async fn collect(
    mut response: reqwest::Response,
    size: usize,
    mut progress: impl FnMut(usize),
) -> Result<Vec<u8>> {
    if response.status() != reqwest::StatusCode::OK {
        return Err(Error::Invalid("CORE_DOWNLOAD_HTTP_FAILED"));
    }
    if response
        .content_length()
        .is_some_and(|length| length != size as u64)
    {
        return Err(Error::Invalid("CORE_DOWNLOAD_SIZE_MISMATCH"));
    }
    let mut bytes = Vec::with_capacity(size);
    while let Some(chunk) = response.chunk().await.map_err(download_error)? {
        if chunk.len() > size - bytes.len() {
            return Err(Error::Invalid("CORE_DOWNLOAD_SIZE_MISMATCH"));
        }
        bytes.extend_from_slice(&chunk);
        progress(bytes.len());
    }
    if bytes.len() != size {
        return Err(Error::Invalid("CORE_DOWNLOAD_SIZE_MISMATCH"));
    }
    Ok(bytes)
}

fn download_error(error: reqwest::Error) -> Error {
    Error::Invalid(if error.is_timeout() {
        "CORE_DOWNLOAD_TIMEOUT"
    } else {
        "CORE_DOWNLOAD_FAILED"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_windows::store::Store;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn install_failure_diagnostics_survive_store_reopen_without_error_message_leaks() {
        use app_proxy_core::core_control::CoreAction;
        use app_proxy_windows::core_requests::CoreRequestPhase;
        let progress = InstallProgress {
            phase: InstallPhase::Publishing,
            downloaded: singbox_install::ARCHIVE_SIZE,
            total: singbox_install::ARCHIVE_SIZE,
        };
        let outcome = InstallFailure::capture(
            Error::Windows {
                operation: "CoreInstallRenameDirectory",
                code: 32,
            },
            progress.clone(),
            Duration::from_millis(1234),
        )
        .outcome();
        let CoreOutcome::Failed { code } = &outcome else {
            panic!("expected failure")
        };
        for detail in [
            "CORE_INSTALL_IO_FAILED;",
            "phase=publishing",
            "operation=CoreInstallRenameDirectory",
            "win32=32",
            "downloaded_bytes=32841719",
            "elapsed_ms=1234",
        ] {
            assert!(code.contains(detail), "{code}");
        }
        let expected = code.clone();
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("store");
        let mut store = Store::create(&root).unwrap();
        let id = uuid::Uuid::new_v4();
        let epoch = uuid::Uuid::new_v4();
        store
            .begin_core_request(id, epoch, &CoreAction::Install {})
            .unwrap();
        store.finish_core_request(id, epoch, outcome).unwrap();
        drop(store);
        let store = Store::open(&root).unwrap();
        assert!(
            matches!(store.core_request_status(id).unwrap(), Some(CoreRequestPhase::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == expected)
        );

        let failure = InstallFailure::capture(
            Error::Io(std::io::Error::other("secret-token-private-path")),
            progress.clone(),
            Duration::ZERO,
        );
        assert!(!format!("{failure:?}").contains("secret-token"));
        let CoreOutcome::Failed { code } = failure.outcome() else {
            panic!("expected failure")
        };
        assert!(!code.contains("secret-token"));
        assert!(matches!(
            InstallFailure::capture(
                Error::Invalid("CORE_INSTALL_PUBLICATION_UNCONFIRMED"),
                progress,
                Duration::ZERO
            )
            .outcome(),
            CoreOutcome::Indeterminate { .. }
        ));
    }

    #[tokio::test]
    async fn download_body_requires_success_exact_length_and_bounded_streams() {
        for (wire, expected) in [
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                true,
            ),
            (
                "HTTP/1.1 403 Forbidden\r\nContent-Length: 3\r\nConnection: close\r\n\r\nabc",
                false,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nabcd",
                false,
            ),
            (
                "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nabcd\r\n0\r\n\r\n",
                false,
            ),
            (
                "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\na",
                false,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 1024];
                let _ = socket.read(&mut request).await.unwrap();
                socket.write_all(wire.as_bytes()).await.unwrap();
            });
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(1))
                .build()
                .unwrap();
            let response = client
                .get(format!("http://{address}/?secret=never-print-me"))
                .send()
                .await
                .unwrap();
            let result = collect(response, 3, |_| {}).await;
            assert_eq!(result.is_ok(), expected);
            if let Err(error) = result {
                assert!(!error.to_string().contains("never-print-me"));
            }
            server.await.unwrap();
        }
    }

    #[tokio::test]
    async fn concurrent_install_attempts_share_failure_and_later_retry_is_explicit() {
        let temp = tempfile::tempdir().unwrap();
        let configuration = Arc::new(Configuration::new(
            Store::create(&temp.path().join("store")).unwrap(),
        ));
        let installer = Arc::new(CoreInstaller::new(configuration));
        let calls = Arc::new(AtomicU64::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let installer = installer.clone();
            let calls = calls.clone();
            let release = release.clone();
            tasks.spawn(async move {
                installer
                    .run_attempt(async {
                        calls.fetch_add(1, Ordering::SeqCst);
                        release.notified().await;
                        Err(Error::Invalid("CORE_DOWNLOAD_FAILED"))
                    })
                    .await
            });
        }
        tokio::task::yield_now().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        release.notify_one();
        while let Some(result) = tasks.join_next().await {
            assert!(matches!(
                result.unwrap(),
                Err(failure) if failure.code == "CORE_DOWNLOAD_FAILED"
            ));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        installer
            .run_attempt(async {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
