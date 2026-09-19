use super::{Error, Result, base64_bytes, clean, host};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::{Value, json};
use uuid::Uuid;

// No Debug/Serialize: only the persistence layer may split secrets into references.
#[derive(Clone, PartialEq, Eq)]
pub struct Node {
    pub name: String,
    pub server: String,
    pub port: u16,
    pub protocol: Protocol,
    pub tls: Option<Tls>,
    pub transport: Option<Transport>,
    pub tcp_only: bool,
}
#[derive(Clone, PartialEq, Eq)]
pub enum Protocol {
    AnyTls {
        password: String,
    },
    Vless {
        uuid: Uuid,
        flow: Option<String>,
    },
    Vmess {
        uuid: Uuid,
        security: String,
        alter_id: u16,
    },
    Shadowsocks {
        method: String,
        password: String,
    },
    Trojan {
        password: String,
    },
    Hysteria2 {
        password: String,
        obfs_password: Option<String>,
        up_mbps: Option<u32>,
        down_mbps: Option<u32>,
    },
}
#[derive(Clone, PartialEq, Eq)]
pub struct Tls {
    pub server_name: Option<String>,
    pub insecure: bool,
    pub alpn: Vec<String>,
    pub fingerprint: Option<String>,
    pub reality: Option<Reality>,
}
#[derive(Clone, PartialEq, Eq)]
pub struct Reality {
    pub public_key: String,
    pub short_id: String,
}
#[derive(Clone, PartialEq, Eq)]
pub enum Transport {
    WebSocket {
        path: String,
        host: Option<String>,
    },
    Http {
        path: String,
        hosts: Vec<String>,
        method: Option<String>,
    },
    Grpc {
        service_name: String,
    },
    HttpUpgrade {
        path: String,
        host: Option<String>,
    },
    Quic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Taiwan,
    HongKong,
    Japan,
    Us,
    Singapore,
    SouthKorea,
    Uk,
    Germany,
    France,
    Netherlands,
    Canada,
    Australia,
    Unknown,
}
impl Node {
    pub fn region(&self) -> Region {
        let lower = self.name.to_lowercase();
        let words: Vec<_> = lower.split(|c: char| !c.is_ascii_alphanumeric()).collect();
        for (region, flag, terms, labels) in [
            (
                Region::Taiwan,
                "🇹🇼",
                &["tw", "taiwan"][..],
                &["台灣", "台湾"][..],
            ),
            (
                Region::HongKong,
                "🇭🇰",
                &["hk"][..],
                &["香港", "hong kong"][..],
            ),
            (
                Region::Japan,
                "🇯🇵",
                &["jp", "japan"][..],
                &["日本", "东京", "東京"][..],
            ),
            (
                Region::Us,
                "🇺🇸",
                &["us", "usa", "america"][..],
                &["美国", "美國", "united states"][..],
            ),
            (
                Region::Singapore,
                "🇸🇬",
                &["sg", "singapore"][..],
                &["新加坡", "狮城", "獅城"][..],
            ),
            (
                Region::SouthKorea,
                "🇰🇷",
                &["kr", "korea"][..],
                &["韩国", "韓國", "首尔", "首爾"][..],
            ),
            (
                Region::Uk,
                "🇬🇧",
                &["uk", "gb", "britain", "england"][..],
                &["英国", "英國", "united kingdom"][..],
            ),
            (
                Region::Germany,
                "🇩🇪",
                &["de", "germany"][..],
                &["德国", "德國"][..],
            ),
            (
                Region::France,
                "🇫🇷",
                &["fr", "france"][..],
                &["法国", "法國"][..],
            ),
            (
                Region::Netherlands,
                "🇳🇱",
                &["nl", "netherlands", "holland"][..],
                &["荷兰", "荷蘭"][..],
            ),
            (Region::Canada, "🇨🇦", &["ca", "canada"][..], &["加拿大"][..]),
            (
                Region::Australia,
                "🇦🇺",
                &["au", "australia"][..],
                &["澳大利亚", "澳大利亞", "澳洲"][..],
            ),
        ] {
            if lower.contains(flag)
                || terms.iter().any(|t| words.contains(t))
                || labels.iter().any(|t| lower.contains(t))
            {
                return region;
            }
        }
        Region::Unknown
    }

    pub fn validate(&self) -> Result<()> {
        if self.tcp_only && matches!(self.protocol, Protocol::AnyTls { .. }) {
            return Err(Error::UnsupportedOption);
        }
        if !clean(&self.name, 256) || self.port == 0 {
            return Err(Error::InvalidNode);
        }
        host(&self.server)?;
        match &self.protocol {
            Protocol::AnyTls { password }
            | Protocol::Trojan { password }
            | Protocol::Hysteria2 { password, .. } => {
                if !clean(password, 4096) || self.tls.is_none() {
                    return Err(Error::InvalidNode);
                }
            }
            Protocol::Vless { flow, .. } => {
                if flow.as_ref().is_some_and(|f| {
                    f != "xtls-rprx-vision" || self.tls.is_none() || self.transport.is_some()
                }) {
                    return Err(Error::UnsupportedOption);
                }
            }
            Protocol::Vmess { security, .. } => {
                if ![
                    "auto",
                    "none",
                    "zero",
                    "aes-128-gcm",
                    "chacha20-poly1305",
                    "aes-128-ctr",
                ]
                .contains(&security.as_str())
                {
                    return Err(Error::UnsupportedOption);
                }
            }
            Protocol::Shadowsocks { method, password } => {
                if !clean(password, 4096) {
                    return Err(Error::InvalidNode);
                }
                if ![
                    "2022-blake3-aes-128-gcm",
                    "2022-blake3-aes-256-gcm",
                    "2022-blake3-chacha20-poly1305",
                    "none",
                    "aes-128-gcm",
                    "aes-192-gcm",
                    "aes-256-gcm",
                    "chacha20-ietf-poly1305",
                    "xchacha20-ietf-poly1305",
                    "aes-128-ctr",
                    "aes-192-ctr",
                    "aes-256-ctr",
                    "aes-128-cfb",
                    "aes-192-cfb",
                    "aes-256-cfb",
                    "rc4-md5",
                    "chacha20-ietf",
                    "xchacha20",
                ]
                .contains(&method.as_str())
                {
                    return Err(Error::UnsupportedOption);
                }
                if method.starts_with("2022-") {
                    if method == "2022-blake3-chacha20-poly1305" && password.contains(':') {
                        return Err(Error::UnsupportedOption);
                    }
                    let length = if method == "2022-blake3-aes-128-gcm" {
                        16
                    } else {
                        32
                    };
                    for key in password.split(':') {
                        if base64_bytes(key)?.len() != length {
                            return Err(Error::InvalidNode);
                        }
                    }
                }
                if self.tls.is_some() {
                    return Err(Error::UnsupportedOption);
                }
            }
        }
        if matches!(
            self.protocol,
            Protocol::AnyTls { .. } | Protocol::Hysteria2 { .. } | Protocol::Shadowsocks { .. }
        ) && self.transport.is_some()
        {
            return Err(Error::UnsupportedOption);
        }
        if let Protocol::Hysteria2 {
            obfs_password,
            up_mbps,
            down_mbps,
            ..
        } = &self.protocol
            && (obfs_password.as_ref().is_some_and(|p| !clean(p, 4096))
                || up_mbps == &Some(0)
                || down_mbps == &Some(0))
        {
            return Err(Error::InvalidNode);
        }
        if let Some(tls) = &self.tls {
            if let Some(server) = &tls.server_name {
                host(server)?;
            }
            if tls.alpn.len() > 16 || tls.alpn.iter().any(|v| !clean(v, 255) || !v.is_ascii()) {
                return Err(Error::InvalidNode);
            }
            if let Some(fp) = &tls.fingerprint
                && ![
                    "chrome",
                    "firefox",
                    "edge",
                    "safari",
                    "360",
                    "qq",
                    "ios",
                    "android",
                    "random",
                    "randomized",
                ]
                .contains(&fp.as_str())
            {
                return Err(Error::UnsupportedOption);
            }
            if let Some(reality) = &tls.reality {
                if tls.insecure
                    || base64_bytes(&reality.public_key)?.len() != 32
                    || reality.short_id.len() > 16
                    || reality.short_id.len() % 2 != 0
                    || !reality.short_id.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err(Error::InvalidNode);
                }
                if tls.fingerprint.is_none() {
                    return Err(Error::InvalidNode);
                }
            }
            if (matches!(self.protocol, Protocol::Hysteria2 { .. })
                || self.transport == Some(Transport::Quic))
                && (tls.fingerprint.is_some() || tls.reality.is_some())
            {
                return Err(Error::UnsupportedOption);
            }
        }
        if let Some(transport) = &self.transport {
            match transport {
                Transport::WebSocket { path, host: name }
                | Transport::HttpUpgrade { path, host: name } => {
                    valid_path(path)?;
                    if name
                        .as_ref()
                        .is_some_and(|s| !clean(s, 1024) || !s.is_ascii())
                    {
                        return Err(Error::InvalidNode);
                    }
                }
                Transport::Http {
                    path,
                    hosts,
                    method,
                } => {
                    if method.as_ref().is_some_and(|s| {
                        s.is_empty()
                            || s.len() > 32
                            || !s.bytes().all(|b| b.is_ascii_uppercase() || b == b'-')
                    }) {
                        return Err(Error::UnsupportedOption);
                    }
                    valid_path(path)?;
                    if hosts.len() > 16 {
                        return Err(Error::InvalidNode);
                    }
                    for name in hosts {
                        host(name)?;
                    }
                }
                Transport::Grpc { service_name } => {
                    if service_name.len() > 1024 || service_name.chars().any(char::is_control) {
                        return Err(Error::InvalidNode);
                    }
                }
                Transport::Quic if self.tls.is_none() => return Err(Error::InvalidNode),
                Transport::Quic => {}
            }
        }
        Ok(())
    }

    /// Contains secrets. Only compose into a protected generated config; never
    /// log or return this JSON through diagnostics. All fields are allow-listed.
    pub fn outbound(&self, tag: &str) -> Result<Value> {
        self.validate()?;
        if !clean(tag, 256) {
            return Err(Error::InvalidNode);
        }
        let mut out = match &self.protocol {
            Protocol::AnyTls { password } => json!({"type":"anytls","password":password}),
            Protocol::Vless { uuid, flow } => {
                let mut out = json!({"type":"vless","uuid":uuid});
                if let Some(flow) = flow {
                    out["flow"] = json!(flow);
                }
                out
            }
            Protocol::Vmess {
                uuid,
                security,
                alter_id,
            } => json!({"type":"vmess","uuid":uuid,"security":security,"alter_id":alter_id}),
            Protocol::Shadowsocks { method, password } => {
                let password = if method.starts_with("2022-") {
                    password
                        .split(':')
                        .map(|key| base64_bytes(key).map(|bytes| STANDARD.encode(bytes)))
                        .collect::<Result<Vec<_>>>()?
                        .join(":")
                } else {
                    password.clone()
                };
                json!({"type":"shadowsocks","method":method,"password":password})
            }
            Protocol::Trojan { password } => json!({"type":"trojan","password":password}),
            Protocol::Hysteria2 {
                password,
                obfs_password,
                up_mbps,
                down_mbps,
            } => {
                let mut out = json!({"type":"hysteria2","password":password});
                if let Some(password) = obfs_password {
                    out["obfs"] = json!({"type":"salamander","password":password});
                }
                if let Some(up) = up_mbps {
                    out["up_mbps"] = json!(up);
                }
                if let Some(down) = down_mbps {
                    out["down_mbps"] = json!(down);
                }
                out
            }
        };
        out["tag"] = json!(tag);
        out["server"] = json!(host(&self.server)?);
        out["server_port"] = json!(self.port);
        if self.tcp_only {
            out["network"] = json!("tcp");
        }
        if let Some(tls) = &self.tls {
            let mut value = json!({"enabled":true,"insecure":tls.insecure});
            if let Some(name) = &tls.server_name {
                value["server_name"] = json!(host(name)?);
            }
            if !tls.alpn.is_empty() {
                value["alpn"] = json!(tls.alpn);
            }
            if let Some(fp) = &tls.fingerprint {
                value["utls"] = json!({"enabled":true,"fingerprint":fp});
            }
            if let Some(reality) = &tls.reality {
                value["reality"] = json!({"enabled":true,"public_key":URL_SAFE_NO_PAD.encode(base64_bytes(&reality.public_key)?),"short_id":reality.short_id});
            }
            out["tls"] = value;
        }
        if let Some(transport) = &self.transport {
            out["transport"] = match transport {
                Transport::WebSocket { path, host } => {
                    let mut value = json!({"type":"ws","path":path});
                    if let Some(host) = host {
                        value["headers"] = json!({"Host":host});
                    }
                    value
                }
                Transport::Http {
                    path,
                    hosts,
                    method,
                } => {
                    let mut value = json!({"type":"http","path":path,"host":hosts});
                    if let Some(method) = method {
                        value["method"] = json!(method);
                    }
                    value
                }
                Transport::Grpc { service_name } => {
                    json!({"type":"grpc","service_name":service_name})
                }
                Transport::HttpUpgrade { path, host } => {
                    let mut value = json!({"type":"httpupgrade","path":path});
                    if let Some(host) = host {
                        value["host"] = json!(host);
                    }
                    value
                }
                Transport::Quic => json!({"type":"quic"}),
            };
        }
        Ok(out)
    }
}
fn valid_path(path: &str) -> Result<()> {
    if !clean(path, 4096) || !path.starts_with('/') {
        return Err(Error::InvalidNode);
    }
    Ok(())
}
