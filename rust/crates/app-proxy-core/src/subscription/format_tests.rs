use super::*;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::json;
const YAML: &str = include_str!("../../tests/fixtures/subscription.yaml");
const TEXT: &str = include_str!("../../tests/fixtures/subscription.txt");
const ID: &str = "12345678-1234-1234-1234-123456789abc";
fn one(text: &str) -> Node {
    let mut parsed = parse(text).unwrap();
    assert_eq!(parsed.unsupported, []);
    assert_eq!(parsed.nodes.len(), 1);
    parsed.nodes.remove(0)
}
#[test]
fn clash_six_protocols_nested_alpn_and_client_sections() {
    for text in [YAML, TEXT] {
        let parsed = parse(text).unwrap();
        assert_eq!(parsed.unsupported, []);
        assert_eq!(parsed.nodes.len(), 6);
        for (node, kind) in parsed.nodes.iter().zip([
            "anytls",
            "vless",
            "vmess",
            "shadowsocks",
            "trojan",
            "hysteria2",
        ]) {
            assert_eq!(node.outbound("out").unwrap()["type"], kind);
        }
        assert_eq!(
            parsed.nodes[0].outbound("out").unwrap()["password"],
            "fixture%2F,secret"
        );
        assert_eq!(parsed.nodes[3].outbound("out").unwrap()["network"], "tcp");
    }
    let parsed = parse(YAML).unwrap();
    assert_eq!(
        parsed.nodes[0].tls.as_ref().unwrap().alpn,
        ["h2", "http/1.1"]
    );
    assert_eq!(parsed.nodes[4].tls.as_ref().unwrap().alpn, ["h2"]);
    assert_eq!(
        parsed.nodes[1].outbound("o").unwrap()["transport"]["path"],
        "/edge%2F"
    );
    assert_eq!(
        parsed.nodes[2].outbound("o").unwrap()["transport"]["service_name"],
        "edge"
    );
}
#[test]
fn all_containers_detect_after_one_base64_envelope() {
    for text in [
        YAML,
        TEXT,
        "\n\ntrojan://pass@host:443#URI",
        "proxies: [{name: x, type: trojan, server: host, port: 443, password: pass}]",
    ] {
        let plain = parse(text).unwrap();
        for engine in [&STANDARD, &URL_SAFE_NO_PAD] {
            let encoded = engine.encode(text);
            let parsed = parse(&encoded).unwrap();
            assert!(parsed.nodes == plain.nodes);
            assert_eq!(parsed.unsupported, plain.unsupported);
        }
    }
    let double = STANDARD.encode(STANDARD.encode(TEXT));
    assert_eq!(parse(&double).err(), Some(Error::Format));
}
#[test]
fn quoted_commas_equals_escapes_and_literal_percent_survive_text() {
    for (value, expected) in [
        (r#""p,a=b%2F\\tail\"end""#, "p,a=b%2F\\tail\"end"),
        ("'p,a=b%2F''end'", "p,a=b%2F'end"),
    ] {
        let n = one(&format!(
            "[Proxy]\n'JP, one' = trojan, host, 443, password={value}"
        ));
        assert_eq!(n.name, "JP, one");
        assert_eq!(n.outbound("out").unwrap()["password"], expected);
    }
    let input = format!(
        "[server_local]\nvmess=127.0.0.1:443, method=auto, password={ID}, obfs=wss, obfs-host=front.example, obfs-uri=/edge%2F, aead=false, fast-open=false, udp-relay=false, tag=JP"
    );
    let out = one(&input).outbound("out").unwrap();
    assert_eq!(out["alter_id"], 1);
    assert_eq!(out["tls"]["server_name"], "front.example");
    assert_eq!(out["transport"]["headers"]["Host"], "front.example");
    assert_eq!(out["transport"]["path"], "/edge%2F");
}
#[test]
fn yaml_merge_precedence_and_quoted_scalar_types_are_preserved() {
    let text = "first: &first {type: trojan, server: host, port: 443, password: '001', name: first}\nsecond: &second {password: second, sni: example.com}\nproxies:\n- <<: [*first, *second]\n  name: final\n";
    let out = one(text).outbound("o").unwrap();
    assert_eq!(out["password"], "001");
    assert_eq!(out["tls"]["server_name"], "example.com");
    let n = one("proxies: [{name: x, type: trojan, server: host, port: 443, password: 'null'}]");
    assert_eq!(n.outbound("o").unwrap()["password"], "null");
    let parsed =
        parse("proxies: [{name: x, type: trojan, server: host, port: 443, password: null}]")
            .unwrap();
    assert!(parsed.nodes.is_empty());
}
#[test]
fn document_ambiguity_alias_depth_and_merge_work_are_bounded() {
    for text in [
        "proxies: []\nproxies: []",
        "proxies: []\n---\nproxies: []",
        "proxies: !custom []",
        "a: &a [*a]\nproxies: []",
    ] {
        assert!(parse(text).is_err());
    }
    let nested = format!("unused: {}0{}\nproxies: []", "[".repeat(40), "]".repeat(40));
    assert_eq!(parse(&nested).err(), Some(Error::TooLarge));
    let mut aliases = "a0: &a0 [0]\n".to_owned();
    for index in 1..40 {
        aliases.push_str(&format!("a{index}: &a{index} [*a{}]\n", index - 1));
    }
    aliases.push_str("proxies: []");
    assert_eq!(parse(&aliases).err(), Some(Error::TooLarge));
    let mut bomb =
        "a0: &a0 {type: trojan, server: host, port: 443, password: pass, name: x}\n".to_owned();
    for index in 1..14 {
        bomb.push_str(&format!(
            "a{index}: &a{index} {{<<: [*a{}, *a{}]}}\n",
            index - 1,
            index - 1
        ));
    }
    bomb.push_str("proxies: [*a13]");
    let parsed = parse(&bomb).unwrap();
    assert!(parsed.nodes.is_empty());
    assert_eq!(parsed.unsupported[0].reason, Error::TooLarge);
}
#[test]
fn invalid_entries_report_actual_lines_without_secret_values() {
    let text = "\n\nproxies:\n- {name: good, type: trojan, server: host, port: 443, password: secret}\n- {name: bad, type: trojan, server: host, port: 443, password: secret, unknown-secret: secret}\n";
    let parsed = parse(text).unwrap();
    assert_eq!(parsed.nodes.len(), 1);
    assert_eq!(
        parsed.unsupported,
        [Issue {
            source_index: 5,
            reason: Error::UnsupportedOption
        }]
    );
    assert!(!format!("{:?}", parsed.unsupported).contains("secret"));
    let parsed = parse("\n[Proxy]\ngood = trojan, host, 443, password=pass\nbad = trojan, host, 443, password=pass, unknown=secret").unwrap();
    assert_eq!(parsed.unsupported[0].source_index, 4);
}
#[test]
fn unknown_nested_security_fields_and_protocol_semantics_are_not_dropped() {
    for field in [
        "fingerprint: chrome",
        "fingerprint: deadbeef",
        "dialer-proxy: other",
        "tls: false",
        "network: http, tls: true",
        "network: ws, ws-opts: {path: /, headers: {X-Unknown: secret}}",
        "udp: false, type: anytls",
    ] {
        let input = format!(
            "proxies: [{{name: x, type: trojan, server: host, port: 443, password: pass, {field}}}]"
        );
        if let Ok(parsed) = parse(&input) {
            assert!(parsed.nodes.is_empty());
        }
    }
    let parsed =
        parse("[Proxy]\nx = trojan, host, 443, password=pass, tls-cert-sha256=secret").unwrap();
    assert!(parsed.nodes.is_empty());
    assert_eq!(parsed.unsupported[0].reason, Error::UnsupportedOption);
    let n = one(
        "proxies: [{name: x, type: trojan, server: host, port: 443, password: pass, client-fingerprint: chrome}]",
    );
    assert_eq!(
        n.outbound("o").unwrap()["tls"]["utls"],
        json!({"enabled":true,"fingerprint":"chrome"})
    );
}
#[test]
fn only_plain_yaml_merge_keys_can_change_node_fields() {
    for merge in [r#""<<""#, "'<<'"] {
        let input = format!(
            "proxies: [{{name: x, type: trojan, server: host, port: 443, password: pass, {merge}: {{skip-cert-verify: true}}}}]"
        );
        assert_eq!(parse(&input).err(), Some(Error::UnsupportedOption));
    }
    let alias = "key: &key '<<'\nproxies:\n- name: x\n  type: trojan\n  server: host\n  port: 443\n  password: pass\n  *key: {skip-cert-verify: true}\n";
    assert!(parse(alias).is_err());
    let n = one("proxies: [{name: x, type: trojan, server: host, port: 443, password: '<<'}]");
    assert_eq!(n.outbound("out").unwrap()["password"], "<<");
    assert_eq!(n.outbound("out").unwrap()["tls"]["insecure"], false);
}

#[test]
fn clash_plain_http_keeps_get_default_and_explicit_method() {
    for (extra, method) in [
        ("", "GET"),
        (", http-opts: {method: POST, path: [/edge]}", "POST"),
    ] {
        let text = format!(
            "proxies: [{{name: x, type: vmess, server: host, port: 80, uuid: {ID}, network: http{extra}}}]"
        );
        let out = one(&text).outbound("out").unwrap();
        assert_eq!(out["transport"]["method"], method);
        assert!(out.get("tls").is_none());
    }
    let text = format!(
        "proxies: [{{name: x, type: vmess, server: host, port: 443, uuid: {ID}, network: http, tls: true}}]"
    );
    assert!(parse(&text).unwrap().nodes.is_empty());
}

#[test]
fn duplicate_fields_aliases_names_and_extra_positionals_are_rejected() {
    for input in [
        "[Proxy]\nx=trojan,host,443,password=a,password=b",
        "[Proxy]\nx=trojan,host,443,password=a,auth=b",
        "[Proxy]\nx=trojan,host,443,pass,extra",
        "[Proxy]\nx=trojan,host,443,password=\"unterminated",
        "proxies: [{name: x, type: trojan, server: host, port: 443, password: pass, sni: a, server-name: b}]",
    ] {
        assert!(parse(input).unwrap().nodes.is_empty());
    }
    assert_eq!(
        parse("[Proxy]\nx=trojan,host,443,password=a\nx=trojan,other,443,password=b").err(),
        Some(Error::DuplicateNames)
    );
}
