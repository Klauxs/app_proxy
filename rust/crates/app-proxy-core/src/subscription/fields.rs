//! Shared field adapter for structured YAML and client text. Values have already
//! been unquoted by their container parser; never URI-decode credentials here.
use super::{
    Error, Node, Protocol, Result,
    clash::Value,
    host,
    uri::{self, Options},
};
use std::collections::BTreeMap;
use uuid::Uuid;

pub(super) fn key(value: &str) -> String {
    value
        .chars()
        .filter(|c| !matches!(c, '-' | '_'))
        .flat_map(char::to_lowercase)
        .collect()
}
pub(super) struct Fields(BTreeMap<String, Value>);
impl Fields {
    pub fn new(map: BTreeMap<String, Value>) -> Result<Self> {
        let mut normalized = BTreeMap::new();
        for (name, value) in map {
            if normalized.insert(key(&name), value).is_some() {
                return Err(Error::ConflictingOptions);
            }
        }
        Ok(Self(normalized))
    }
    pub fn take(&mut self, names: &[&str]) -> Result<Option<Value>> {
        let mut value = None;
        for name in names {
            if let Some(next) = self.0.remove(&key(name))
                && value.replace(next).is_some()
            {
                return Err(Error::ConflictingOptions);
            }
        }
        Ok(value)
    }
    pub fn string(&mut self, names: &[&str]) -> Result<Option<String>> {
        self.take(names)?.map(|v| v.string()).transpose()
    }
    pub fn required(&mut self, names: &[&str]) -> Result<String> {
        self.string(names)?.ok_or(Error::InvalidNode)
    }
    pub fn flag(&mut self, names: &[&str]) -> Result<Option<bool>> {
        self.string(names)?
            .map(|s| match s.as_str() {
                "true" | "1" => Ok(true),
                "false" | "0" => Ok(false),
                _ => Err(Error::InvalidNode),
            })
            .transpose()
    }
    pub fn insert(&mut self, name: &str, value: Value) -> Result<()> {
        if self.0.insert(key(name), value).is_some() {
            return Err(Error::ConflictingOptions);
        }
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Error::UnsupportedOption)
        }
    }
}
pub(super) fn protocol(value: &str) -> Result<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "anytls" => Ok("anytls"),
        "vless" => Ok("vless"),
        "vmess" => Ok("vmess"),
        "ss" | "shadowsocks" => Ok("ss"),
        "trojan" => Ok("trojan"),
        "hy2" | "hysteria2" | "hysteria-2" => Ok("hysteria2"),
        _ => Err(Error::Protocol),
    }
}
pub(super) fn node(map: BTreeMap<String, Value>) -> Result<Node> {
    let mut f = Fields::new(map)?;
    let kind = protocol(&f.required(&["type"])?)?;
    let server = host(&f.required(&["server", "address"])?)?;
    let port = uri::decimal(&f.required(&["port", "server-port"])?)?;
    let name = f
        .string(&["name", "tag", "remarks"])?
        .unwrap_or_else(|| server.clone());
    let protocol = match kind {
        "anytls" => Protocol::AnyTls {
            password: f.required(&["password", "auth", "auth-str"])?,
        },
        "trojan" => Protocol::Trojan {
            password: f.required(&["password", "auth", "auth-str"])?,
        },
        "ss" => Protocol::Shadowsocks {
            method: f.required(&["cipher", "method", "encrypt-method"])?,
            password: f.required(&["password"])?,
        },
        "vless" => {
            if f.string(&["encryption", "method"])?
                .is_some_and(|s| s != "none")
            {
                return Err(Error::UnsupportedOption);
            }
            Protocol::Vless {
                uuid: Uuid::parse_str(&f.required(&["uuid", "id", "username", "password"])?)
                    .map_err(|_| Error::InvalidNode)?,
                flow: f.string(&["flow"])?,
            }
        }
        "vmess" => {
            let alter_id = f
                .string(&["alterId"])?
                .map(|s| uri::decimal(&s))
                .transpose()?;
            let aead = f.flag(&["aead"])?;
            if alter_id.is_some() && aead.is_some() {
                return Err(Error::ConflictingOptions);
            }
            Protocol::Vmess {
                uuid: Uuid::parse_str(&f.required(&["uuid", "id", "username", "password"])?)
                    .map_err(|_| Error::InvalidNode)?,
                security: f
                    .string(&["cipher", "method", "security"])?
                    .unwrap_or_else(|| "auto".into()),
                alter_id: alter_id.unwrap_or(if aead == Some(false) { 1 } else { 0 }),
            }
        }
        "hysteria2" => {
            let obfs = f.string(&["obfs"])?;
            let obfs_password = f.string(&["obfs-password"])?;
            if !matches!(
                (obfs.as_deref(), obfs_password.as_ref()),
                (None, None) | (Some("salamander"), Some(_))
            ) {
                return Err(Error::UnsupportedOption);
            }
            Protocol::Hysteria2 {
                password: f.required(&["password", "auth", "auth-str"])?,
                obfs_password,
                up_mbps: bandwidth(f.string(&["up", "up-mbps", "upmbps"])?)?,
                down_mbps: bandwidth(f.string(&["down", "down-mbps", "downmbps"])?)?,
            }
        }
        _ => return Err(Error::Protocol),
    };
    let tcp_only = f.flag(&["udp", "udp-relay"])? == Some(false);
    for names in [&["tfo", "fast-open"][..], &["mptcp"][..]] {
        if f.flag(names)? == Some(true) {
            return Err(Error::UnsupportedOption);
        }
    }
    let mut options = Options::default();
    for (to, from) in [
        ("tls", &["tls", "over-tls"][..]),
        (
            "sni",
            &[
                "sni",
                "servername",
                "server-name",
                "tls-name",
                "tls-host",
                "peer",
            ][..],
        ),
        (
            "insecure",
            &["skip-cert-verify", "insecure", "allowInsecure"][..],
        ),
        ("fp", &["client-fingerprint"][..]),
        ("security", &["security"][..]),
        ("pbk", &["public-key", "reality-base64-pubkey"][..]),
        ("sid", &["short-id", "reality-hex-shortid"][..]),
    ] {
        if let Some(value) = f.string(from)? {
            options.0.insert(to.into(), value);
        }
    }
    if let Some(verify) = f.flag(&["tls-verification"])?
        && options
            .0
            .insert("insecure".into(), (!verify).to_string())
            .is_some()
    {
        return Err(Error::ConflictingOptions);
    }
    if let Some(reality) = f.take(&["reality-opts"])? {
        let mut reality = Fields::new(reality.mapping()?)?;
        for (to, from) in [("pbk", "public-key"), ("sid", "short-id")] {
            if let Some(value) = reality.string(&[from])?
                && options.0.insert(to.into(), value).is_some()
            {
                return Err(Error::ConflictingOptions);
            }
        }
        reality.finish()?;
    }
    if options.0.contains_key("pbk") {
        if options
            .0
            .get("tls")
            .is_some_and(|v| v == "false" || v == "0")
        {
            return Err(Error::ConflictingOptions);
        }
        if let Some(enabled) = options.0.remove("tls")
            && enabled != "true"
            && enabled != "1"
        {
            return Err(Error::InvalidNode);
        }
        if options.0.get("security").is_some_and(|v| v != "reality") {
            return Err(Error::ConflictingOptions);
        }
        options.0.insert("security".into(), "reality".into());
    }
    let alpn = f.take(&["alpn"])?.map(|v| v.strings()).transpose()?;
    let mut tls = uri::tls(
        &mut options,
        matches!(kind, "anytls" | "trojan" | "hysteria2"),
    )?;
    if let Some(alpn) = alpn {
        tls.as_mut().ok_or(Error::ConflictingOptions)?.alpn = alpn;
    }

    let mut http_method = None;
    for (nested, expected) in [
        ("ws-opts", "ws"),
        ("h2-opts", "h2"),
        ("http-opts", "http"),
        ("grpc-opts", "grpc"),
        ("httpupgrade-opts", "httpupgrade"),
    ] {
        if let Some(value) = f.take(&[nested])? {
            let network = f.string(&["network"])?.ok_or(Error::ConflictingOptions)?;
            if network != expected {
                return Err(Error::ConflictingOptions);
            }
            f.insert("network", Value::scalar(network, value.line))?;
            let mut nested = Fields::new(value.mapping()?)?;
            if expected == "http" {
                http_method = nested.string(&["method"])?;
            }
            for (to, from) in [
                ("path", "path"),
                ("host", "host"),
                ("serviceName", "grpc-service-name"),
            ] {
                if let Some(value) = nested.take(&[from])? {
                    f.insert(to, value)?;
                }
            }
            if let Some(headers) = nested.take(&["headers"])? {
                let mut headers = Fields::new(headers.mapping()?)?;
                if let Some(value) = headers.take(&["Host"])? {
                    f.insert("host", value)?;
                }
                headers.finish()?;
            }
            nested.finish()?;
        }
    }
    for (to, from) in [
        ("type", "network"),
        ("path", "path"),
        ("host", "host"),
        ("serviceName", "serviceName"),
    ] {
        if let Some(value) = f.take(&[from])? {
            let values = value.strings()?;
            let s = if from == "host" {
                values.join(",")
            } else {
                if values.len() != 1 {
                    return Err(Error::UnsupportedOption);
                }
                values.into_iter().next().unwrap()
            };
            options.0.insert(to.into(), s);
        }
    }
    // Clash/client network=http is TCP HTTP camouflage. With TLS, sing-box's
    // HTTP transport switches to HTTP/2 instead; that is not an equivalent wire format.
    if tls.is_some() && options.0.get("type").is_some_and(|v| v == "http") {
        return Err(Error::UnsupportedOption);
    }
    let plain_http = options.0.get("type").is_some_and(|v| v == "http");
    let mut transport = uri::transport(&mut options, tls.is_some())?;
    if plain_http && let Some(super::Transport::Http { method, .. }) = &mut transport {
        *method = Some(http_method.unwrap_or_else(|| "GET".into()));
    }
    if !options.0.is_empty() {
        return Err(Error::UnsupportedOption);
    }
    f.finish()?;
    let node = Node {
        name,
        server,
        port,
        protocol,
        tls,
        transport,
        tcp_only,
    };
    node.validate()?;
    Ok(node)
}
fn bandwidth(value: Option<String>) -> Result<Option<u32>> {
    value
        .map(|s| uri::decimal(s.strip_suffix(" Mbps").unwrap_or(&s)))
        .transpose()
}
