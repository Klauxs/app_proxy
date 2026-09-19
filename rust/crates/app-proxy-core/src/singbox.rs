//! A private, deterministic configuration for one owned sing-box process.
//! This compiles configuration only; availability and network health are separate.
use crate::model::{Manifest, ManualProtocol, ProxySource, ValidationError};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;

pub const CONFIG_LIMIT: usize = 8 * 1024 * 1024;

/// Contains resolved credentials: deliberately no Debug, Display or Serialize.
pub struct CoreConfig {
    bytes: Vec<u8>,
    profiles: Vec<Uuid>,
}
impl CoreConfig {
    /// Only write to an owned, protected generation directory. Never log these bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub fn profiles(&self) -> &[Uuid] {
        &self.profiles
    }
}

pub fn compile(
    manifest: &Manifest,
    profiles: &[Uuid],
    mut secret: impl FnMut(Uuid) -> Result<String, ValidationError>,
) -> Result<CoreConfig, ValidationError> {
    manifest.validate()?;
    let profiles: Vec<_> = profiles
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if profiles.is_empty() {
        return Err(ValidationError("NO_ACTIVE_PROFILES"));
    }
    let mut inbounds = Vec::new();
    let mut outbounds = Vec::new();
    let mut rules = Vec::new();
    for id in &profiles {
        let profile = manifest
            .profiles
            .iter()
            .find(|p| p.id == *id)
            .ok_or(ValidationError("PROFILE_NOT_FOUND"))?;
        let incoming = format!("in-{id}");
        let outgoing = format!("out-{id}");
        inbounds.push(json!({"type":"http","tag":incoming,"listen":profile.endpoint.host.to_string(),"listen_port":profile.endpoint.port,"set_system_proxy":false}));
        let outbound = match &profile.source {
            ProxySource::Manual { nodes } => {
                let node = nodes
                    .iter()
                    .find(|n| n.id == profile.selected_node_id)
                    .ok_or(ValidationError("SELECTED_NODE_NOT_FOUND"))?;
                let mut outbound = json!({"type":match node.protocol { ManualProtocol::Http => "http", ManualProtocol::Socks5 => "socks" },"tag":outgoing,"server":node.host,"server_port":node.port});
                if matches!(node.protocol, ManualProtocol::Socks5) {
                    outbound["version"] = json!("5");
                    outbound["network"] = json!("tcp");
                }
                if let Some(credentials) = &node.credentials {
                    let password = secret(credentials.password_secret_id)?;
                    crate::model::validate_proxy_credentials(
                        &node.protocol,
                        &credentials.username,
                        &password,
                    )?;
                    outbound["username"] = json!(credentials.username);
                    outbound["password"] = json!(password);
                }
                outbound
            }
            ProxySource::Subscription { nodes, .. } => {
                let saved = nodes
                    .iter()
                    .find(|n| n.id == profile.selected_node_id)
                    .ok_or(ValidationError("SELECTED_NODE_NOT_FOUND"))?;
                let node = saved
                    .resolve(&secret(saved.secret_id)?)
                    .map_err(|_| ValidationError("INVALID_SUBSCRIPTION_SECRET"))?;
                node.outbound(&outgoing)
                    .map_err(|_| ValidationError("INVALID_SUBSCRIPTION_NODE"))?
            }
        };
        outbounds.push(outbound);
        rules.push(json!({"inbound":[incoming],"action":"route","outbound":outgoing}));
    }
    // Catch-all rejection prevents implicit first-outbound/default routing.
    // HTTP/SOCKS pass target domains to the selected upstream; the local resolver
    // is for establishing a connection to an upstream whose address is a domain.
    rules.push(json!({"action":"reject"}));
    let config: Value = json!({"log":{"disabled":true},
        "dns":{"servers":[{"type":"local","tag":"upstream-resolver"}]},
        "inbounds":inbounds,"outbounds":outbounds,
        "route":{"rules":rules,"default_domain_resolver":"upstream-resolver"}});
    let bytes =
        serde_json::to_vec(&config).map_err(|_| ValidationError("CORE_CONFIG_SERIALIZE_FAILED"))?;
    if bytes.len() > CONFIG_LIMIT {
        return Err(ValidationError("CORE_CONFIG_TOO_LARGE"));
    }
    Ok(CoreConfig { bytes, profiles })
}
