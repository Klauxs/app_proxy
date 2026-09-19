//! Secret-bearing subscription bodies stay in memory. This transport neither
//! parses nodes nor commits configuration. Its caller validates source revision
//! before publishing and verifies ownership of any supplied proxy endpoint.
use app_proxy_core::model::Endpoint;
use async_compression::tokio::bufread::{BrotliDecoder, GzipDecoder, ZlibDecoder, ZstdDecoder};
use reqwest::{Client, ClientBuilder, Proxy, Url, header};
use std::{net::SocketAddr, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt};

pub const BODY_LIMIT: usize = 8 * 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
const REDIRECT_LIMIT: usize = 5;
const AGENTS: &[&str] = &[
    "Clash.Meta",
    "Loon/3.2.0",
    "Quantumult X/1.5.0",
    "Surge/5.0",
    "Shadowrocket/2.2.0",
    "ClashforWindows/0.20.39",
    "ClashX Pro/1.118.1",
    "Mozilla/5.0",
];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("SUBSCRIPTION_URL_INVALID")]
    InvalidUrl,
    #[error("SUBSCRIPTION_PROXY_INVALID")]
    InvalidProxy,
    #[error("SUBSCRIPTION_CLIENT_FAILED")]
    Client,
    #[error("SUBSCRIPTION_TOTAL_TIMEOUT")]
    TotalTimeout,
    #[error("SUBSCRIPTION_REQUEST_TIMEOUT")]
    RequestTimeout,
    #[error("SUBSCRIPTION_TRANSPORT_FAILED")]
    Transport,
    #[error("SUBSCRIPTION_HTTP_STATUS_{0}")]
    HttpStatus(u16),
    #[error("SUBSCRIPTION_REDIRECT_REJECTED")]
    Redirect,
    #[error("SUBSCRIPTION_CONTENT_ENCODING_INVALID")]
    ContentEncoding,
    #[error("SUBSCRIPTION_BODY_TOO_LARGE")]
    TooLarge,
    #[error("SUBSCRIPTION_BODY_EMPTY")]
    Empty,
    #[error("SUBSCRIPTION_UTF8_INVALID")]
    Utf8,
}
type Result<T> = std::result::Result<T, Error>;

// No Debug/Serialize: text can include both proxy credentials and provider tokens.
pub struct Downloaded {
    text: String,
}
impl Downloaded {
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// None means explicit direct access, ignoring every environment/system proxy.
/// Some(endpoint) uses only that loopback HTTP proxy, with no direct fallback.
/// Endpoint ownership/selection comes from the managed-core caller, not a port
/// discovery here. Dropping this future cancels the in-memory request.
pub async fn download(target: &str, endpoint: Option<&Endpoint>) -> Result<Downloaded> {
    let target = validate(target)?;
    let client = builder(endpoint)?.build().map_err(|_| Error::Client)?;
    run(client, target, AGENTS, TOTAL_TIMEOUT).await
}

fn validate(value: &str) -> Result<Url> {
    if value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(Error::InvalidUrl);
    }
    // Reject even empty userinfo before URL normalization can discard it.
    if value.split_once("://").is_none_or(|(_, rest)| {
        rest.split(['/', '?', '#'])
            .next()
            .is_some_and(|s| s.contains('@'))
    }) {
        return Err(Error::InvalidUrl);
    }
    let url = Url::parse(value).map_err(|_| Error::InvalidUrl)?;
    if !allowed_url(&url) {
        return Err(Error::InvalidUrl);
    }
    Ok(url)
}
fn allowed_url(url: &Url) -> bool {
    matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.port() != Some(0)
        && url.username().is_empty()
        && url.password().is_none()
        && url.fragment().is_none()
        && url.as_str().len() <= 8192
}
fn allowed_redirect(next: &Url, previous: &[Url]) -> bool {
    previous.len() <= REDIRECT_LIMIT
        && allowed_url(next)
        && !(next.scheme() == "http" && previous.iter().any(|u| u.scheme() == "https"))
}
fn builder(endpoint: Option<&Endpoint>) -> Result<ClientBuilder> {
    let mut builder = Client::builder().no_proxy();
    if let Some(endpoint) = endpoint {
        if !endpoint.host.is_loopback() || endpoint.port == 0 {
            return Err(Error::InvalidProxy);
        }
        let proxy = Proxy::all(format!(
            "http://{}",
            SocketAddr::new(endpoint.host, endpoint.port)
        ))
        .map_err(|_| Error::InvalidProxy)?
        .no_proxy(None);
        builder = builder.proxy(proxy);
    }
    Ok(builder
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if allowed_redirect(attempt.url(), attempt.previous()) {
                attempt.follow()
            } else {
                attempt.error("SUBSCRIPTION_REDIRECT_REJECTED")
            }
        }))
        .retry(reqwest::retry::never())
        .referer(false)
        .connect_timeout(REQUEST_TIMEOUT)
        .read_timeout(REQUEST_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .pool_max_idle_per_host(0)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd())
}
async fn run(client: Client, target: Url, agents: &[&str], total: Duration) -> Result<Downloaded> {
    tokio::time::timeout(total, async {
        let mut last = Error::Transport;
        for agent in agents {
            match fetch(&client, &target, agent).await {
                Ok(body) => return Ok(body),
                Err(error @ (Error::HttpStatus(_) | Error::RequestTimeout | Error::Transport)) => {
                    last = error
                }
                Err(error) => return Err(error),
            }
        }
        Err(last)
    })
    .await
    .map_err(|_| Error::TotalTimeout)?
}
async fn fetch(client: &Client, target: &Url, agent: &str) -> Result<Downloaded> {
    let mut response = client
        .get(target.clone())
        .header(header::USER_AGENT, agent)
        .header(header::ACCEPT, "*/*")
        .header(header::ACCEPT_ENCODING, "gzip, br, deflate, zstd")
        .send()
        .await
        .map_err(transport)?;
    if !response.status().is_success() {
        return Err(Error::HttpStatus(response.status().as_u16()));
    }
    // Inspect original headers before decompression can remove duplicate values.
    let mut encodings = response.headers().get_all(header::CONTENT_ENCODING).iter();
    let encoding = match encodings.next().map(|v| v.as_bytes()) {
        None | Some(b"identity") => "identity",
        Some(b"gzip") => "gzip",
        Some(b"br") => "br",
        Some(b"deflate") => "deflate",
        Some(b"zstd") => "zstd",
        _ => return Err(Error::ContentEncoding),
    };
    if encodings.next().is_some() {
        return Err(Error::ContentEncoding);
    }
    if response
        .content_length()
        .is_some_and(|n| n > BODY_LIMIT as u64)
    {
        return Err(Error::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if chunk.len() > BODY_LIMIT - bytes.len() {
            return Err(Error::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    let bytes = decode(bytes, encoding).await?;
    if bytes.is_empty() {
        return Err(Error::Empty);
    }
    let text = String::from_utf8(bytes).map_err(|_| Error::Utf8)?;
    Ok(Downloaded { text })
}
async fn decode(bytes: Vec<u8>, encoding: &str) -> Result<Vec<u8>> {
    if encoding == "identity" {
        return Ok(bytes);
    }
    let mut input = bytes.as_slice();
    let reader: Box<dyn AsyncRead + Unpin + Send + '_> = match encoding {
        "gzip" => {
            let mut decoder = GzipDecoder::new(&mut input);
            decoder.multiple_members(true);
            Box::new(decoder)
        }
        "br" => Box::new(BrotliDecoder::new(&mut input)),
        "deflate" => Box::new(ZlibDecoder::new(&mut input)),
        "zstd" => {
            let mut decoder = ZstdDecoder::new(&mut input);
            decoder.multiple_members(true);
            Box::new(decoder)
        }
        _ => return Err(Error::ContentEncoding),
    };
    let mut decoded = Vec::new();
    reader
        .take(BODY_LIMIT as u64 + 1)
        .read_to_end(&mut decoded)
        .await
        .map_err(|_| Error::ContentEncoding)?;
    if decoded.len() > BODY_LIMIT {
        return Err(Error::TooLarge);
    }
    if !input.is_empty() {
        return Err(Error::ContentEncoding);
    }
    Ok(decoded)
}
fn transport(error: reqwest::Error) -> Error {
    if error.is_redirect() {
        Error::Redirect
    } else if error.is_timeout() {
        Error::RequestTimeout
    } else {
        // Automatic decoding is disabled. reqwest also marks interrupted body
        // reads as Decode, so only our explicit decoder reports encoding errors.
        Error::Transport
    }
}

#[cfg(test)]
mod tests;
