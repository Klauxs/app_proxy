use super::{Error, INPUT_LIMIT, LINE_LIMIT, Parsed, Result, clash::Value, collect, fields, uri};
use std::collections::BTreeMap;

pub(super) fn parse(text: &str) -> Result<Parsed> {
    if text.len() > INPUT_LIMIT {
        return Err(Error::TooLarge);
    }
    let mut active = true;
    let mut entries = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if line.starts_with('[') {
            let section = line.strip_suffix(']').ok_or(Error::Format)?;
            active = matches!(
                section.to_ascii_lowercase().as_str(),
                "[proxy" | "[server_local"
            );
            continue;
        }
        if !active {
            continue;
        }
        if entries.len() >= super::NODE_LIMIT {
            return Err(Error::TooManyNodes);
        }
        entries.push((
            index + 1,
            if line.len() > LINE_LIMIT {
                Err(Error::TooLarge)
            } else {
                parse_line(line, index + 1)
            },
        ));
    }
    collect(entries)
}
fn parse_line(line: &str, index: usize) -> Result<super::Node> {
    if line.contains("://")
        && !line
            .split_once('=')
            .is_some_and(|(before, _)| !before.contains("://"))
    {
        return uri::parse(line);
    }
    let equal = separator(line, '=')?
        .into_iter()
        .next()
        .ok_or(Error::Format)?;
    let left = unquote(line[..equal].trim())?;
    let raw = &line[equal + 1..];
    let commas = separator(raw, ',')?;
    let mut parts = Vec::new();
    let mut start = 0;
    for end in commas.into_iter().chain(std::iter::once(raw.len())) {
        let part = raw[start..end].trim();
        if part.is_empty() {
            return Err(Error::Format);
        }
        parts.push(part);
        start = end + 1;
    }
    let first = unquote(parts.first().ok_or(Error::Format)?)?;
    let (kind, quantum) = if let Ok(kind) = fields::protocol(&first) {
        parts.remove(0);
        (kind, false)
    } else {
        (fields::protocol(&left)?, true)
    };
    let mut values = BTreeMap::new();
    let mut positional = Vec::new();
    let mut seen_options = false;
    for part in parts {
        if let Some(equal) = separator(part, '=')?.into_iter().next() {
            seen_options = true;
            let name = part[..equal].trim();
            if name.is_empty() {
                return Err(Error::Format);
            }
            put(&mut values, name, unquote(part[equal + 1..].trim())?, index)?;
        } else {
            if seen_options {
                return Err(Error::Format);
            }
            positional.push(unquote(part)?);
        }
    }
    let mut positional = positional.into_iter();
    let endpoint = positional.next().ok_or(Error::InvalidNode)?;
    let (server, port) = if endpoint.contains(':') {
        let (host, port) = uri::endpoint_parts(&endpoint, false)?;
        (host, port.to_string())
    } else {
        (endpoint, positional.next().ok_or(Error::InvalidNode)?)
    };
    put(&mut values, "type", kind.into(), index)?;
    put(&mut values, "server", server, index)?;
    put(&mut values, "port", port, index)?;
    if !quantum {
        put(&mut values, "name", left, index)?;
    }
    let mut remaining: Vec<_> = positional.collect();
    if (kind == "ss" || (kind == "vmess" && remaining.len() == 2)) && !remaining.is_empty() {
        if has(&values, &["cipher", "method", "encrypt-method", "security"]) {
            return Err(Error::ConflictingOptions);
        }
        put(&mut values, "method", remaining.remove(0), index)?;
    }
    if !remaining.is_empty() {
        if remaining.len() != 1
            || has(
                &values,
                &["password", "uuid", "id", "username", "auth", "auth-str"],
            )
        {
            return Err(Error::ConflictingOptions);
        }
        put(&mut values, "password", remaining.remove(0), index)?;
    }
    // These text-client keys express transport modes, unlike Hysteria obfs.
    if kind != "hysteria2"
        && let Some(obfs) = values.remove("obfs")
    {
        let obfs = obfs.string()?;
        if kind == "ss" {
            return Err(Error::UnsupportedOption);
        }
        match obfs.as_str() {
            "over-tls" => put(&mut values, "tls", "true".into(), index)?,
            "ws" => put(&mut values, "network", "ws".into(), index)?,
            "wss" => {
                put(&mut values, "network", "ws".into(), index)?;
                put(&mut values, "tls", "true".into(), index)?;
            }
            _ => return Err(Error::UnsupportedOption),
        }
        if let Some(host) = values.remove("obfshost") {
            let host = host.string()?;
            if obfs == "over-tls" {
                put(&mut values, "sni", host, index)?;
            } else {
                // For wss, the client uses this as the TLS server name too.
                if obfs == "wss" && !has(&values, &["sni", "tls-host", "server-name", "peer"]) {
                    put(&mut values, "sni", host.clone(), index)?;
                }
                put(&mut values, "host", host, index)?;
            }
        }
        if let Some(path) = values
            .remove("obfsuri")
            .or_else(|| values.remove("obfspath"))
        {
            put(&mut values, "path", path.string()?, index)?;
        }
    }
    if let Some(ws) = values.remove("ws") {
        match ws.string()?.as_str() {
            "true" | "1" => put(&mut values, "network", "ws".into(), index)?,
            "false" | "0" => {}
            _ => return Err(Error::InvalidNode),
        }
    }
    if let Some(path) = values.remove("wspath") {
        put(&mut values, "path", path.string()?, index)?;
    }
    if let Some(alpn) = values.remove("alpn") {
        let strings = alpn
            .string()?
            .split(',')
            .map(|s| Value::scalar(s.trim().into(), index))
            .collect();
        values.insert(
            "alpn".into(),
            Value {
                line: index,
                data: std::sync::Arc::new(super::clash::Data::Seq(strings)),
                depth: 1,
            },
        );
    }
    fields::node(values)
}
fn has(map: &BTreeMap<String, Value>, names: &[&str]) -> bool {
    names.iter().any(|n| map.contains_key(&fields::key(n)))
}
fn put(map: &mut BTreeMap<String, Value>, key: &str, value: String, line: usize) -> Result<()> {
    if map
        .insert(fields::key(key), Value::scalar(value, line))
        .is_some()
    {
        return Err(Error::ConflictingOptions);
    }
    Ok(())
}
// Locate delimiters outside quotes, preserving literal comma/equal/backslash in
// quoted credentials. Escape interpretation belongs only to unquote below.
fn separator(value: &str, delimiter: char) -> Result<Vec<usize>> {
    let mut quote = None;
    let mut escape = false;
    let mut positions = Vec::new();
    for (index, c) in value.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        if quote == Some('"') && c == '\\' {
            escape = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == delimiter {
            positions.push(index);
        }
    }
    if quote.is_some() || escape {
        return Err(Error::Format);
    }
    Ok(positions)
}
fn unquote(value: &str) -> Result<String> {
    if value.starts_with('"') {
        return serde_json::from_str::<String>(value).map_err(|_| Error::Format);
    }
    if let Some(value) = value.strip_prefix('\'') {
        let value = value.strip_suffix('\'').ok_or(Error::Format)?;
        let mut chars = value.chars();
        let mut decoded = String::new();
        while let Some(c) = chars.next() {
            if c == '\'' && chars.next() != Some('\'') {
                return Err(Error::Format);
            }
            decoded.push(c);
        }
        return Ok(decoded);
    }
    if value.contains(['\'', '"']) {
        return Err(Error::Format);
    }
    Ok(value.into())
}
