//! Download only after explicit installation intent. Network, extraction and
//! hashes run outside the configuration gate. No application is ever launched.
use crate::configuration::Configuration;
use app_proxy_core::core_control::{InstallPhase, InstallProgress};
use app_proxy_windows::{Error, Result, singbox_install};
use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Mutex;

pub(crate) struct CoreInstaller {
    configuration: Arc<Configuration>,
    generation: AtomicU64,
    attempt: Mutex<Option<std::result::Result<(), &'static str>>>,
    progress: std::sync::Mutex<InstallProgress>,
    #[cfg(test)]
    pub(crate) test_gate: std::sync::Mutex<Option<Arc<tokio::sync::Notify>>>,
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

    pub async fn install(&self) -> Result<()> {
        self.run_attempt(self.perform()).await
    }

    async fn run_attempt(
        &self,
        operation: impl std::future::Future<Output = Result<()>>,
    ) -> Result<()> {
        let observed = self.generation.load(Ordering::SeqCst);
        let mut last = self.attempt.lock().await;
        // Concurrent callers share the attempt that ran while they waited,
        // including failures. A later explicit retry starts a new attempt.
        if observed != self.generation.load(Ordering::SeqCst) {
            return last
                .expect("completed installation attempt")
                .map_err(Error::Invalid);
        }
        let result = operation.await.map_err(|error| match error {
            Error::Invalid(code) => code,
            _ => "CORE_INSTALL_IO_FAILED",
        });
        *last = Some(result);
        self.generation.fetch_add(1, Ordering::SeqCst);
        result.map_err(Error::Invalid)
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
        if matches!(phase, InstallPhase::Downloading) {
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
        .user_agent(concat!("AppProxyRust/", env!("CARGO_PKG_VERSION")))
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
                Err(Error::Invalid("CORE_DOWNLOAD_FAILED"))
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
