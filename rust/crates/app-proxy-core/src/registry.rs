//! Configuration-only edits. Installation identity and external integration cleanup
//! must be resolved by the coordinator before invoking a durable store transaction.
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

mod instance_edit;
pub use instance_edit::{EnvironmentAssignment, EnvironmentEdit, InstanceEdit};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRequest {
    pub request_id: Uuid,
    pub expected_revision: u64,
    pub action: ConfigAction,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigAction {
    AddApplication {
        application: Application,
    },
    AddProfile {
        profile: ProxyProfile,
    },
    CreateManualProfile {
        profile_id: Uuid,
        name: String,
        endpoint: Endpoint,
        node: ManualProxyInput,
    },
    UpdateManualProfile {
        profile_id: Uuid,
        node: ManualProxyInput,
    },
    EditSubscriptionProfile {
        profile_id: Uuid,
        edit: SubscriptionEdit,
    },
    RenameProfile {
        profile_id: Uuid,
        name: String,
    },
    RemoveProfile {
        profile_id: Uuid,
    },
    CreateInstance {
        instance: NewInstance,
    },
    CloneInstance {
        source_id: Uuid,
        instance_id: Uuid,
        name: String,
        storage: NewStorage,
        network: Option<NetworkBinding>,
        guard: Option<Desired>,
    },
    RenameInstance {
        instance_id: Uuid,
        name: String,
    },
    EditInstance {
        instance_id: Uuid,
        edit: InstanceEdit,
    },
    BindInstance {
        instance_id: Uuid,
        network: NetworkBinding,
        guard: Option<Desired>,
    },
    RemoveInstance {
        instance_id: Uuid,
    },
}

/// References only. Downloaded bodies, source URLs and credentials never enter
/// the durable configuration request or its diagnostic receipt.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SubscriptionEdit {
    Refresh {
        expected_source_revision: u64,
        expected_url_secret_id: Uuid,
        nodes: Vec<crate::subscription::saved::SavedNode>,
    },
    Select {
        expected_source_revision: u64,
        node_id: Uuid,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SubscriptionChanges {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub unsupported: usize,
}

fn edit_subscription(
    profile: &mut ProxyProfile,
    edit: &SubscriptionEdit,
) -> Result<(), ValidationError> {
    let ProxySource::Subscription {
        revision,
        url_secret_id,
        nodes,
    } = &mut profile.source
    else {
        return Err(ValidationError("SUBSCRIPTION_PROFILE_REQUIRED"));
    };
    let expected = match edit {
        SubscriptionEdit::Refresh {
            expected_source_revision,
            ..
        }
        | SubscriptionEdit::Select {
            expected_source_revision,
            ..
        } => *expected_source_revision,
    };
    if *revision != expected {
        return Err(ValidationError("STALE_SUBSCRIPTION_SOURCE"));
    }
    match edit {
        SubscriptionEdit::Refresh {
            expected_url_secret_id,
            nodes: next,
            ..
        } => {
            if url_secret_id != expected_url_secret_id {
                return Err(ValidationError("STALE_SUBSCRIPTION_SOURCE"));
            }
            let selected = nodes
                .iter()
                .find(|n| n.id == profile.selected_node_id)
                .ok_or(ValidationError("SELECTED_NODE_NOT_FOUND"))?;
            if !next.iter().any(|n| n.name == selected.name) {
                return Err(ValidationError("SUBSCRIPTION_SELECTED_NODE_REMOVED"));
            }
            // Existing identities stay attached to the same names. This prevents
            // a refresh from silently reassigning an active node's identity.
            for node in next {
                if nodes
                    .iter()
                    .any(|old| (old.id == node.id) != (old.name == node.name))
                {
                    return Err(ValidationError("SUBSCRIPTION_NODE_ID_CHANGED"));
                }
            }
            *revision = next_revision(*revision)?;
            *nodes = next.clone();
        }
        SubscriptionEdit::Select { node_id, .. } => {
            if !nodes.iter().any(|node| node.id == *node_id) {
                return Err(ValidationError("SELECTED_NODE_NOT_FOUND"));
            }
            profile.selected_node_id = *node_id;
        }
    }
    profile.revision = next_revision(profile.revision)?;
    Ok(())
}

/// Transient request input. Passwords are stored separately from the manifest
/// and durable request records; never derive Debug for these request types.
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualProxyInput {
    pub protocol: ManualProtocol,
    pub host: String,
    pub port: u16,
    pub credentials: Option<ProxyCredentialInput>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyCredentialInput {
    pub username: String,
    pub password: String,
}

impl ManualProxyInput {
    fn node(&self, id: Uuid, secret_id: Uuid) -> Result<ManualNode, ValidationError> {
        if self
            .credentials
            .as_ref()
            .is_some_and(|c| c.password.contains('\0') || c.password.len() > 32768)
        {
            return Err(ValidationError("INVALID_PROXY_PASSWORD"));
        }
        if let Some(c) = &self.credentials {
            validate_proxy_credentials(&self.protocol, &c.username, &c.password)?;
        }
        Ok(ManualNode {
            id,
            name: "手动代理".into(),
            protocol: self.protocol.clone(),
            host: self.host.clone(),
            port: self.port,
            credentials: self.credentials.as_ref().map(|c| Credentials {
                username: c.username.clone(),
                password_secret_id: secret_id,
            }),
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewInstance {
    pub id: Uuid,
    pub application_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub data: NewData,
    pub network: NetworkBinding,
    pub guard: Option<Desired>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: SavedEnvironment,
    #[serde(default = "application_cwd")]
    pub cwd: WorkingDirectory,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NewData {
    Original {},
    Isolated { storage: NewStorage },
}
impl Default for NewData {
    fn default() -> Self {
        Self::Original {}
    }
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NewStorage {
    Store,
    PackageLocalState,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigReceipt {
    pub entity_id: Uuid,
    pub revision: u64,
}

fn application_cwd() -> WorkingDirectory {
    WorkingDirectory::Application {}
}

pub fn apply(
    mut manifest: Manifest,
    request: &ConfigRequest,
) -> Result<(Manifest, ConfigReceipt), ValidationError> {
    manifest.validate()?;
    if request.request_id.is_nil() {
        return Err(ValidationError("INVALID_REQUEST_ID"));
    }
    if manifest.revision != request.expected_revision {
        return Err(ValidationError("STALE_MANIFEST_REVISION"));
    }
    let revision = manifest
        .revision
        .checked_add(1)
        .ok_or(ValidationError("REVISION_EXHAUSTED"))?;
    let entity_id = match &request.action {
        ConfigAction::AddApplication { application } => {
            if manifest.applications.iter().any(|a| {
                a.locator == application.locator && a.template_ref == application.template_ref
            }) {
                return Err(ValidationError("APPLICATION_ALREADY_REGISTERED"));
            }
            if application.revision != 1 {
                return Err(ValidationError("NEW_ENTITY_REVISION_REQUIRED"));
            }
            manifest.applications.push(Application {
                id: application.id,
                name: application.name.clone(),
                revision: 1,
                locator: application.locator.clone(),
                template_ref: application.template_ref,
            });
            application.id
        }
        ConfigAction::AddProfile { profile } => {
            if profile.revision != 1 {
                return Err(ValidationError("NEW_ENTITY_REVISION_REQUIRED"));
            }
            manifest.profiles.push(profile.clone());
            profile.id
        }
        ConfigAction::CreateManualProfile {
            profile_id,
            name,
            endpoint,
            node,
        } => {
            let node = node.node(*profile_id, request.request_id)?;
            manifest.profiles.push(ProxyProfile {
                id: *profile_id,
                name: name.clone(),
                revision: 1,
                kind: ProxyKind::Managed,
                endpoint: endpoint.clone(),
                selected_node_id: node.id,
                source: ProxySource::Manual { nodes: vec![node] },
            });
            *profile_id
        }
        ConfigAction::UpdateManualProfile { profile_id, node } => {
            let target = profile_mut(&mut manifest, *profile_id)?;
            let ProxySource::Manual { nodes } = &target.source else {
                return Err(ValidationError("MANUAL_PROFILE_REQUIRED"));
            };
            if nodes.len() != 1 {
                return Err(ValidationError("SINGLE_MANUAL_NODE_REQUIRED"));
            }
            let node = node.node(target.selected_node_id, request.request_id)?;
            target.revision = next_revision(target.revision)?;
            target.source = ProxySource::Manual { nodes: vec![node] };
            *profile_id
        }
        ConfigAction::EditSubscriptionProfile { profile_id, edit } => {
            edit_subscription(profile_mut(&mut manifest, *profile_id)?, edit)?;
            *profile_id
        }
        ConfigAction::RenameProfile { profile_id, name } => {
            let target = profile_mut(&mut manifest, *profile_id)?;
            target.revision = next_revision(target.revision)?;
            target.name = name.clone();
            *profile_id
        }
        ConfigAction::RemoveProfile { profile_id } => {
            profile_mut(&mut manifest, *profile_id)?;
            let binding = NetworkBinding::Profile {
                profile_id: *profile_id,
            };
            if manifest.instances.iter().any(|i| i.network == binding)
                || manifest.settings.download_network == binding
            {
                return Err(ValidationError("PROFILE_IN_USE"));
            }
            manifest.profiles.retain(|p| p.id != *profile_id);
            *profile_id
        }
        ConfigAction::CreateInstance { instance } => {
            let app = application(&manifest, instance.application_id)?;
            let data = match instance.data {
                NewData::Original {} => InstanceData::Original {},
                NewData::Isolated { storage } => {
                    isolated_data(&manifest, app, instance.id, storage)?
                }
            };
            let guard = guard_for(app.template_ref, instance.network, instance.guard);
            manifest.instances.push(Instance {
                id: instance.id,
                application_id: instance.application_id,
                name: instance.name.clone(),
                revision: 1,
                data,
                args: instance.args.clone(),
                env: instance.env.clone(),
                cwd: instance.cwd.clone(),
                network: instance.network,
                guard: GuardConfig {
                    desired: guard,
                    policy: GuardPolicy::StopUnproxied,
                },
            });
            instance.id
        }
        ConfigAction::CloneInstance {
            source_id,
            instance_id,
            name,
            storage,
            network,
            guard,
        } => {
            let source = instance(&manifest, *source_id)?;
            let app = application(&manifest, source.application_id)?;
            let network = network.unwrap_or(source.network);
            let copy = Instance {
                id: *instance_id,
                application_id: source.application_id,
                name: name.clone(),
                revision: 1,
                data: isolated_data(&manifest, app, *instance_id, *storage)?,
                args: source.args.clone(),
                env: source.env.clone(),
                cwd: source.cwd.clone(),
                network,
                guard: GuardConfig {
                    desired: guard_for(app.template_ref, network, *guard),
                    policy: GuardPolicy::StopUnproxied,
                },
            };
            manifest.instances.push(copy);
            *instance_id
        }
        ConfigAction::RenameInstance { instance_id, name } => {
            let target = instance_mut(&mut manifest, *instance_id)?;
            target.revision = target
                .revision
                .checked_add(1)
                .ok_or(ValidationError("REVISION_EXHAUSTED"))?;
            target.name = name.clone();
            *instance_id
        }
        ConfigAction::EditInstance { instance_id, edit } => {
            let target = instance_mut(&mut manifest, *instance_id)?;
            edit.apply(target)?;
            target.revision = next_revision(target.revision)?;
            *instance_id
        }
        ConfigAction::BindInstance {
            instance_id,
            network,
            guard,
        } => {
            let previous = instance(&manifest, *instance_id)?;
            let desired = guard.unwrap_or(if matches!(network, NetworkBinding::Direct {}) {
                Desired::Disabled
            } else {
                previous.guard.desired
            });
            let target = instance_mut(&mut manifest, *instance_id)?;
            target.revision = target
                .revision
                .checked_add(1)
                .ok_or(ValidationError("REVISION_EXHAUSTED"))?;
            target.network = *network;
            target.guard.desired = desired;
            *instance_id
        }
        ConfigAction::RemoveInstance { instance_id } => {
            instance(&manifest, *instance_id)?;
            if manifest
                .integrations
                .shortcuts
                .iter()
                .any(|s| s.instance_id == *instance_id)
            {
                return Err(ValidationError("INTEGRATION_CLEANUP_REQUIRED"));
            }
            manifest.instances.retain(|i| i.id != *instance_id);
            *instance_id
        }
    };
    // Global revision belongs to the atomic store commit, not this pure edit.
    manifest.validate()?;
    Ok((
        manifest,
        ConfigReceipt {
            entity_id,
            revision,
        },
    ))
}

fn next_revision(revision: u64) -> Result<u64, ValidationError> {
    revision
        .checked_add(1)
        .ok_or(ValidationError("REVISION_EXHAUSTED"))
}
fn profile_mut(manifest: &mut Manifest, id: Uuid) -> Result<&mut ProxyProfile, ValidationError> {
    manifest
        .profiles
        .iter_mut()
        .find(|p| p.id == id)
        .ok_or(ValidationError("PROFILE_NOT_FOUND"))
}

fn application(manifest: &Manifest, id: Uuid) -> Result<&Application, ValidationError> {
    manifest
        .applications
        .iter()
        .find(|a| a.id == id)
        .ok_or(ValidationError("APPLICATION_NOT_FOUND"))
}
fn instance(manifest: &Manifest, id: Uuid) -> Result<&Instance, ValidationError> {
    manifest
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or(ValidationError("INSTANCE_NOT_FOUND"))
}
fn instance_mut(manifest: &mut Manifest, id: Uuid) -> Result<&mut Instance, ValidationError> {
    manifest
        .instances
        .iter_mut()
        .find(|i| i.id == id)
        .ok_or(ValidationError("INSTANCE_NOT_FOUND"))
}
fn guard_for(template: Template, network: NetworkBinding, desired: Option<Desired>) -> Desired {
    desired.unwrap_or(
        if matches!(network, NetworkBinding::Profile { .. })
            && matches!(template, Template::Codex | Template::Claude)
        {
            Desired::Enabled
        } else {
            Desired::Disabled
        },
    )
}
fn isolated_data(
    manifest: &Manifest,
    app: &Application,
    id: Uuid,
    storage: NewStorage,
) -> Result<InstanceData, ValidationError> {
    if !app.template_ref.supports_isolation() {
        return Err(ValidationError("ISOLATION_UNSUPPORTED"));
    }
    let relative_path = PathBuf::from("instances").join(id.to_string());
    let location = match storage {
        NewStorage::Store => StorageLocation::Store { relative_path },
        NewStorage::PackageLocalState => {
            let ApplicationLocator::Msix { family_name, .. } = &app.locator else {
                return Err(ValidationError("PACKAGE_STORAGE_REQUIRES_MSIX"));
            };
            StorageLocation::PackageLocalState {
                family_name: family_name.clone(),
                namespace: manifest.store_id.to_string(),
                relative_path,
            }
        }
    };
    Ok(InstanceData::Isolated { location })
}
