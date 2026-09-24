//! HTTPS evidence through an explicit profile endpoint. This does not prove
//! listener ownership or application traffic; callers establish those separately.
use app_proxy_core::model::Endpoint;
use reqwest::{Client, ClientBuilder, Proxy, Url};
use serde::Serialize;
use std::{
    future::Future,
    net::SocketAddr,
    time::{Duration, Instant},
};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const CHECK_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_RETRIES: usize = 5;
const BODY_LIMIT: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Configuration,
    ConnectAndTls,
    Response,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    InvalidInput,
    ClientSetup,
    Timeout,
    TotalTimeout,
    Transport,
    UnexpectedStatus,
    BodyTooLarge,
}

/// Intentionally never holds reqwest::Error, headers, URL or response body.
#[derive(Debug, thiserror::Error, Serialize)]
#[error("PROXY_HEALTH_{stage:?}_{kind:?}")]
pub struct Error {
    pub stage: Stage,
    pub kind: Failure,
    pub status: Option<u16>,
    #[serde(skip)]
    retryable: bool,
}

impl Error {
    /// Stable, sanitized diagnostics survive the coordinator receipt without
    /// carrying a target URL, proxy credentials or native TLS error text.
    pub(crate) fn code(&self) -> &'static str {
        match self.kind {
            Failure::InvalidInput => "CORE_PROXY_HEALTH_INVALID_INPUT",
            Failure::ClientSetup => "CORE_PROXY_HEALTH_CLIENT_SETUP_FAILED",
            Failure::TotalTimeout => "CORE_PROXY_HEALTH_DEADLINE_EXCEEDED",
            Failure::Timeout if self.stage == Stage::ConnectAndTls => "CORE_PROXY_CONNECT_TIMEOUT",
            Failure::Timeout => "CORE_PROXY_RESPONSE_TIMEOUT",
            Failure::Transport if self.retryable => "CORE_PROXY_CONNECTION_FAILED",
            Failure::Transport => "CORE_PROXY_TLS_OR_TRANSPORT_FAILED",
            Failure::UnexpectedStatus => "CORE_PROXY_UNEXPECTED_STATUS",
            Failure::BodyTooLarge => "CORE_PROXY_RESPONSE_TOO_LARGE",
        }
    }
}

pub(crate) fn failure_message(code: &str) -> Option<&'static str> {
    Some(match code {
        "CORE_PROXY_HEALTH_INVALID_INPUT" => "代理检测地址或配置无效。",
        "CORE_PROXY_HEALTH_CLIENT_SETUP_FAILED" => "无法初始化代理检测客户端。",
        "CORE_PROXY_HEALTH_DEADLINE_EXCEEDED" => "代理健康检查在 30 秒内未通过，已停止重试。",
        "CORE_PROXY_CONNECT_TIMEOUT" => "代理连接或 TLS 握手超时，重试后仍未通过。",
        "CORE_PROXY_RESPONSE_TIMEOUT" => "代理检测响应超时，重试后仍未通过。",
        "CORE_PROXY_CONNECTION_FAILED" => "代理连接失败或中断，重试后仍未通过。",
        "CORE_PROXY_TLS_OR_TRANSPORT_FAILED" => "代理连接或 TLS 验证失败。",
        "CORE_PROXY_UNEXPECTED_STATUS" => "代理检测返回了不符合要求的 HTTP 状态。",
        "CORE_PROXY_RESPONSE_TOO_LARGE" => "代理检测响应超过大小限制。",
        _ => return None,
    })
}

#[derive(Debug, Serialize)]
pub struct Evidence {
    pub status: u16,
    pub elapsed_ms: u64,
}

/// expected_statuses normally contains 200 and 204. A caller may explicitly
/// accept other 2xx responses. Redirects never count and are never followed.
pub async fn check(
    endpoint: &Endpoint,
    target: &str,
    expected_statuses: &[u16],
) -> Result<Evidence, Error> {
    let target = validate(target, expected_statuses)?;
    let client = client_builder(endpoint)?
        .build()
        .map_err(|_| failure(Stage::Configuration, Failure::ClientSetup))?;
    check_with_retries(|| request(client.clone(), target.clone(), expected_statuses)).await
}

/// Only the idempotent health GET is retried, using the same owned proxy.
/// No launch, configuration request or core lifecycle action is resubmitted.
async fn check_with_retries<F, Fut>(mut attempt: F) -> Result<Evidence, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<Evidence, Error>>,
{
    let started = tokio::time::Instant::now();
    let deadline = started + CHECK_TIMEOUT;
    for retries in 0..=MAX_RETRIES {
        if tokio::time::Instant::now() >= deadline {
            return Err(failure(Stage::ConnectAndTls, Failure::TotalTimeout));
        }
        match tokio::time::timeout_at(deadline, attempt()).await {
            Ok(Ok(mut evidence)) => {
                evidence.elapsed_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                return Ok(evidence);
            }
            Ok(Err(error)) => {
                if !error.retryable || retries == MAX_RETRIES {
                    return Err(error);
                }
                let delay = Duration::from_secs(if retries == 0 { 1 } else { 2 });
                tokio::time::sleep_until((tokio::time::Instant::now() + delay).min(deadline)).await;
            }
            Err(_) => return Err(failure(Stage::ConnectAndTls, Failure::TotalTimeout)),
        }
    }
    unreachable!("last failed attempt returns without sleeping")
}

fn validate(target: &str, statuses: &[u16]) -> Result<Url, Error> {
    let invalid = || failure(Stage::Configuration, Failure::InvalidInput);
    if target.len() > 8192
        || target.chars().any(char::is_control)
        || statuses.is_empty()
        || statuses.len() > 16
        || statuses.iter().any(|s| !(200..300).contains(s))
    {
        return Err(invalid());
    }
    let url = Url::parse(target).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}

fn client_builder(endpoint: &Endpoint) -> Result<ClientBuilder, Error> {
    if !endpoint.host.is_loopback() || endpoint.port == 0 {
        return Err(failure(Stage::Configuration, Failure::InvalidInput));
    }
    let proxy = Proxy::all(format!(
        "http://{}",
        SocketAddr::new(endpoint.host, endpoint.port)
    ))
    .map_err(|_| failure(Stage::Configuration, Failure::InvalidInput))?
    .no_proxy(None);
    Ok(Client::builder()
        .no_proxy()
        .proxy(proxy)
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .referer(false)
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .read_timeout(REQUEST_TIMEOUT)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .pool_max_idle_per_host(0))
}

async fn request(client: Client, target: Url, statuses: &[u16]) -> Result<Evidence, Error> {
    let start = Instant::now();
    let mut response = client
        .get(target)
        .send()
        .await
        .map_err(|e| transport(Stage::ConnectAndTls, e))?;
    let status = response.status().as_u16();
    if !statuses.contains(&status) {
        return Err(Error {
            stage: Stage::Response,
            kind: Failure::UnexpectedStatus,
            status: Some(status),
            retryable: false,
        });
    }
    if response
        .content_length()
        .is_some_and(|n| n > BODY_LIMIT as u64)
    {
        return Err(failure(Stage::Response, Failure::BodyTooLarge));
    }
    let mut size = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| transport(Stage::Response, e))?
    {
        size += chunk.len();
        if size > BODY_LIMIT {
            return Err(failure(Stage::Response, Failure::BodyTooLarge));
        }
    }
    Ok(Evidence {
        status,
        elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
    })
}

fn failure(stage: Stage, kind: Failure) -> Error {
    Error {
        stage,
        kind,
        status: None,
        retryable: kind == Failure::Timeout,
    }
}
fn transport(stage: Stage, error: reqwest::Error) -> Error {
    let mut result = failure(
        stage,
        if error.is_timeout() {
            Failure::Timeout
        } else {
            Failure::Transport
        },
    );
    // Unknown transport/TLS errors (including certificate failures) are not
    // assumed transient. Retry only timeouts or concrete network I/O failures.
    result.retryable |= transient_io(&error);
    result
}

fn transient_io(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut source = Some(error);
    while let Some(error) = source {
        if let Some(io) = error.downcast_ref::<std::io::Error>()
            && matches!(
                io.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
            )
        {
            return true;
        }
        source = error.source();
    }
    false
}

#[cfg(test)]
mod tests;
