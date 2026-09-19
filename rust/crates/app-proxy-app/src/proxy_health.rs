//! HTTPS evidence through an explicit profile endpoint. This does not prove
//! listener ownership or application traffic; callers establish those separately.
use app_proxy_core::model::Endpoint;
use reqwest::{Client, ClientBuilder, Proxy, Url};
use serde::Serialize;
use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
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
    request(client, target, expected_statuses).await
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
        .connect_timeout(Duration::from_secs(3))
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
    }
}
fn transport(stage: Stage, error: reqwest::Error) -> Error {
    failure(
        stage,
        if error.is_timeout() {
            Failure::Timeout
        } else {
            Failure::Transport
        },
    )
}

#[cfg(test)]
mod tests;
