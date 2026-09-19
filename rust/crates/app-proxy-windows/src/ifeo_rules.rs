//! Machine IFEO registration primitives. The foreground installer must finish
//! entry/continuation compatibility checks before calling install. No CLI calls
//! these writes yet. A protected journal precedes every activation/removal.
use crate::{Error, Result, guard_deployment::Deployment, identity, installation};
use app_proxy_core::{
    FileIdentity,
    model::{Desired, InstanceData, Manifest, NetworkBinding},
};
use registry::{Key, Value};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path, PathBuf, Prefix};
use uuid::Uuid;
use windows_sys::Win32::System::Registry::*;

mod journal;
use journal::Roots;
mod registry;
mod security;
const IFEO: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution Options";
const PRODUCT: &str = r"SOFTWARE\AppProxyRust";
const FORMAT: &str = "app-proxy-rust-ifeo-v1";

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    format: String,
    pub id: Uuid,
    pub store_id: Uuid,
    pub owner_sid: String,
    pub application_id: Uuid,
    pub instance_id: Uuid,
    pub deployment_generation: Uuid,
    pub target: PathBuf,
    pub target_image: FileIdentity,
    pub package_full_name: Option<String>,
    pub host: PathBuf,
    pub host_image: FileIdentity,
}
impl Registration {
    fn validate(&self) -> Result<()> {
        if self.format != FORMAT
            || [
                self.id,
                self.store_id,
                self.application_id,
                self.instance_id,
                self.deployment_generation,
            ]
            .iter()
            .any(Uuid::is_nil)
            || self.owner_sid.is_empty()
        {
            return Err(Error::Invalid("IFEO_INVALID_RECORD"));
        }
        dos_path(&self.target)?;
        dos_path(&self.host)?;
        if !self
            .host
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("app-proxy-host.exe"))
        {
            return Err(Error::Invalid("IFEO_INVALID_HOST"));
        }
        Ok(())
    }
    fn basename(&self) -> Result<String> {
        self.target
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_owned)
            .ok_or(Error::Invalid("IFEO_INVALID_TARGET"))
    }
    fn filter(&self) -> String {
        format!("AppProxyRust-{}", self.id)
    }
    fn debugger(&self) -> Result<String> {
        Ok(format!(
            "\"{}\" ifeo-entry --registration {} --",
            dos_path(&self.host)?,
            self.id
        ))
    }
}

/// Elevated platform operation, not an end-user enable command. The snapshot
/// is an immutable foreground installation input; the entry will revalidate it.
pub fn install(
    deployment: &Deployment,
    manifest: &Manifest,
    instance: Uuid,
    registration: Uuid,
) -> Result<Registration> {
    identity::assert_elevated_user()?;
    manifest
        .validate()
        .map_err(|_| Error::Invalid("IFEO_INVALID_CONFIGURATION"))?;
    if manifest.store_id != deployment.store_id()
        || manifest.owner_sid != identity::current()?.user_sid
    {
        return Err(Error::Invalid("IFEO_OWNER_MISMATCH"));
    }
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == instance)
        .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
    if !matches!(instance.data, InstanceData::Original {})
        || instance.guard.desired != Desired::Enabled
        || !matches!(instance.network, NetworkBinding::Profile { .. })
    {
        return Err(Error::Invalid("IFEO_MANAGED_ORIGINAL_REQUIRED"));
    }
    let app = manifest
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .ok_or(Error::Invalid("APPLICATION_NOT_FOUND"))?;
    let target = installation::resolve(&app.locator)?;
    let record = Registration {
        format: FORMAT.into(),
        id: registration,
        store_id: manifest.store_id,
        owner_sid: manifest.owner_sid.clone(),
        application_id: app.id,
        instance_id: instance.id,
        deployment_generation: deployment.generation(),
        target: target.executable().into(),
        target_image: target.image().clone(),
        package_full_name: target.package().map(|p| p.full_name.clone()),
        host: deployment.host_path().into(),
        host_image: deployment.host_image().clone(),
    };
    record.validate()?;
    target.verify_current()?;
    Roots::machine().install(&record)?;
    verify_registered(record.id)
}

/// Used by the ordinary entry to locate protected routing metadata. An ID is
/// only an index: ownership, exact rule contents and protected ACLs are checked.
pub fn read_registration(id: Uuid) -> Result<Registration> {
    let record = Roots::machine().read(id)?;
    if record.owner_sid != identity::current()?.user_sid {
        return Err(Error::Invalid("IFEO_OWNER_MISMATCH"));
    }
    Ok(record)
}
pub fn verify_registered(id: Uuid) -> Result<Registration> {
    let record = read_registration(id)?;
    Roots::machine().verify(&record)?;
    let deployment = Deployment::open(record.store_id, record.deployment_generation)?;
    if deployment.host_path() != record.host || deployment.host_image() != &record.host_image {
        return Err(Error::Invalid("IFEO_HOST_CHANGED"));
    }
    if identity::file_identity(&record.target)? != record.target_image {
        return Err(Error::Invalid("IFEO_TARGET_CHANGED"));
    }
    Ok(record)
}
/// Recovery does not require the portable coordinator or deployed host to work.
/// It refuses changed/foreign registry contents instead of overwriting them.
pub fn remove(id: Uuid) -> Result<()> {
    identity::assert_elevated_user()?;
    let record = Roots::machine().read_for_removal(id)?;
    if record.owner_sid != identity::current()?.user_sid {
        return Err(Error::Invalid("IFEO_OWNER_MISMATCH"));
    }
    Roots::machine().remove(&record)
}

fn dos_path(path: &Path) -> Result<String> {
    let mut parts = path.components();
    if !matches!(parts.next(), Some(Component::Prefix(p)) if matches!(p.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)))
        || parts.next() != Some(Component::RootDir)
        || parts.any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(Error::Invalid("IFEO_LOCAL_PATH_REQUIRED"));
    }
    let text = path
        .to_str()
        .ok_or(Error::Invalid("IFEO_UNICODE_PATH_REQUIRED"))?;
    let text = text.strip_prefix(r"\\?\").unwrap_or(text);
    if text.contains(['\0', '"', '%'])
        || text.split(['\\', '/']).any(|p| p.ends_with(['.', ' ']))
        || !path
            .extension()
            .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
    {
        return Err(Error::Invalid("IFEO_INVALID_EXECUTABLE"));
    }
    Ok(text.replace('/', "\\"))
}

#[cfg(test)]
mod tests;
