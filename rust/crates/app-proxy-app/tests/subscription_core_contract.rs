#![cfg(windows)]
use app_proxy_core::subscription::{Protocol, parse_uris};
use serde_json::json;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

async fn invoke(binary: &Path, args: &[&str], input: Option<Vec<u8>>) -> std::process::Output {
    let mut child = Command::new(binary)
        .args(args)
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    if let Some(input) = input {
        stdin.write_all(&input).await.unwrap();
    }
    drop(stdin);
    tokio::time::timeout(Duration::from_secs(10), child.wait_with_output())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX pointing to verified sing-box 1.14.1"]
async fn parsed_six_protocols_and_extensions_pass_real_core_check() {
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let version = invoke(&binary, &["version"], None).await;
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    const ID: &str = "12345678-1234-1234-1234-123456789abc";
    const VMESS: &str = "eyJ2IjoiMiIsInBzIjoiVk1lc3MiLCJhZGQiOiIxMjcuMC4wLjEiLCJwb3J0Ijo0NDMsImlkIjoiMTIzNDU2NzgtMTIzNC0xMjM0LTEyMzQtMTIzNDU2Nzg5YWJjIiwiYWlkIjowLCJuZXQiOiJ0Y3AiLCJ0bHMiOiJ0bHMifQ==";
    let input = [
        "anytls://test@127.0.0.1:443#AnyTLS".to_owned(),
        format!("vless://{ID}@127.0.0.1:443?security=tls#VLESS"),
        format!("vmess://{VMESS}"),
        "ss://aes-128-gcm:test@127.0.0.1:443#SS".into(),
        "trojan://test@127.0.0.1:443#Trojan".into(),
        "hy2://test@127.0.0.1:443?obfs=salamander&obfs-password=test#HY2".into(),
        format!("vless://{ID}@127.0.0.1:443?security=reality&pbk=YWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWE=&flow=xtls-rprx-vision#Reality"),
        "ss://2022-blake3-aes-128-gcm:_____________________w@127.0.0.1:443#SS2022".into(),
        "ss://2022-blake3-aes-256-gcm:__________________________________________8@127.0.0.1:443#SS2022-256".into(),
        "ss://2022-blake3-chacha20-poly1305:__________________________________________8@127.0.0.1:443#SS2022-Chacha".into(),
    ].join("\n");
    let parsed = parse_uris(&input).unwrap();
    assert_eq!(parsed.unsupported, []);
    assert_eq!(parsed.nodes.len(), 10);
    let mut nodes = parsed.nodes;
    for query in [
        "type=ws&host=front.example&path=%2Fedge",
        "type=grpc&serviceName=edge",
        "type=h2&host=front.example&path=%2Fedge",
        "type=httpupgrade&host=front.example&path=%2Fedge",
        "type=quic",
    ] {
        let parsed = parse_uris(&format!("trojan://test@127.0.0.1:443?{query}#transport")).unwrap();
        assert_eq!(parsed.unsupported, []);
        nodes.extend(parsed.nodes);
    }
    let mut ss = nodes[7].clone();
    if let Protocol::Shadowsocks { password, .. } = &mut ss.protocol {
        *password = format!("{password}:{password}");
    }
    nodes.push(ss);
    for (index, node) in nodes.iter().enumerate() {
        let outbound = node.outbound("out").unwrap();
        let config = json!({"log":{"disabled":true}, "outbounds":[outbound]});
        let result = invoke(
            &binary,
            &["check", "-c", "stdin"],
            Some(serde_json::to_vec(&config).unwrap()),
        )
        .await;
        // Every value is a synthetic fixture, never a user's subscription.
        assert!(
            result.status.success(),
            "fixture {index}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX pointing to verified sing-box 1.14.1"]
async fn clash_and_client_text_outbounds_pass_real_core_check() {
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let version = invoke(&binary, &["version"], None).await;
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    for text in [
        include_str!("../../app-proxy-core/tests/fixtures/subscription.yaml"),
        include_str!("../../app-proxy-core/tests/fixtures/subscription.txt"),
    ] {
        let parsed = app_proxy_core::subscription::parse(text).unwrap();
        assert_eq!(parsed.unsupported, []);
        assert_eq!(parsed.nodes.len(), 6);
        for (index, node) in parsed.nodes.iter().enumerate() {
            let config =
                json!({"log":{"disabled":true}, "outbounds":[node.outbound("out").unwrap()]});
            let result = invoke(
                &binary,
                &["check", "-c", "stdin"],
                Some(serde_json::to_vec(&config).unwrap()),
            )
            .await;
            assert!(
                result.status.success(),
                "synthetic fixture {index}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; starts only an owned fixture core and loopback HTTP peer"]
async fn clash_http_method_is_preserved_on_the_real_wire() {
    use tokio::{
        io::AsyncReadExt,
        net::{TcpListener, TcpStream},
    };
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let version = invoke(&binary, &["version"], None).await;
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    for (option, expected) in [
        ("", "GET"),
        (", http-opts: {method: POST, path: [/edge]}", "POST"),
    ] {
        let upstream = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_port = upstream.local_addr().unwrap().port();
        let reserved = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let entry = reserved.local_addr().unwrap();
        let text = format!(
            "proxies: [{{name: synthetic, type: vmess, server: 127.0.0.1, port: {upstream_port}, uuid: 12345678-1234-1234-1234-123456789abc, network: http{option}}}]"
        );
        let parsed = app_proxy_core::subscription::parse(&text).unwrap();
        assert_eq!(parsed.unsupported, []);
        let config = json!({"log":{"disabled":true},
            "inbounds":[{"type":"http","listen":"127.0.0.1","listen_port":entry.port()}],
            "outbounds":[parsed.nodes[0].outbound("out").unwrap()]});
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.json");
        std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
        drop(reserved);
        let mut child = Command::new(&binary)
            .arg("run")
            .arg("-c")
            .arg(&path)
            .creation_flags(0x08000000)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                assert!(
                    child.try_wait().unwrap().is_none(),
                    "fixture core exited before ready"
                );
                if TcpStream::connect(entry).await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let client = reqwest::Client::builder()
            .no_proxy()
            .proxy(
                reqwest::Proxy::all(format!("http://{entry}"))
                    .unwrap()
                    .no_proxy(None),
            )
            .timeout(Duration::from_secs(3))
            .build()
            .unwrap();
        let request = client.get("http://wire-fixture.invalid/").send();
        let observe = async {
            let (mut peer, _) = upstream.accept().await.unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 16384);
                header.push(peer.read_u8().await.unwrap());
            }
            let first = std::str::from_utf8(&header)
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .to_owned();
            assert!(first.starts_with(&format!("{expected} ")), "{first}");
        };
        tokio::time::timeout(Duration::from_secs(5), async {
            // The fixture closes after observing headers; no successful upstream response is expected.
            let (_response, ()) = tokio::join!(request, observe);
        })
        .await
        .unwrap();
        child.start_kill().unwrap();
        child.wait().await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX pointing to verified sing-box 1.14.1"]
async fn saved_subscription_profiles_compile_together_with_manual_profile_for_real_core() {
    use app_proxy_core::{
        model::*,
        singbox,
        subscription::{self, saved::SavedNode},
    };
    use std::collections::HashMap;
    use uuid::Uuid;
    let binary = std::path::PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let version = invoke(&binary, &["version"], None).await;
    assert!(version.status.success());
    assert!(String::from_utf8_lossy(&version.stdout).starts_with("sing-box version 1.14.1"));
    let mut manifest: Manifest =
        serde_json::from_str(include_str!("../../../examples/manifest.json")).unwrap();
    let mut secrets = HashMap::new();
    for (index, node) in subscription::parse(include_str!(
        "../../app-proxy-core/tests/fixtures/subscription.yaml"
    ))
    .unwrap()
    .nodes
    .iter()
    .enumerate()
    {
        let (saved, secret) = SavedNode::capture(Uuid::new_v4(), Uuid::new_v4(), node).unwrap();
        secrets.insert(saved.secret_id, secret);
        manifest.profiles.push(ProxyProfile {
            id: Uuid::new_v4(),
            name: format!("fixture-{index}"),
            revision: 1,
            kind: ProxyKind::Managed,
            endpoint: Endpoint {
                host: "127.0.0.1".parse().unwrap(),
                port: 19000 + index as u16,
            },
            selected_node_id: saved.id,
            source: ProxySource::Subscription {
                url_secret_id: Uuid::new_v4(),
                revision: 1,
                nodes: vec![saved],
                auto_test_node_ids: vec![],
            },
        });
    }
    let ids: Vec<_> = manifest.profiles.iter().map(|p| p.id).collect();
    let config = singbox::compile(&manifest, &ids, |id| Ok(secrets[&id].clone())).unwrap();
    assert_eq!(config.profiles().len(), 7);
    let result = invoke(
        &binary,
        &["check", "-c", "stdin"],
        Some(config.bytes().to_vec()),
    )
    .await;
    assert!(
        result.status.success(),
        "synthetic shared config: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}
