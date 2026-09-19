use super::{Error, Node, Protocol, Reality, Result, Tls, Transport, base64_text, host};
use serde::Deserialize;
use std::collections::BTreeMap;
use uuid::Uuid;

pub(super) fn parse(line: &str) -> Result<Node> {
    if line.chars().any(char::is_control) {
        return Err(Error::InvalidNode);
    }
    let (scheme, payload) = line.split_once("://").ok_or(Error::Format)?;
    if scheme == "vmess" {
        return vmess(payload);
    }
    if !["anytls", "vless", "ss", "trojan", "hysteria2", "hy2"].contains(&scheme) {
        return Err(Error::Protocol);
    }
    let (before_fragment, fragment) = payload.split_once('#').unwrap_or((payload, ""));
    let (authority, query) = before_fragment
        .split_once('?')
        .unwrap_or((before_fragment, ""));
    let mut options = Options::parse(query)?;
    let (credential, endpoint) = if scheme == "ss" {
        shadowsocks_authority(authority)?
    } else {
        let (userinfo, endpoint) = authority.rsplit_once('@').ok_or(Error::InvalidNode)?;
        (decode(userinfo, false)?, endpoint.to_owned())
    };
    let endpoint = endpoint.strip_suffix('/').unwrap_or(&endpoint);
    let (server, port) = endpoint_parts(endpoint, matches!(scheme, "hy2" | "hysteria2"))?;
    let name = if fragment.is_empty() {
        server.clone()
    } else {
        decode(fragment, false)?
    };
    let protocol = match scheme {
        "anytls" => Protocol::AnyTls {
            password: credential,
        },
        "trojan" => Protocol::Trojan {
            password: credential,
        },
        "vless" => {
            if options.take(&["encryption"])?.is_some_and(|s| s != "none") {
                return Err(Error::UnsupportedOption);
            }
            Protocol::Vless {
                uuid: Uuid::parse_str(&credential).map_err(|_| Error::InvalidNode)?,
                flow: options.take(&["flow"])?,
            }
        }
        "ss" => {
            let (method, password) = credential.split_once(':').ok_or(Error::InvalidNode)?;
            Protocol::Shadowsocks {
                method: method.into(),
                password: password.into(),
            }
        }
        "hysteria2" | "hy2" => {
            let obfs = options.take(&["obfs"])?;
            let password = options.take(&["obfs-password", "obfs_password", "obfsPassword"])?;
            match (obfs.as_deref(), &password) {
                (None, None) => {}
                (Some("salamander"), Some(_)) => {}
                _ => return Err(Error::UnsupportedOption),
            }
            Protocol::Hysteria2 {
                password: credential,
                obfs_password: password,
                up_mbps: options.number(&["upmbps", "up_mbps"])?,
                down_mbps: options.number(&["downmbps", "down_mbps"])?,
            }
        }
        _ => return Err(Error::Protocol),
    };
    let tls = tls(
        &mut options,
        matches!(scheme, "anytls" | "trojan" | "hysteria2" | "hy2"),
    )?;
    let transport = transport(&mut options, tls.is_some())?;
    options.finish()?;
    let node = Node {
        name,
        server,
        port,
        protocol,
        tls,
        transport,
        tcp_only: false,
    };
    node.validate()?;
    Ok(node)
}

pub(super) fn endpoint_parts(value: &str, default_https: bool) -> Result<(String, u16)> {
    let (name, port) = if value.starts_with('[') {
        let closing = value.find(']').ok_or(Error::InvalidNode)?;
        let port = &value[closing + 1..];
        (
            &value[..=closing],
            port.strip_prefix(':')
                .or_else(|| (port.is_empty() && default_https).then_some("443"))
                .ok_or(Error::InvalidNode)?,
        )
    } else if let Some((name, port)) = value.rsplit_once(':') {
        if name.contains(':') {
            return Err(Error::InvalidNode);
        }
        (name, port)
    } else if default_https {
        (value, "443")
    } else {
        return Err(Error::InvalidNode);
    };
    Ok((host(name)?, decimal(port)?))
}
pub(super) fn decimal<T: std::str::FromStr>(value: &str) -> Result<T> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidNode);
    }
    value.parse().map_err(|_| Error::InvalidNode)
}
fn decode(value: &str, plus_as_space: bool) -> Result<String> {
    let mut rest = value.as_bytes();
    while let Some(pos) = rest.iter().position(|b| *b == b'%') {
        rest = &rest[pos + 1..];
        if rest.len() < 2 || !rest[..2].iter().all(u8::is_ascii_hexdigit) {
            return Err(Error::Encoding);
        }
        rest = &rest[2..];
    }
    let value = if plus_as_space {
        value.replace('+', " ")
    } else {
        value.to_owned()
    };
    percent_encoding::percent_decode_str(&value)
        .decode_utf8()
        .map(|s| s.into_owned())
        .map_err(|_| Error::Encoding)
}
fn shadowsocks_authority(value: &str) -> Result<(String, String)> {
    if let Some((userinfo, endpoint)) = value.rsplit_once('@') {
        let userinfo = decode(userinfo, false)?;
        // A Base64 payload already represents the literal credential bytes.
        let credential = if userinfo.contains(':') {
            userinfo
        } else {
            base64_text(&userinfo)?
        };
        Ok((credential, endpoint.into()))
    } else {
        let decoded = base64_text(value)?;
        let (credential, endpoint) = decoded.rsplit_once('@').ok_or(Error::InvalidNode)?;
        Ok((credential.into(), endpoint.into()))
    }
}

#[derive(Default)]
pub(super) struct Options(pub(super) BTreeMap<String, String>);
impl Options {
    fn parse(query: &str) -> Result<Self> {
        let mut values = BTreeMap::new();
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').ok_or(Error::Format)?;
            let key = decode(key, true)?;
            if key.is_empty() || values.insert(key, decode(value, true)?).is_some() {
                return Err(Error::ConflictingOptions);
            }
        }
        Ok(Self(values))
    }
    fn take(&mut self, names: &[&str]) -> Result<Option<String>> {
        let mut value = None;
        for name in names {
            if let Some(next) = self.0.remove(*name)
                && value.replace(next).is_some()
            {
                return Err(Error::ConflictingOptions);
            }
        }
        Ok(value)
    }
    fn number(&mut self, names: &[&str]) -> Result<Option<u32>> {
        self.take(names)?.map(|s| decimal(&s)).transpose()
    }
    fn flag(&mut self, names: &[&str]) -> Result<Option<bool>> {
        self.take(names)?
            .map(|s| match s.as_str() {
                "1" | "true" => Ok(true),
                "0" | "false" => Ok(false),
                _ => Err(Error::InvalidNode),
            })
            .transpose()
    }
    fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::UnsupportedOption)
        }
    }
}
pub(super) fn tls(options: &mut Options, required: bool) -> Result<Option<Tls>> {
    let security = options.take(&["security"])?;
    let enabled = options.flag(&["tls"])?;
    if security.is_some() && enabled.is_some() {
        return Err(Error::ConflictingOptions);
    }
    let server_name = options.take(&["sni", "peer", "serverName", "servername"])?;
    let insecure = options.flag(&["allowInsecure", "insecure", "skip-cert-verify"])?;
    let alpn = options.take(&["alpn"])?;
    let mut fingerprint = options.take(&["fp", "fingerprint"])?;
    let public_key = options.take(&["pbk", "public-key", "publicKey"])?;
    let short_id = options.take(&["sid", "short-id", "shortId"])?;
    let default = if required || enabled == Some(true) {
        "tls"
    } else if public_key.is_some() {
        "reality"
    } else {
        "none"
    };
    let mode = security.as_deref().unwrap_or(default);
    if !["none", "tls", "reality"].contains(&mode) {
        return Err(Error::UnsupportedOption);
    }
    if enabled == Some(false) || mode == "none" {
        if required
            || server_name.is_some()
            || insecure.is_some()
            || alpn.is_some()
            || fingerprint.is_some()
            || public_key.is_some()
            || short_id.is_some()
        {
            return Err(Error::ConflictingOptions);
        }
        return Ok(None);
    }
    let reality = if mode == "reality" {
        if fingerprint.is_none() {
            fingerprint = Some("chrome".into());
        }
        Some(Reality {
            public_key: public_key.ok_or(Error::InvalidNode)?,
            short_id: short_id.unwrap_or_default(),
        })
    } else {
        if public_key.is_some() || short_id.is_some() {
            return Err(Error::ConflictingOptions);
        }
        None
    };
    Ok(Some(Tls {
        server_name,
        insecure: insecure.unwrap_or(false),
        alpn: alpn
            .map(|s| s.split(',').map(str::to_owned).collect())
            .unwrap_or_default(),
        fingerprint,
        reality,
    }))
}
pub(super) fn transport(options: &mut Options, tls_enabled: bool) -> Result<Option<Transport>> {
    let kind = options
        .take(&["type", "network"])?
        .unwrap_or_else(|| "tcp".into());
    let header_type = options.take(&["headerType"])?;
    // sing-box HTTP is HTTP/1 without TLS and HTTP/2 with TLS. Do not silently
    // turn an explicit h2 request into a different wire protocol.
    if kind == "h2" && !tls_enabled {
        return Err(Error::UnsupportedOption);
    }
    let kind = match (kind.as_str(), header_type.as_deref()) {
        (kind, None | Some("none")) => kind,
        _ => return Err(Error::UnsupportedOption),
    };
    Ok(match kind {
        "tcp" => None,
        "ws" => Some(Transport::WebSocket {
            path: options.take(&["path"])?.unwrap_or_else(|| "/".into()),
            host: options.take(&["host"])?,
        }),
        "http" | "h2" => Some(Transport::Http {
            method: None,
            path: options.take(&["path"])?.unwrap_or_else(|| "/".into()),
            hosts: options
                .take(&["host"])?
                .map(|s| s.split(',').map(str::to_owned).collect())
                .unwrap_or_default(),
        }),
        "grpc" => Some(Transport::Grpc {
            service_name: options
                .take(&["serviceName", "service_name", "path"])?
                .unwrap_or_default(),
        }),
        "httpupgrade" => Some(Transport::HttpUpgrade {
            path: options.take(&["path"])?.unwrap_or_else(|| "/".into()),
            host: options.take(&["host"])?,
        }),
        "quic" => Some(Transport::Quic {}),
        _ => return Err(Error::UnsupportedOption),
    })
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Number {
    String(String),
    Integer(u64),
}
impl Number {
    fn text(self) -> String {
        match self {
            Self::String(s) => s,
            Self::Integer(n) => n.to_string(),
        }
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vmess {
    #[serde(default)]
    v: Option<Number>,
    #[serde(default, alias = "name")]
    ps: String,
    add: String,
    port: Number,
    id: String,
    #[serde(default)]
    aid: Option<Number>,
    #[serde(default)]
    scy: String,
    #[serde(default)]
    net: String,
    #[serde(default)]
    tls: String,
    #[serde(default)]
    sni: String,
    #[serde(default)]
    host: String,
    #[serde(default)]
    path: String,
    #[serde(default, rename = "type")]
    header_type: String,
    #[serde(default)]
    alpn: String,
    #[serde(default)]
    fp: String,
    #[serde(default, rename = "allowInsecure")]
    allow_insecure: Option<serde_json::Value>,
}
fn vmess(payload: &str) -> Result<Node> {
    let decoded = base64_text(payload)?;
    let data: Vmess = serde_json::from_str(&decoded).map_err(|_| Error::InvalidNode)?;
    if data.v.is_some_and(|v| v.text() != "2") {
        return Err(Error::UnsupportedOption);
    }
    let mut options = Options::default();
    for (key, value) in [
        ("type", data.net),
        ("security", data.tls),
        ("sni", data.sni),
        ("host", data.host),
        ("path", data.path),
        ("headerType", data.header_type),
        ("alpn", data.alpn),
        ("fp", data.fp),
    ] {
        if !value.is_empty() {
            options.0.insert(key.into(), value);
        }
    }
    if let Some(value) = data.allow_insecure {
        let flag = match value {
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::String(s) => s,
            serde_json::Value::Number(n) => n.to_string(),
            _ => return Err(Error::InvalidNode),
        };
        options.0.insert("allowInsecure".into(), flag);
    }
    let tls = tls(&mut options, false)?;
    let transport = transport(&mut options, tls.is_some())?;
    options.finish()?;
    let server = host(&data.add)?;
    let node = Node {
        name: if data.ps.is_empty() {
            server.clone()
        } else {
            data.ps
        },
        server,
        port: decimal(&data.port.text())?,
        protocol: Protocol::Vmess {
            uuid: Uuid::parse_str(&data.id).map_err(|_| Error::InvalidNode)?,
            security: if data.scy.is_empty() {
                "auto".into()
            } else {
                data.scy
            },
            alter_id: match data.aid {
                Some(n) => decimal(&n.text())?,
                None => 0,
            },
        },
        tls,
        transport,
        tcp_only: false,
    };
    node.validate()?;
    Ok(node)
}
