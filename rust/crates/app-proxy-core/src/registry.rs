//! Configuration-only edits. Installation identity and external integration cleanup
//! must be resolved by the coordinator before invoking a durable store transaction.
use crate::model::*;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

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
    BindInstance {
        instance_id: Uuid,
        network: NetworkBinding,
        guard: Option<Desired>,
    },
    RemoveInstance {
        instance_id: Uuid,
    },
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
            if desired == Desired::Disabled
                && manifest
                    .integrations
                    .ifeo
                    .iter()
                    .any(|i| i.default_instance_id == *instance_id)
            {
                return Err(ValidationError("INTEGRATION_CLEANUP_REQUIRED"));
            }
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
                || manifest
                    .integrations
                    .ifeo
                    .iter()
                    .any(|i| i.default_instance_id == *instance_id)
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
