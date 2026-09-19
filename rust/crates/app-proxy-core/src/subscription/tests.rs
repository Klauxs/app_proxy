use super::*;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use serde_json::json;

const ID: &str = "12345678-1234-1234-1234-123456789abc";
fn uris() -> Vec<String> {
    vec![
        "anytls://test@127.0.0.1:443?sni=example.com#JP%20anytls".into(),
        format!("vless://{ID}@127.0.0.1:443?security=tls&sni=example.com#JP%20vless"),
        format!("vmess://{}", STANDARD.encode(json!({"v":"2","ps":"JP vmess","add":"127.0.0.1","port":"443","id":ID,"aid":"0","net":"tcp","tls":"tls"}).to_string())),
        format!("ss://{}@127.0.0.1:8388#US%20ss", STANDARD.encode("aes-128-gcm:password")),
        "trojan://password@127.0.0.1:443?sni=example.com#US%20trojan".into(),
        "hy2://password@127.0.0.1:443?sni=example.com#US%20hy2".into(),
    ]
}
fn node(value: &str) -> Node {
    let mut parsed = parse_uris(value).unwrap();
    assert_eq!(parsed.unsupported, []);
    assert_eq!(parsed.nodes.len(), 1);
    parsed.nodes.remove(0)
}
fn rejected(value: &str, expected: Error) {
    let parsed = parse_uris(value).unwrap();
    assert!(parsed.nodes.is_empty());
    assert_eq!(
        parsed.unsupported,
        [Issue {
            source_index: 1,
            reason: expected
        }]
    );
}

#[test]
fn six_protocols_and_base64_envelopes_preserve_nodes() {
    let text = uris().join("\n");
    let parsed = parse_uris(&text).unwrap();
    assert_eq!(parsed.nodes.len(), 6);
    assert!(parsed.unsupported.is_empty());
    for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
        let encoded = engine.encode(&text);
        let split = encoded
            .as_bytes()
            .chunks(72)
            .map(|s| std::str::from_utf8(s).unwrap())
            .collect::<Vec<_>>()
            .join("\r\n");
        assert!(parse_uris(&split).unwrap().nodes == parsed.nodes);
    }
    let outbounds: Vec<_> = parsed
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| n.outbound(&format!("out-{i}")).unwrap())
        .collect();
    for (i, kind) in [
        "anytls",
        "vless",
        "vmess",
        "shadowsocks",
        "trojan",
        "hysteria2",
    ]
    .iter()
    .enumerate()
    {
        assert_eq!(outbounds[i]["type"], *kind);
        assert_eq!(outbounds[i]["server"], "127.0.0.1");
        assert_eq!(outbounds[i]["tag"], format!("out-{i}"));
        assert!(outbounds[i].get("detour").is_none());
    }
}

#[test]
fn credential_bytes_are_decoded_once_not_after_base64_or_json() {
    let percent = node("trojan://a%252Fb%2Bc%40d@127.0.0.1:443#name+plus%20space");
    assert_eq!(percent.outbound("o").unwrap()["password"], "a%2Fb+c@d");
    assert_eq!(percent.name, "name+plus space");
    for uri in [
        format!(
            "ss://{}@127.0.0.1:8080#ss",
            URL_SAFE_NO_PAD.encode("aes-128-gcm:a%2Fb:tail")
        ),
        format!(
            "ss://{}#ss",
            STANDARD.encode("aes-128-gcm:a%2Fb:tail@127.0.0.1:8080")
        ),
        "ss://aes-128-gcm:a%252Fb%3Atail@127.0.0.1:8080#ss".into(),
    ] {
        assert_eq!(node(&uri).outbound("o").unwrap()["password"], "a%2Fb:tail");
    }
    let vmess = json!({"add":"example.com","port":443,"id":ID,"ps":"JP%20literal","net":"ws","path":"/%2F","tls":"tls"});
    let parsed = node(&format!("vmess://{}", STANDARD.encode(vmess.to_string())));
    assert_eq!(parsed.name, "JP%20literal");
    assert_eq!(parsed.outbound("o").unwrap()["transport"]["path"], "/%2F");
}

#[test]
fn malformed_escapes_ports_hosts_and_required_fields_are_not_selectable() {
    for uri in [
        "trojan://@host:443#x",
        "trojan://pass@host:0#x",
        "trojan://pass@host:65536#x",
        "trojan://pass@host:+443#x",
        "trojan://pass@host:4.43#x",
        "trojan://pass@host#x",
        "trojan://pass@bad host:443#x",
        "trojan://pass@host:443/path#x",
        "trojan://pass@2001:db8::1:443#x",
        "vless://bad@host:443#x",
        "trojan://pass@host:443#bad%00name",
    ] {
        rejected(uri, Error::InvalidNode);
    }
    for uri in [
        "trojan://pass%@host:443#x",
        "trojan://pass@host:443#bad%ff",
        "trojan://pass@host:443?sni=x%zz#x",
    ] {
        rejected(uri, Error::Encoding);
    }
    assert_eq!(node("hy2://pass@[2001:db8::1]/#x").port, 443);
    assert_eq!(
        node("trojan://pass@[2001:db8::1]:8443/#x").server,
        "2001:db8::1"
    );
    assert_eq!(
        node("trojan://pass@例子.测试:443#x").server,
        "xn--fsqu00a.xn--0zwm56d"
    );
}

#[test]
fn duplicate_query_keys_aliases_and_json_fields_are_rejected() {
    for query in [
        "sni=a&sni=b",
        "sni=a&peer=b",
        "%73ni=a&sni=b",
        "security=tls&tls=true",
        "insecure=1&allowInsecure=1",
        "type=ws&network=ws",
    ] {
        rejected(
            &format!("trojan://pass@host:443?{query}#x"),
            Error::ConflictingOptions,
        );
    }
    let repeated = format!(r#"{{"add":"host","port":443,"id":"{ID}","id":"{ID}"}}"#);
    rejected(
        &format!("vmess://{}", STANDARD.encode(repeated)),
        Error::InvalidNode,
    );
}

#[test]
fn unknown_connection_options_and_invalid_tls_combinations_are_rejected() {
    for query in [
        "unknown=secret",
        "ech=secret",
        "certificate=/secret",
        "type=kcp",
        "type=xhttp",
        "fp=unsupported",
    ] {
        rejected(
            &format!("trojan://pass@host:443?{query}#x"),
            Error::UnsupportedOption,
        );
    }
    for query in [
        "security=none",
        "tls=false",
        "security=tls&sid=01",
        "pbk=key",
    ] {
        rejected(
            &format!("trojan://pass@host:443?{query}#x"),
            Error::ConflictingOptions,
        );
    }
    rejected(
        &format!("vless://{ID}@host:443?security=none&sni=host#x"),
        Error::ConflictingOptions,
    );
    rejected("hy2://pass@host:443?fp=chrome#x", Error::UnsupportedOption);
    rejected("hy2://pass@host:443?type=ws#x", Error::UnsupportedOption);
    rejected(
        "anytls://pass@host:443?type=grpc#x",
        Error::UnsupportedOption,
    );
    rejected(
        "ss://aes-128-gcm:pass@host:443?plugin=anything#x",
        Error::UnsupportedOption,
    );
}

#[test]
fn tls_reality_and_transport_fields_map_without_downgrades() {
    let key = URL_SAFE_NO_PAD.encode([7u8; 32]);
    let reality = node(&format!(
        "vless://{ID}@host:443?security=reality&pbk={key}&sid=0123456789abcdef&sni=example.com&flow=xtls-rprx-vision#Reality"
    ));
    let value = reality.outbound("out").unwrap();
    assert_eq!(value["tls"]["reality"]["public_key"], key);
    assert_eq!(value["tls"]["utls"]["fingerprint"], "chrome");
    assert_eq!(value["tls"]["insecure"], false);
    for query in [
        format!("security=reality&pbk={key}&insecure=1"),
        format!("security=reality&pbk={key}&sid=123"),
    ] {
        rejected(
            &format!("vless://{ID}@host:443?{query}#bad"),
            Error::InvalidNode,
        );
    }
    rejected(
        &format!("vless://{ID}@host:443?security=reality&pbk=short#bad"),
        Error::Encoding,
    );
    let ws = node("trojan://pass@host:443?type=ws&host=front.example&path=%2Fedge%253F&alpn=h2%2Chttp%2F1.1&allowInsecure=true#WS").outbound("out").unwrap();
    assert_eq!(
        ws["transport"],
        json!({"type":"ws","path":"/edge%3F","headers":{"Host":"front.example"}})
    );
    assert_eq!(ws["tls"]["alpn"], json!(["h2", "http/1.1"]));
    assert_eq!(ws["tls"]["insecure"], true);
    let grpc = node("trojan://pass@host:443?type=grpc&serviceName=edge#GRPC")
        .outbound("out")
        .unwrap();
    assert_eq!(
        grpc["transport"],
        json!({"type":"grpc","service_name":"edge"})
    );
    let http = node(&format!(
        "vless://{ID}@host:443?security=tls&type=h2&host=a.example,b.example&path=%2Fedge#HTTP"
    ))
    .outbound("out")
    .unwrap();
    assert_eq!(
        http["transport"],
        json!({"type":"http","host":["a.example", "b.example"],"path":"/edge"})
    );
    let upgrade = node("trojan://pass@host:443?type=httpupgrade&host=front.example#UP")
        .outbound("out")
        .unwrap();
    assert_eq!(
        upgrade["transport"],
        json!({"type":"httpupgrade","host":"front.example","path":"/"})
    );
    let hy = node("hy2://user%3Apass@host:443?obfs=salamander&obfs-password=secret&upmbps=100&downmbps=200#HY").outbound("out").unwrap();
    assert_eq!(hy["password"], "user:pass");
    assert_eq!(hy["obfs"], json!({"type":"salamander","password":"secret"}));
    assert_eq!(hy["up_mbps"], 100);
}

#[test]
fn vmess_rejects_bad_numeric_json_unknown_fields_and_preserves_options() {
    for (key, value) in [
        ("port", json!("443x")),
        ("aid", json!("1junk")),
        ("aid", json!(-1)),
        ("aid", json!(0.5)),
        ("unknown", json!("secret")),
        ("scy", json!("unsupported")),
    ] {
        let mut input = json!({"add":"host","port":"443","id":ID});
        input[key] = value;
        let parsed =
            parse_uris(&format!("vmess://{}", STANDARD.encode(input.to_string()))).unwrap();
        assert!(parsed.nodes.is_empty());
        assert_eq!(parsed.unsupported.len(), 1);
    }
    let input = json!({"add":"host","port":443,"id":ID,"aid":0,"scy":"chacha20-poly1305","net":"ws","host":"front.example","path":"/x","tls":"tls","sni":"sni.example","alpn":"h2,http/1.1","fp":"firefox","allowInsecure":false});
    let out = node(&format!("vmess://{}", STANDARD.encode(input.to_string())))
        .outbound("out")
        .unwrap();
    assert_eq!(out["security"], "chacha20-poly1305");
    assert_eq!(out["alter_id"], 0);
    assert_eq!(out["transport"]["headers"]["Host"], "front.example");
    assert_eq!(out["tls"]["server_name"], "sni.example");
}

#[test]
fn names_regions_duplicates_and_safe_indexed_diagnostics() {
    for (name, expected) in [
        ("🇯🇵 東京", Region::Japan),
        ("HK 01", Region::HongKong),
        ("US west", Region::Us),
        ("台湾", Region::Taiwan),
        ("adjust", Region::Unknown),
    ] {
        assert_eq!(
            node(&format!("trojan://pass@host:443#{name}")).region(),
            expected
        );
    }
    assert_eq!(
        parse_uris("trojan://pass@host:443#same\nanytls://pass@host:443#same").err(),
        Some(Error::DuplicateNames)
    );
    let parsed = parse_uris("# comment\n\nunknown-secret://credential@host\ntrojan://pass@host:443?token=secret#bad\ntrojan://pass@host:443#valid").unwrap();
    assert_eq!(parsed.nodes.len(), 1);
    assert_eq!(parsed.unsupported[0].source_index, 3);
    assert_eq!(parsed.unsupported[1].source_index, 4);
    let diagnostic = format!("{:?}", parsed.unsupported);
    assert!(!diagnostic.contains("secret"));
    assert!(!diagnostic.contains("credential"));
    assert!(!diagnostic.contains("host"));
}

#[test]
fn input_limits_and_invalid_base64_are_bounded() {
    assert_eq!(parse_uris("").err(), Some(Error::Empty));
    assert_eq!(
        parse_uris(&"a".repeat(INPUT_LIMIT + 1)).err(),
        Some(Error::TooLarge)
    );
    assert_eq!(parse_uris("not base64!").err(), Some(Error::Encoding));
    assert_eq!(
        parse_uris(&STANDARD.encode([0xff])).err(),
        Some(Error::Encoding)
    );
    assert_eq!(
        parse_uris(&"unknown://x\n".repeat(NODE_LIMIT + 1)).err(),
        Some(Error::TooManyNodes)
    );
    rejected(
        &format!("trojan://{}@host:443#x", "a".repeat(LINE_LIMIT)),
        Error::TooLarge,
    );
}

#[test]
fn key_encodings_are_normalized_and_tcp_http_camouflage_is_not_reinterpreted() {
    let key = STANDARD.encode([255u8; 32]);
    let escaped =
        percent_encoding::utf8_percent_encode(&key, percent_encoding::NON_ALPHANUMERIC).to_string();
    let reality = node(&format!(
        "vless://{ID}@host:443?security=reality&pbk={escaped}#x"
    ));
    assert_eq!(
        reality.outbound("out").unwrap()["tls"]["reality"]["public_key"],
        URL_SAFE_NO_PAD.encode([255u8; 32])
    );
    for method in [
        "2022-blake3-aes-128-gcm",
        "2022-blake3-aes-256-gcm",
        "2022-blake3-chacha20-poly1305",
    ] {
        let bytes = vec![255u8; if method.contains("128") { 16 } else { 32 }];
        for engine in [&STANDARD, &STANDARD_NO_PAD, &URL_SAFE, &URL_SAFE_NO_PAD] {
            let key = engine.encode(&bytes);
            let parsed = node(&format!("ss://{method}:{key}@host:443#x"));
            assert_eq!(
                parsed.outbound("out").unwrap()["password"],
                STANDARD.encode(&bytes)
            );
        }
    }
    let key = STANDARD.encode([255u8; 32]);
    rejected(
        &format!("ss://2022-blake3-chacha20-poly1305:{key}:{key}@host:443#x"),
        Error::UnsupportedOption,
    );
    for query in [
        "type=tcp&headerType=http",
        "type=tcp&headerType=http&security=tls",
    ] {
        rejected(
            &format!("vless://{ID}@host:443?{query}#x"),
            Error::UnsupportedOption,
        );
    }
}

#[test]
fn h2_requires_tls_without_changing_explicit_http_semantics() {
    rejected(
        &format!("vless://{ID}@host:443?type=h2#h2"),
        Error::UnsupportedOption,
    );
    let input = json!({"add":"host","port":443,"id":ID,"net":"h2"});
    rejected(
        &format!("vmess://{}", STANDARD.encode(input.to_string())),
        Error::UnsupportedOption,
    );
    let plain_http = node(&format!("vless://{ID}@host:80?type=http#http"))
        .outbound("out")
        .unwrap();
    assert_eq!(plain_http["transport"]["type"], "http");
    assert!(plain_http.get("tls").is_none());
    let h2 = node(&format!("vless://{ID}@host:443?type=h2&security=tls#h2"))
        .outbound("out")
        .unwrap();
    assert_eq!(h2["transport"]["type"], "http");
    assert_eq!(h2["tls"]["enabled"], true);
}

#[test]
fn outbound_revalidates_mutated_nodes_and_exposes_no_arbitrary_fields() {
    let mut parsed = node("trojan://pass@host:443#safe");
    parsed.port = 0;
    assert_eq!(parsed.outbound("o").err(), Some(Error::InvalidNode));
    parsed.port = 443;
    parsed.tls = None;
    assert_eq!(parsed.outbound("o").err(), Some(Error::InvalidNode));
    let key = STANDARD.encode([42u8; 16]);
    let ss = node(&format!("ss://2022-blake3-aes-128-gcm:{key}@host:443#ss"));
    assert_eq!(
        ss.outbound("o").unwrap()["method"],
        "2022-blake3-aes-128-gcm"
    );
    rejected(
        "ss://2022-blake3-aes-128-gcm:short@host:443#ss",
        Error::Encoding,
    );
}
