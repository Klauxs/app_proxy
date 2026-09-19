//! Pure template expansion; this does not verify proxy readiness or authorize spawn.
use crate::EnvPatch;
use crate::model::{
    self, EnvValue, InstanceData, Manifest, NetworkBinding, Template, ValidationError,
    WorkingDirectory,
};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use uuid::Uuid;

type Result<T> = std::result::Result<T, ValidationError>;
pub const ENVIRONMENT_LIMIT: usize = 256 * 1024;

/// Paths resolved/owned by the platform. A caller must verify filesystem ownership
/// and create isolated directories before using the resulting arguments to launch.
pub struct TemplatePaths<'a> {
    pub executable: &'a Path,
    pub instance_root: Option<&'a Path>,
}

// No Debug or Serialize: argv and environment can include credentials.
pub struct TemplateOutput {
    pub args: Vec<OsString>,
    pub environment: EnvPatch,
    pub cwd: PathBuf,
    pub data: Option<IsolatedPaths>,
}

pub struct IsolatedPaths {
    pub root: PathBuf,
    pub user_data: PathBuf,
    pub app_home: PathBuf,
}

pub fn compile(
    manifest: &Manifest,
    instance_id: Uuid,
    paths: TemplatePaths<'_>,
    mut secret: impl FnMut(Uuid) -> Result<String>,
) -> Result<TemplateOutput> {
    manifest.validate()?;
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or(ValidationError("INSTANCE_NOT_FOUND"))?;
    let application = manifest
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .ok_or(ValidationError("APPLICATION_NOT_FOUND"))?;
    let executable = absolute(paths.executable)?;
    if !executable
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err(ValidationError("EXE_REQUIRED"));
    }
    let app_dir = executable
        .parent()
        .ok_or(ValidationError("APPLICATION_DIRECTORY_REQUIRED"))?;
    let data = match (&instance.data, paths.instance_root) {
        (InstanceData::Original {}, None) => None,
        (InstanceData::Isolated { .. }, Some(root)) => {
            let root = absolute(root)?.to_owned();
            Some(IsolatedPaths {
                user_data: root.join("user-data"),
                app_home: root.join("app-home"),
                root,
            })
        }
        _ => return Err(ValidationError("INSTANCE_PATH_MODE_MISMATCH")),
    };
    let expand = |input: &str| {
        model::expand_placeholders(input, |name| {
            let path = match name {
                "app_dir" => app_dir,
                "instance_root" => {
                    &data
                        .as_ref()
                        .ok_or(ValidationError("ISOLATED_PATH_VARIABLE_IN_ORIGINAL"))?
                        .root
                }
                "user_data" => {
                    &data
                        .as_ref()
                        .ok_or(ValidationError("ISOLATED_PATH_VARIABLE_IN_ORIGINAL"))?
                        .user_data
                }
                "app_home" => {
                    &data
                        .as_ref()
                        .ok_or(ValidationError("ISOLATED_PATH_VARIABLE_IN_ORIGINAL"))?
                        .app_home
                }
                _ => return Err(ValidationError("UNKNOWN_PATH_VARIABLE")),
            };
            unicode(path).map(str::to_owned)
        })
    };
    let mut args = instance
        .args
        .iter()
        .map(|arg| expand(arg).map(OsString::from))
        .collect::<Result<Vec<_>>>()?;
    let cwd = match &instance.cwd {
        WorkingDirectory::Application {} => app_dir.to_owned(),
        WorkingDirectory::Explicit { path } => PathBuf::from(expand(unicode(path)?)?),
    };
    absolute(&cwd)?;
    let mut patch = EnvPatch {
        set: BTreeMap::new(),
        unset: model::MANAGED_ENV.iter().map(|s| (*s).into()).collect(),
    };
    for name in &instance.env.unset {
        unset(&mut patch, name);
    }
    for (name, value) in &instance.env.set {
        let value = match value {
            EnvValue::Literal { value } => value.clone(),
            EnvValue::SecretRef { id } => secret(*id)?,
        };
        set(&mut patch, name, value);
    }
    if let Some(data) = &data {
        args.push(format!("--user-data-dir={}", unicode(&data.user_data)?).into());
        match application.template_ref {
            Template::Codex => {
                set(&mut patch, "CODEX_HOME", unicode(&data.app_home)?.into());
                set(
                    &mut patch,
                    "CODEX_ELECTRON_USER_DATA_PATH",
                    unicode(&data.user_data)?.into(),
                );
            }
            Template::Claude => set(
                &mut patch,
                "CLAUDE_CONFIG_DIR",
                unicode(&data.app_home)?.into(),
            ),
            _ => return Err(ValidationError("ISOLATION_UNSUPPORTED")),
        }
    }
    let chromium = application.template_ref != Template::Environment;
    match instance.network {
        NetworkBinding::Direct {} => {
            if chromium {
                args.push("--no-proxy-server".into());
            }
        }
        NetworkBinding::Profile { profile_id } => {
            let profile = manifest
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or(ValidationError("PROFILE_NOT_FOUND"))?;
            let endpoint = format!(
                "http://{}",
                SocketAddr::new(profile.endpoint.host, profile.endpoint.port)
            );
            if chromium {
                args.push(format!("--proxy-server={endpoint}").into());
            }
            for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY"] {
                set(&mut patch, name, endpoint.clone());
            }
            // Clearing inherited bypass lists keeps another launcher from bypassing this binding.
            set(&mut patch, "NO_PROXY", String::new());
        }
    }
    patch
        .validate()
        .map_err(|_| ValidationError("INVALID_COMPILED_ENVIRONMENT"))?;
    let bytes: usize = patch
        .set
        .iter()
        .map(|(name, value)| (name.encode_utf16().count() + value.encode_utf16().count() + 2) * 2)
        .chain(
            patch
                .unset
                .iter()
                .map(|name| (name.encode_utf16().count() + 1) * 2),
        )
        .sum();
    if bytes > ENVIRONMENT_LIMIT {
        return Err(ValidationError("ENVIRONMENT_TOO_LARGE"));
    }
    Ok(TemplateOutput {
        args,
        environment: patch,
        cwd,
        data,
    })
}

fn set(patch: &mut EnvPatch, name: &str, value: String) {
    patch.unset.retain(|key| !key.eq_ignore_ascii_case(name));
    patch.set.insert(name.into(), value);
}
fn unset(patch: &mut EnvPatch, name: &str) {
    if !patch.unset.iter().any(|key| key.eq_ignore_ascii_case(name)) {
        patch.unset.push(name.into());
    }
}
fn unicode(path: &Path) -> Result<&str> {
    path.to_str()
        .filter(|value| !value.contains('\0'))
        .ok_or(ValidationError("UNICODE_PATH_REQUIRED"))
}
fn absolute(path: &Path) -> Result<&Path> {
    unicode(path)?;
    if !path.is_absolute() {
        return Err(ValidationError("ABSOLUTE_PATH_REQUIRED"));
    }
    Ok(path)
}
