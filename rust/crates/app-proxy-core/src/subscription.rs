//! Subscription import data is secret-bearing, in-memory data, not a manifest
//! or arbitrary sing-box config. Diagnostics contain only categories and indexes.
mod node;
mod uri;
use base64::{
    Engine, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
pub use node::{Node, Protocol, Reality, Region, Tls, Transport};
use std::collections::HashSet;

pub const INPUT_LIMIT: usize = 8 * 1024 * 1024;
pub const NODE_LIMIT: usize = 4096;
const LINE_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("SUBSCRIPTION_EMPTY")]
    Empty,
    #[error("SUBSCRIPTION_TOO_LARGE")]
    TooLarge,
    #[error("SUBSCRIPTION_TOO_MANY_NODES")]
    TooManyNodes,
    #[error("SUBSCRIPTION_ENCODING_INVALID")]
    Encoding,
    #[error("SUBSCRIPTION_FORMAT_INVALID")]
    Format,
    #[error("SUBSCRIPTION_NODE_INVALID")]
    InvalidNode,
    #[error("SUBSCRIPTION_PROTOCOL_UNSUPPORTED")]
    Protocol,
    #[error("SUBSCRIPTION_OPTION_UNSUPPORTED")]
    UnsupportedOption,
    #[error("SUBSCRIPTION_OPTIONS_CONFLICT")]
    ConflictingOptions,
    #[error("SUBSCRIPTION_NAMES_DUPLICATE")]
    DuplicateNames,
}
type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, PartialEq, Eq)]
pub struct Issue {
    /// One-based line in the decoded source. No name, raw protocol or input text.
    pub source_index: usize,
    pub reason: Error,
}
// No Debug or Serialize: successful entries contain credentials.
pub struct Parsed {
    pub nodes: Vec<Node>,
    pub unsupported: Vec<Issue>,
}

/// Parse URI lines, optionally wrapped once in Base64 (standard or URL-safe,
/// padded or unpadded). Other subscription formats have separate adapters.
pub fn parse_uris(text: &str) -> Result<Parsed> {
    if text.len() > INPUT_LIMIT {
        return Err(Error::TooLarge);
    }
    let text = text.trim().trim_start_matches('\u{feff}');
    if text.is_empty() {
        return Err(Error::Empty);
    }
    let decoded;
    let text = if !text.contains("://") {
        let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        decoded = base64_text(&compact)?;
        if !decoded.contains("://") {
            return Err(Error::Format);
        }
        decoded.as_str()
    } else {
        text
    };
    let mut parsed = Parsed {
        nodes: Vec::new(),
        unsupported: Vec::new(),
    };
    let mut names = HashSet::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if parsed.nodes.len() + parsed.unsupported.len() >= NODE_LIMIT {
            return Err(Error::TooManyNodes);
        }
        let result = if line.len() > LINE_LIMIT {
            Err(Error::TooLarge)
        } else {
            uri::parse(line)
        };
        match result {
            Ok(node) => {
                if !names.insert(node.name.clone()) {
                    return Err(Error::DuplicateNames);
                }
                parsed.nodes.push(node);
            }
            Err(reason) => parsed.unsupported.push(Issue {
                source_index: index + 1,
                reason,
            }),
        }
    }
    if parsed.nodes.is_empty() && parsed.unsupported.is_empty() {
        return Err(Error::Empty);
    }
    Ok(parsed)
}

fn base64_bytes(value: &str) -> Result<Vec<u8>> {
    let config =
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent);
    GeneralPurpose::new(&alphabet::STANDARD, config)
        .decode(value)
        .or_else(|_| GeneralPurpose::new(&alphabet::URL_SAFE, config).decode(value))
        .map_err(|_| Error::Encoding)
}
fn base64_text(value: &str) -> Result<String> {
    String::from_utf8(base64_bytes(value)?).map_err(|_| Error::Encoding)
}
fn clean(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn host(value: &str) -> Result<String> {
    if !clean(value, 253) || value.chars().any(char::is_whitespace) || value.contains('%') {
        return Err(Error::InvalidNode);
    }
    if let Ok(ip) = value.parse::<std::net::IpAddr>() {
        return Ok(ip.to_string());
    }
    let host = url::Host::parse(value).map_err(|_| Error::InvalidNode)?;
    match host {
        url::Host::Domain(domain)
            if domain.split('.').all(|part| {
                !part.is_empty()
                    && part.len() <= 63
                    && !part.starts_with('-')
                    && !part.ends_with('-')
                    && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            }) =>
        {
            Ok(domain)
        }
        url::Host::Ipv4(ip) => Ok(ip.to_string()),
        url::Host::Ipv6(ip) => Ok(ip.to_string()),
        _ => Err(Error::InvalidNode),
    }
}

#[cfg(test)]
mod tests;
