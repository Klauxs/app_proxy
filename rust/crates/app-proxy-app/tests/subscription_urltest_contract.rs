#![cfg(windows)]
//! Real core, synthetic local nodes only. No user subscription or application is opened.
use app_proxy_core::{
    model::*,
    singbox,
    subscription::{self, saved::SavedNode},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    path::Path,
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    process::{Child, Command},
};
use uuid::Uuid;

async fn header(stream: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        if bytes.len() >= 16384 {
            return Err(std::io::ErrorKind::InvalidData.into());
        }
        bytes.push(stream.read_u8().await?);
    }
    Ok(bytes)
}

async fn upstream(
    label: &'static str,
    delay: u64,
) -> (u16, Arc<AtomicBool>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let failed = Arc::new(AtomicBool::new(false));
    let stopped = failed.clone();
    let task = tokio::spawn(async move {
        let mut peers = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                accepted = listener.accept() => {
                    let (mut stream, _) = accepted.unwrap();
                    let failed = stopped.clone();
                    peers.spawn(async move {
                        let _ = tokio::time::timeout(Duration::from_secs(3), async {
                            let request = header(&mut stream).await?;
                            if failed.load(Ordering::SeqCst) { return Ok::<_, std::io::Error>(()); }
                            assert!(request.starts_with(b"CONNECT "));
                            stream.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
                            let request = header(&mut stream).await?;
                            tokio::time::sleep(Duration::from_millis(delay)).await;
                            let probe = String::from_utf8_lossy(&request).contains("/generate_204");
                            let response = if probe { "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".into() }
                                else { format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{label}", label.len()) };
                            stream.write_all(response.as_bytes()).await
                        }).await;
                    });
                }
                _ = peers.join_next(), if !peers.is_empty() => {}
            }
        }
    });
    (port, failed, task)
}

fn start(binary: &Path, path: &Path, config: &Value) -> Child {
    std::fs::write(path, serde_json::to_vec(config).unwrap()).unwrap();
    Command::new(binary)
        .args(["run", "-c"])
        .arg(path)
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

async fn wait_ready(child: &mut Child, port: u16) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(child.try_wait().unwrap().is_none(), "fixture core exited");
            if TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_route(client: &reqwest::Client, expected: &str) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if let Ok(response) = client.get("http://local-fixture.invalid/data").send().await
                && response.text().await.ok().as_deref() == Some(expected)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("urltest did not select the available node");
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; runs only local synthetic nodes"]
async fn compiled_urltest_prefers_fast_node_and_switches_after_failure() {
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let version = Command::new(&binary)
        .arg("version")
        .creation_flags(0x08000000)
        .output()
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    let (a, fail_a, task_a) = upstream("A", 5).await;
    let (b, fail_b, task_b) = upstream("B", 200).await;
    let reserve_a = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve_b = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve_entry = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (ss_a, ss_b, entry) = (
        reserve_a.local_addr().unwrap().port(),
        reserve_b.local_addr().unwrap().port(),
        reserve_entry.local_addr().unwrap().port(),
    );
    let temp = tempfile::tempdir().unwrap();
    let upstream_config = json!({"log":{"disabled":true}, "inbounds":[
        {"type":"shadowsocks","tag":"a","listen":"127.0.0.1","listen_port":ss_a,"method":"aes-128-gcm","password":"synthetic"},
        {"type":"shadowsocks","tag":"b","listen":"127.0.0.1","listen_port":ss_b,"method":"aes-128-gcm","password":"synthetic"}],
        "outbounds":[{"type":"http","tag":"a","server":"127.0.0.1","server_port":a},
                     {"type":"http","tag":"b","server":"127.0.0.1","server_port":b}],
        "route":{"rules":[{"inbound":["a"],"action":"route","outbound":"a"},{"inbound":["b"],"action":"route","outbound":"b"},{"action":"reject"}]}});
    let parsed = subscription::parse(&format!("ss://aes-128-gcm:synthetic@127.0.0.1:{ss_a}#HK-A\nss://aes-128-gcm:synthetic@127.0.0.1:{ss_b}#JP-B")).unwrap();
    let mut secrets = HashMap::new();
    let nodes: Vec<_> = parsed
        .nodes
        .iter()
        .map(|node| {
            let (saved, secret) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node).unwrap();
            secrets.insert(saved.secret_id, secret);
            saved
        })
        .collect();
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    manifest.profiles[0].endpoint.port = entry;
    manifest.profiles[0].selected_node_id = nodes[0].id;
    manifest.profiles[0].source = ProxySource::Subscription {
        url_secret_id: Uuid::new_v4(),
        revision: 1,
        auto_test_node_ids: nodes.iter().map(|n| n.id).collect(),
        nodes,
    };
    let compiled = singbox::compile(&manifest, &[manifest.profiles[0].id], |id| {
        Ok(secrets[&id].clone())
    })
    .unwrap();
    let mut config: Value = serde_json::from_slice(compiled.bytes()).unwrap();
    let path = temp.path().join("compiled.json");
    std::fs::write(&path, compiled.bytes()).unwrap();
    let check = Command::new(&binary)
        .args(["check", "-c"])
        .arg(&path)
        .creation_flags(0x08000000)
        .output()
        .await
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stderr)
    );
    // Local HTTP avoids certificates/network access in this fixture. Validate the
    // unchanged production HTTPS config above, then accelerate the test interval.
    let group = config["outbounds"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|o| o["type"] == "urltest")
        .unwrap();
    assert_eq!(group["interval"], "3m");
    group["interval"] = json!("1s");
    group["url"] = json!("http://local-fixture.invalid/generate_204");
    drop((reserve_a, reserve_b));
    let mut peer = start(
        &binary,
        &temp.path().join("upstream.json"),
        &upstream_config,
    );
    wait_ready(&mut peer, ss_a).await;
    drop(reserve_entry);
    let mut core = start(&binary, &path, &config);
    wait_ready(&mut core, entry).await;
    let client = reqwest::Client::builder()
        .no_proxy()
        .proxy(
            reqwest::Proxy::all(format!("http://127.0.0.1:{entry}"))
                .unwrap()
                .no_proxy(None),
        )
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap();
    wait_route(&client, "A").await;
    fail_a.store(true, Ordering::SeqCst);
    wait_route(&client, "B").await;
    fail_b.store(true, Ordering::SeqCst);
    let unavailable = client.get("http://local-fixture.invalid/data").send().await;
    assert!(unavailable.is_err() || !unavailable.unwrap().status().is_success());
    core.kill().await.unwrap();
    peer.kill().await.unwrap();
    task_a.abort();
    task_b.abort();
    let _ = tokio::join!(task_a, task_b);
}
