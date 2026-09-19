use super::*;
use std::{
    io::Write,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer},
};

enum Reply {
    Bytes(Vec<u8>),
    Stall,
}
struct Fixture {
    url: String,
    endpoint: Endpoint,
    certificate: Option<reqwest::Certificate>,
    requests: Arc<Mutex<Vec<String>>>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    fn client(&self) -> ClientBuilder {
        let builder = builder(None).unwrap();
        match &self.certificate {
            Some(cert) => builder.tls_certs_only([cert.clone()]),
            None => builder,
        }
    }
    fn count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}
trait Stream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Stream for T {}
async fn fixture(
    tls: bool,
    reply: impl Fn(usize, &str) -> Reply + Send + Sync + 'static,
) -> Fixture {
    let (certificate, acceptor) = if tls {
        let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        let certificate = reqwest::Certificate::from_der(cert.cert.der()).unwrap();
        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.cert.der().clone()],
                PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
            )
            .unwrap();
        (Some(certificate), Some(TlsAcceptor::from(Arc::new(config))))
    } else {
        (None, None)
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let reply = Arc::new(reply);
    let task = tokio::spawn(async move {
        let mut children = JoinSet::new();
        loop {
            let (tcp, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let seen = seen.clone();
            let reply = reply.clone();
            children.spawn(async move {
                let mut stream: Box<dyn Stream> = match acceptor {
                    Some(acceptor) => match acceptor.accept(tcp).await {
                        Ok(tls) => Box::new(tls),
                        Err(_) => return,
                    },
                    None => Box::new(tcp),
                };
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    if request.len() >= 16384 {
                        return;
                    }
                    let Ok(byte) = stream.read_u8().await else {
                        return;
                    };
                    request.push(byte);
                }
                let request = String::from_utf8(request).unwrap();
                let index = {
                    let mut seen = seen.lock().unwrap();
                    seen.push(request.clone());
                    seen.len()
                };
                match reply(index, &request) {
                    Reply::Bytes(bytes) => {
                        let _ = stream.write_all(&bytes).await;
                    }
                    Reply::Stall => {
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nx")
                            .await;
                        tokio::time::sleep(Duration::from_secs(5)).await;
                    }
                }
            });
        }
    });
    Fixture {
        url: format!(
            "{}://{address}/subscription?token=fixture-secret",
            if tls { "https" } else { "http" }
        ),
        endpoint: Endpoint {
            host: address.ip(),
            port: address.port(),
        },
        certificate,
        requests,
        task,
    }
}
fn response(status: u16, extra: &str, body: &[u8]) -> Reply {
    let mut bytes = format!(
        "HTTP/1.1 {status} Fixture\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n",
        body.len()
    )
    .into_bytes();
    bytes.extend_from_slice(body);
    Reply::Bytes(bytes)
}

#[test]
fn url_redirect_and_proxy_policy_reject_secret_or_unsafe_inputs() {
    for url in [
        "file:///private",
        "ftp://example.com/list",
        "http://user:secret@example.com",
        "http://@example.com",
        "http://example.com/#token",
        "http://example.com:0/list",
        "https://example.com/\nsecret",
    ] {
        assert_eq!(validate(url).unwrap_err(), Error::InvalidUrl);
    }
    let http = validate("http://example.com/list?token=secret").unwrap();
    let https = validate("https://example.com/list?token=secret").unwrap();
    assert!(allowed_redirect(&https, std::slice::from_ref(&http)));
    assert!(!allowed_redirect(&http, std::slice::from_ref(&https)));
    assert!(allowed_redirect(
        &https,
        &vec![https.clone(); REDIRECT_LIMIT]
    ));
    assert!(!allowed_redirect(
        &https,
        &vec![https.clone(); REDIRECT_LIMIT + 1]
    ));
    assert!(
        builder(Some(&Endpoint {
            host: "192.0.2.1".parse().unwrap(),
            port: 80
        }))
        .is_err()
    );
}

#[tokio::test]
async fn fallback_changes_agents_only_until_a_body_is_downloaded() {
    let fixture = fixture(false, |index, request| {
        assert!(!request.to_ascii_lowercase().contains("referer:"));
        if index == 1 {
            response(403, "", b"do not log body")
        } else {
            response(200, "", b"syntax belongs to parser, no UA retry")
        }
    })
    .await;
    let body = download(&fixture.url, None).await.unwrap();
    assert_eq!(body.text(), "syntax belongs to parser, no UA retry");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("user-agent: Clash.Meta"));
    assert!(requests[1].contains("user-agent: Loon/3.2.0"));
}

#[tokio::test]
async fn invalid_empty_and_oversized_bodies_do_not_trigger_agent_retries() {
    for (body, expected) in [
        (vec![0xff], Error::Utf8),
        (vec![], Error::Empty),
        (vec![b'a'; BODY_LIMIT + 1], Error::TooLarge),
    ] {
        let fixture = fixture(false, move |_, _| response(200, "", &body)).await;
        assert_eq!(download(&fixture.url, None).await.err(), Some(expected));
        assert_eq!(fixture.count(), 1);
    }
    let fixture = fixture(false, |_, _| {
        response(200, "Content-Encoding: unknown\r\n", b"secret")
    })
    .await;
    let error = download(&fixture.url, None).await.err().unwrap();
    assert_eq!(error, Error::ContentEncoding);
    assert!(!format!("{error:?} {error}").contains("secret"));
    assert_eq!(fixture.count(), 1);
}

#[tokio::test]
async fn gzip_is_decoded_and_limit_applies_after_decompression() {
    for length in [128, BODY_LIMIT + 1] {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&vec![b'a'; length]).unwrap();
        let compressed = encoder.finish().unwrap();
        assert!(compressed.len() < BODY_LIMIT);
        let fixture = fixture(false, move |_, _| {
            response(200, "Content-Encoding: gzip\r\n", &compressed)
        })
        .await;
        let result = download(&fixture.url, None).await;
        if length > BODY_LIMIT {
            assert_eq!(result.err(), Some(Error::TooLarge));
        } else {
            assert_eq!(result.unwrap().text(), "a".repeat(length));
        }
        assert_eq!(fixture.count(), 1);
    }
}

#[tokio::test]
async fn repeated_stacked_and_corrupt_encodings_are_terminal() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(b"must not be accepted").unwrap();
    let compressed = encoder.finish().unwrap();
    for headers in [
        "Content-Encoding: gzip\r\nContent-Encoding: unknown\r\n",
        "Content-Encoding: gzip\r\nContent-Encoding: gzip\r\n",
        "Content-Encoding: gzip, br\r\n",
    ] {
        let body = compressed.clone();
        let server = fixture(false, move |_, _| response(200, headers, &body)).await;
        assert_eq!(
            download(&server.url, None).await.err(),
            Some(Error::ContentEncoding)
        );
        assert_eq!(server.count(), 1);
    }
    let server = fixture(false, |_, _| {
        response(200, "Content-Encoding: gzip\r\n", b"invalid gzip")
    })
    .await;
    assert_eq!(
        download(&server.url, None).await.err(),
        Some(Error::ContentEncoding)
    );
    assert_eq!(server.count(), 1);
}

#[tokio::test]
async fn interrupted_body_retries_next_agent_without_changing_route() {
    let server = fixture(false, |index, _| {
        if index == 1 {
            Reply::Bytes(
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nd".to_vec(),
            )
        } else {
            response(200, "", b"direct")
        }
    })
    .await;
    assert_eq!(download(&server.url, None).await.unwrap().text(), "direct");
    let requests = server.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].contains("user-agent: Clash.Meta"));
    assert!(requests[1].contains("user-agent: Loon/3.2.0"));
}

#[tokio::test]
async fn each_advertised_encoding_decodes_over_http() {
    use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZlibEncoder, ZstdEncoder};
    let body = b"subscription fixture".as_slice();
    let encoders: Vec<(&str, Box<dyn AsyncRead + Unpin + Send>)> = vec![
        ("gzip", Box::new(GzipEncoder::new(body))),
        ("br", Box::new(BrotliEncoder::new(body))),
        ("deflate", Box::new(ZlibEncoder::new(body))),
        ("zstd", Box::new(ZstdEncoder::new(body))),
    ];
    for (encoding, mut encoder) in encoders {
        let mut bytes = Vec::new();
        encoder.read_to_end(&mut bytes).await.unwrap();
        let headers = format!("Content-Encoding: {encoding}\r\n");
        let server = fixture(false, move |_, _| response(200, &headers, &bytes)).await;
        assert_eq!(
            download(&server.url, None).await.unwrap().text(),
            "subscription fixture"
        );
        assert_eq!(server.count(), 1);
    }
}

#[tokio::test]
async fn compressed_members_are_complete_and_trailing_garbage_is_rejected() {
    use async_compression::tokio::bufread::{BrotliEncoder, GzipEncoder, ZlibEncoder, ZstdEncoder};
    async fn encode(body: &[u8], encoding: &str) -> Vec<u8> {
        let mut encoder: Box<dyn AsyncRead + Unpin + Send + '_> = match encoding {
            "gzip" => Box::new(GzipEncoder::new(body)),
            "br" => Box::new(BrotliEncoder::new(body)),
            "deflate" => Box::new(ZlibEncoder::new(body)),
            "zstd" => Box::new(ZstdEncoder::new(body)),
            _ => unreachable!(),
        };
        let mut bytes = Vec::new();
        encoder.read_to_end(&mut bytes).await.unwrap();
        bytes
    }
    for encoding in ["gzip", "br", "deflate", "zstd"] {
        let mut body = encode(b"prefix", encoding).await;
        body.extend_from_slice(b"invalid tail");
        let headers = format!("Content-Encoding: {encoding}\r\n");
        let server = fixture(false, move |_, _| response(200, &headers, &body)).await;
        assert_eq!(
            download(&server.url, None).await.err(),
            Some(Error::ContentEncoding)
        );
        assert_eq!(server.count(), 1);
    }
    for encoding in ["gzip", "zstd"] {
        for length in [6, BODY_LIMIT] {
            let mut body = encode(b"prefix", encoding).await;
            body.extend_from_slice(&encode(&vec![b'a'; length], encoding).await);
            let headers = format!("Content-Encoding: {encoding}\r\n");
            let server = fixture(false, move |_, _| response(200, &headers, &body)).await;
            let result = download(&server.url, None).await;
            if length == BODY_LIMIT {
                assert_eq!(result.err(), Some(Error::TooLarge));
            } else {
                assert_eq!(result.unwrap().text(), "prefixaaaaaa");
            }
            assert_eq!(server.count(), 1);
        }
    }
}

#[tokio::test]
async fn redirect_loop_and_https_downgrade_stop_without_fallback() {
    let looped = fixture(false, |_, _| response(302, "Location: /again\r\n", b"")).await;
    assert_eq!(
        download(&looped.url, None).await.err(),
        Some(Error::Redirect)
    );
    assert_eq!(looped.count(), REDIRECT_LIMIT + 1);
    let target = fixture(false, |_, _| response(200, "", b"must not be reached")).await;
    let location = format!("Location: {}\r\n", target.url);
    let tls = fixture(true, move |_, _| response(302, &location, b"")).await;
    assert_eq!(
        run(
            tls.client().build().unwrap(),
            validate(&tls.url).unwrap(),
            AGENTS,
            TOTAL_TIMEOUT
        )
        .await
        .err(),
        Some(Error::Redirect)
    );
    assert_eq!(tls.count(), 1);
    assert_eq!(target.count(), 0);
}

#[tokio::test]
async fn total_budget_covers_attempts_and_body_stalls() {
    let fixture = fixture(false, |_, _| Reply::Stall).await;
    let client = fixture
        .client()
        .timeout(Duration::from_millis(150))
        .build()
        .unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        run(
            client,
            validate(&fixture.url).unwrap(),
            AGENTS,
            Duration::from_millis(250)
        )
        .await
        .err(),
        Some(Error::TotalTimeout)
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert!(fixture.count() <= 2);
}

#[tokio::test]
async fn explicit_direct_and_proxy_ignore_polluted_environment_in_child() {
    let direct = fixture(false, |_, request| {
        assert!(request.starts_with("GET /subscription?"));
        response(200, "", b"direct")
    })
    .await;
    let proxy = fixture(false, |_, request| {
        assert!(request.starts_with("GET http://subscription.invalid/list?token=secret HTTP/1.1"));
        response(200, "", b"proxy")
    })
    .await;
    let mut child = tokio::process::Command::new(std::env::current_exe().unwrap());
    child
        .kill_on_drop(true)
        .args([
            "--exact",
            "subscription_download::tests::download_child",
            "--ignored",
        ])
        .env("APP_PROXY_SUB_TEST_DIRECT", &direct.url)
        .env("APP_PROXY_SUB_TEST_PORT", proxy.endpoint.port.to_string());
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "http_proxy",
        "https_proxy",
        "all_proxy",
    ] {
        child.env(key, "http://127.0.0.1:1");
    }
    child.env("NO_PROXY", "*").env("no_proxy", "*");
    let result = tokio::time::timeout(Duration::from_secs(10), child.output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    assert_eq!(direct.count(), 1);
    assert_eq!(proxy.count(), 1);
}

#[tokio::test]
#[ignore = "spawned by explicit_direct_and_proxy_ignore_polluted_environment_in_child"]
async fn download_child() {
    let target = std::env::var("APP_PROXY_SUB_TEST_DIRECT").unwrap();
    assert_eq!(download(&target, None).await.unwrap().text(), "direct");
    let endpoint = Endpoint {
        host: "127.0.0.1".parse().unwrap(),
        port: std::env::var("APP_PROXY_SUB_TEST_PORT")
            .unwrap()
            .parse()
            .unwrap(),
    };
    assert_eq!(
        download(
            "http://subscription.invalid/list?token=secret",
            Some(&endpoint)
        )
        .await
        .unwrap()
        .text(),
        "proxy"
    );
}
