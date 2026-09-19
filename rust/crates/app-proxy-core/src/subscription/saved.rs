//! Manifest metadata and the typed secret document it references. Full connection
//! fields live in the protected secret store, never in catalog/request diagnostics.
use super::{Error, Node, Protocol, Result, Tls, Transport};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

const DOCUMENT_LIMIT: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProtocolKind {
    AnyTls,
    Vless,
    Vmess,
    Shadowsocks,
    Trojan,
    Hysteria2,
}
impl ProtocolKind {
    pub fn of(protocol: &Protocol) -> Self {
        match protocol {
            Protocol::AnyTls { .. } => Self::AnyTls,
            Protocol::Vless { .. } => Self::Vless,
            Protocol::Vmess { .. } => Self::Vmess,
            Protocol::Shadowsocks { .. } => Self::Shadowsocks,
            Protocol::Trojan { .. } => Self::Trojan,
            Protocol::Hysteria2 { .. } => Self::Hysteria2,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedNode {
    pub id: Uuid,
    pub name: String,
    pub protocol: ProtocolKind,
    pub server: String,
    pub port: u16,
    pub secret_id: Uuid,
}
impl SavedNode {
    /// Caller publishes the returned secret before committing this metadata.
    /// IDs must be stable for a retry; an existing secret must never be overwritten.
    pub fn capture(id: Uuid, secret_id: Uuid, node: &Node) -> Result<(Self, String)> {
        node.validate()?;
        let saved = Self {
            id,
            name: node.name.clone(),
            protocol: ProtocolKind::of(&node.protocol),
            server: node.server.clone(),
            port: node.port,
            secret_id,
        };
        saved.validate()?;
        let encoded = serde_json::to_string(&Document {
            version: 1,
            node: node.clone(),
        })
        .map_err(|_| Error::InvalidNode)?;
        if encoded.len() > DOCUMENT_LIMIT {
            return Err(Error::TooLarge);
        }
        Ok((saved, encoded))
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.is_nil()
            || self.secret_id.is_nil()
            || !super::clean(&self.name, 256)
            || self.port == 0
        {
            return Err(Error::InvalidNode);
        }
        super::host(&self.server)?;
        Ok(())
    }

    pub fn resolve(&self, encoded: &str) -> Result<Node> {
        self.validate()?;
        if encoded.len() > DOCUMENT_LIMIT {
            return Err(Error::TooLarge);
        }
        let document: Document = serde_json::from_str(encoded).map_err(|_| Error::InvalidNode)?;
        let node = document.node;
        if document.version != 1
            || node.name != self.name
            || node.server != self.server
            || node.port != self.port
            || ProtocolKind::of(&node.protocol) != self.protocol
        {
            return Err(Error::InvalidNode);
        }
        node.validate()?;
        Ok(node)
    }
}

// Private serde adapters keep Node and Protocol non-serializable in normal DTOs.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u32,
    #[serde(with = "NodeDocument")]
    node: Node,
}

#[derive(Serialize, Deserialize)]
#[serde(remote = "Node", deny_unknown_fields)]
struct NodeDocument {
    name: String,
    server: String,
    port: u16,
    #[serde(with = "ProtocolDocument")]
    protocol: Protocol,
    tls: Option<Tls>,
    transport: Option<Transport>,
    tcp_only: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(
    remote = "Protocol",
    tag = "type",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum ProtocolDocument {
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
