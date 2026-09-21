//! Desktop paths and launch targets are derived here, never supplied by RPC.
use crate::configuration::Configuration;
use app_proxy_core::model::{ApplicationLocator, InstanceData};
use app_proxy_windows::{
    Error, Result, installation,
    shortcuts::{
        self as native, Spec, icons,
        journal::{Action, Plan, Request, Status},
    },
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstanceStatus {
    pub instance_id: Uuid,
    pub revision: u64,
    pub integration: Option<Registration>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    pub request: Request,
    pub status: Status,
}

pub fn status(configuration: &Configuration, instance_id: Uuid) -> Result<InstanceStatus> {
    let store = configuration.lock()?;
    let manifest = store.load()?;
    if !manifest.instances.iter().any(|i| i.id == instance_id) {
        return Err(Error::Invalid("INSTANCE_NOT_FOUND"));
    }
    let integration = store
        .instance_shortcut(instance_id)?
        .map(|(request, status)| Registration { request, status });
    if integration.is_none()
        && manifest
            .integrations
            .shortcuts
            .iter()
            .any(|s| s.instance_id == instance_id)
    {
        return Err(Error::Invalid("SHORTCUT_OWNERSHIP_UNAVAILABLE"));
    }
    Ok(InstanceStatus {
        instance_id,
        revision: manifest.revision,
        integration,
    })
}

struct Assets {
    desktop: PathBuf,
    host: PathBuf,
    icon: Vec<u8>,
}
fn prepare(locator: &ApplicationLocator) -> Result<Assets> {
    let application = installation::resolve(locator)?;
    let icon = icons::extract(application.executable())?;
    application.verify_current()?;
    let host = std::env::current_exe()?.with_file_name("app-proxy-host.exe");
    let installed_host = installation::resolve(&ApplicationLocator::Exe { path: host.clone() })?;
    installed_host.verify_current()?;
    Ok(Assets {
        desktop: native::desktop()?,
        host,
        icon,
    })
}

pub fn apply(configuration: &Configuration, root: &Path, request: &Request) -> Result<Status> {
    apply_with(configuration, root, request, prepare)
}
fn apply_with(
    configuration: &Configuration,
    root: &Path,
    request: &Request,
    prepare: impl FnOnce(&ApplicationLocator) -> Result<Assets>,
) -> Result<Status> {
    if request.action != Action::Create && request.expected_creation.is_none() {
        return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
    }
    let snapshot = {
        let mut store = configuration.lock()?;
        // Replay must verify the entire payload before any installation access.
        if store.shortcut_request_status(request.id)?.is_some() || request.action != Action::Create
        {
            return store.apply_shortcut(request, None);
        }
        store.recover_config_requests()?;
        let snapshot = store.load()?;
        preflight(&store, &snapshot, request)?;
        snapshot
    };
    let instance = snapshot
        .instances
        .iter()
        .find(|i| i.id == request.instance_id)
        .unwrap();
    let application = snapshot
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .ok_or(Error::Invalid("APPLICATION_NOT_FOUND"))?;
    // OS/package/resource queries hold no configuration lock.
    let assets = prepare(&application.locator)?;
    let name = match instance.data {
        InstanceData::Original {} => native::original_filename(&application.name)?,
        InstanceData::Isolated { .. } => native::filename(&instance.name, instance.id)?,
    };
    let path = assets.desktop.join(name);
    let mut store = configuration.lock()?;
    // A simultaneous identical request may have completed during preparation.
    if store.shortcut_request_status(request.id)?.is_some() {
        return store.apply_shortcut(request, None);
    }
    store.recover_config_requests()?;
    preflight(&store, &store.load()?, request)?;
    let icon = icons::cache_bytes(&store, &assets.icon)?;
    store.apply_shortcut(
        request,
        Some(Plan {
            path,
            spec: Spec {
                store_id: snapshot.store_id,
                instance_id: request.instance_id,
                home: root.into(),
                host: assets.host,
                icon,
            },
        }),
    )
}
fn preflight(
    store: &app_proxy_windows::store::Store,
    manifest: &app_proxy_core::model::Manifest,
    request: &Request,
) -> Result<()> {
    if request.id.is_nil()
        || request.instance_id.is_nil()
        || request.expected_revision == 0
        || request.expected_creation.is_some()
    {
        return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
    }
    if store.config_request_status(request.id)?.is_some()
        || store.core_request_status(request.id)?.is_some()
        || store.launch_request(request.id)?.is_some()
        || store.login_request_status(request.id)?.is_some()
    {
        return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
    }
    if manifest.revision != request.expected_revision {
        return Err(Error::Invalid("STALE_MANIFEST_REVISION"));
    }
    if !manifest
        .instances
        .iter()
        .any(|i| i.id == request.instance_id)
    {
        return Err(Error::Invalid("INSTANCE_NOT_FOUND"));
    }
    if store.instance_shortcut(request.instance_id)?.is_some()
        || manifest
            .integrations
            .shortcuts
            .iter()
            .any(|s| s.instance_id == request.instance_id)
    {
        return Err(Error::Invalid("SHORTCUT_ALREADY_REGISTERED"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
