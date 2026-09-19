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
