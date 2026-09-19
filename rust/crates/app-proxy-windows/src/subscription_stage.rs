//! Build a reviewable configuration request after lock-free download/parsing.
//! Staging publishes immutable secrets only; commit/core switching is separate.
use crate::{
    Error, Result,
    store::{self, Store},
};
use app_proxy_core::{
    model::*,
    registry::{self, ConfigAction, ConfigRequest, SubscriptionChanges, SubscriptionEdit},
    subscription::{self, Parsed, saved::SavedNode},
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub struct ImportRequest {
    pub request_id: Uuid,
    pub expected_revision: u64,
    pub profile_id: Uuid,
    pub name: String,
    pub endpoint: Endpoint,
    pub url: String,
    pub selected_name: String,
}

/// Retain/replay this exact request after a lost commit response. A new download
/// is a new operation, never a retry with different content under the same ID.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StagedSubscription {
    pub request: ConfigRequest,
    pub changes: SubscriptionChanges,
}

impl Store {
    pub fn stage_subscription_import(
        &mut self,
        input: &ImportRequest,
        parsed: &Parsed,
    ) -> Result<StagedSubscription> {
        subscription::source_url(&input.url)
            .map_err(|_| Error::Invalid("SUBSCRIPTION_URL_INVALID"))?;
        let mut secrets = vec![(input.request_id, input.url.clone())];
        let mut nodes = Vec::new();
        for node in &parsed.nodes {
            let (saved, encoded) = SavedNode::capture(
                derived_id(input.request_id, b"node", &node.name),
                derived_id(input.request_id, b"secret", &node.name),
                node,
            )
            .map_err(|_| Error::Invalid("INVALID_SUBSCRIPTION_NODE"))?;
            secrets.push((saved.secret_id, encoded));
            nodes.push(saved);
        }
        let selected_node_id = nodes
            .iter()
            .find(|n| n.name == input.selected_name)
            .ok_or(Error::Invalid("SELECTED_NODE_NOT_FOUND"))?
            .id;
        let changes = SubscriptionChanges {
            added: nodes.iter().map(|n| n.name.clone()).collect(),
            removed: vec![],
            unsupported: parsed.unsupported.len(),
        };
        let request = ConfigRequest {
            request_id: input.request_id,
            expected_revision: input.expected_revision,
            action: ConfigAction::AddProfile {
                profile: ProxyProfile {
                    id: input.profile_id,
                    name: input.name.clone(),
                    revision: 1,
                    kind: ProxyKind::Managed,
                    endpoint: input.endpoint.clone(),
                    selected_node_id,
                    source: ProxySource::Subscription {
                        url_secret_id: input.request_id,
                        revision: 1,
                        nodes,
                    },
                },
            },
        };
        self.stage_subscription_request(&request, &secrets)?;
        Ok(StagedSubscription { request, changes })
    }

    pub fn stage_subscription_refresh(
        &mut self,
        request_id: Uuid,
        profile_id: Uuid,
        expected_source_revision: u64,
        expected_url_secret_id: Uuid,
        parsed: &Parsed,
    ) -> Result<StagedSubscription> {
        // Source revision is captured before downloading. Other profiles/display
        // edits may commit meanwhile; snapshot the global revision only now.
        let manifest = self.load()?;
        let profile = manifest
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
        let ProxySource::Subscription {
            revision,
            url_secret_id,
            nodes: before,
        } = &profile.source
        else {
            return Err(Error::Invalid("SUBSCRIPTION_PROFILE_REQUIRED"));
        };
        if *revision != expected_source_revision || *url_secret_id != expected_url_secret_id {
            return Err(Error::Invalid("STALE_SUBSCRIPTION_SOURCE"));
        }
        let mut secrets = Vec::new();
        let mut nodes = Vec::new();
        for node in &parsed.nodes {
            let previous = before.iter().find(|old| old.name == node.name);
            if let Some(old) = previous {
                let resolved = old
                    .resolve(&self.read_secret(old.secret_id)?)
                    .map_err(|_| Error::Invalid("INVALID_SUBSCRIPTION_SECRET"))?;
                if resolved == *node {
                    nodes.push(old.clone());
                    continue;
                }
            }
            let (saved, encoded) = SavedNode::capture(
                previous.map_or_else(|| derived_id(request_id, b"node", &node.name), |old| old.id),
                derived_id(request_id, b"secret", &node.name),
                node,
            )
            .map_err(|_| Error::Invalid("INVALID_SUBSCRIPTION_NODE"))?;
            secrets.push((saved.secret_id, encoded));
            nodes.push(saved);
        }
        let changes = SubscriptionChanges {
            added: nodes
                .iter()
                .filter(|n| !before.iter().any(|old| old.name == n.name))
                .map(|n| n.name.clone())
                .collect(),
            removed: before
                .iter()
                .filter(|old| !nodes.iter().any(|n| old.name == n.name))
                .map(|n| n.name.clone())
                .collect(),
            unsupported: parsed.unsupported.len(),
        };
        let request = ConfigRequest {
            request_id,
            expected_revision: manifest.revision,
            action: ConfigAction::EditSubscriptionProfile {
                profile_id,
                edit: SubscriptionEdit::Refresh {
                    expected_source_revision,
                    expected_url_secret_id,
                    nodes,
                },
            },
        };
        self.stage_subscription_request(&request, &secrets)?;
        Ok(StagedSubscription { request, changes })
    }

    fn stage_subscription_request(
        &mut self,
        request: &ConfigRequest,
        secrets: &[(Uuid, String)],
    ) -> Result<()> {
        // Validate size/IDs/version/name/selection before writing any secret.
        store::encode(request, crate::config_transaction::REQUEST_LIMIT)?;
        if self.replay_config(request)?.is_some() {
            // IDs are deterministic, but a terminal retry must still represent
            // the same input. Never fill a missing secret after replaying a result.
            for (id, value) in secrets {
                if self.read_secret(*id)? != *value {
                    return Err(Error::Invalid("SECRET_ID_CONFLICT"));
                }
            }
            return Ok(());
        }
        self.ensure_core_update_idle()?;
        let (target, _) =
            registry::apply(self.load()?, request).map_err(|e| Error::Invalid(e.0))?;
        store::encode(&target, MANIFEST_LIMIT)?;
        for (id, value) in secrets {
            self.put_secret_once(*id, value)?;
        }
        self.validate(&target)
    }
}

fn derived_id(request: Uuid, domain: &[u8], name: &str) -> Uuid {
    let mut hash = Sha256::new();
    hash.update(b"app-proxy-subscription-v1\0");
    hash.update(request.as_bytes());
    hash.update(domain);
    hash.update([0]);
    hash.update(name.as_bytes());
    let bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
    uuid::Builder::from_custom_bytes(bytes).into_uuid()
}
