//! On-disk configuration. Runtime handles and process state are deliberately separate.
use crate::{EnvPatch, FileIdentity};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

pub const FORMAT: &str = "app-proxy-rust";
pub const SCHEMA_VERSION: u32 = 1;
pub const MANIFEST_LIMIT: usize = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("{0}")]
pub struct ValidationError(pub &'static str);
type Result<T> = std::result::Result<T, ValidationError>;

// Configuration values can contain secrets, including arbitrary argv: no Debug derives.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub schema_version: u32,
    pub store_id: Uuid,
    pub revision: u64,
    pub owner_sid: String,
    pub applications: Vec<Application>,
    pub instances: Vec<Instance>,
    pub profiles: Vec<ProxyProfile>,
    pub settings: Settings,
    pub integrations: Integrations,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Application {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub locator: ApplicationLocator,
    pub template_ref: Template,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApplicationLocator {
    Exe { path: PathBuf },
    Msix { family_name: String, app_id: String },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Template {
    #[serde(rename = "builtin.codex@1")]
    Codex,
    #[serde(rename = "builtin.claude@1")]
    Claude,
    #[serde(rename = "builtin.chromium@1")]
    Chromium,
    #[serde(rename = "builtin.environment@1")]
    Environment,
}

impl Template {
    pub fn supports_isolation(self) -> bool {
        matches!(self, Self::Codex | Self::Claude)
    }
    pub fn supports_guard(self) -> bool {
        self != Self::Environment
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Instance {
    pub id: Uuid,
    pub application_id: Uuid,
    pub name: String,
    pub revision: u64,
    pub data: InstanceData,
    pub args: Vec<String>,
    pub env: SavedEnvironment,
    pub cwd: WorkingDirectory,
    pub network: NetworkBinding,
    pub guard: GuardConfig,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum InstanceData {
    Original {},
    Isolated { location: StorageLocation },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StorageLocation {
    Store {
        relative_path: PathBuf,
    },
    PackageLocalState {
        family_name: String,
        namespace: String,
        relative_path: PathBuf,
    },
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedEnvironment {
    #[serde(deserialize_with = "unique_map")]
    pub set: BTreeMap<String, EnvValue>,
    pub unset: Vec<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EnvValue {
    Literal { value: String },
    SecretRef { id: Uuid },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkingDirectory {
    Application {},
    Explicit { path: PathBuf },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkBinding {
    Direct {},
    Profile { profile_id: Uuid },
}

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Desired {
    Enabled,
    Disabled,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardConfig {
    pub desired: Desired,
    pub policy: GuardPolicy,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuardPolicy {
    StopUnproxied,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProxyProfile {
    pub id: Uuid,
    pub name: String,
    pub revision: u64,
    pub kind: ProxyKind,
    pub endpoint: Endpoint,
    pub selected_node_id: Uuid,
    pub source: ProxySource,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyKind {
    Managed,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Endpoint {
    pub host: IpAddr,
    pub port: u16,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProxySource {
    Manual {
        nodes: Vec<ManualNode>,
    },
    Subscription {
        url_secret_id: Uuid,
        revision: u64,
        nodes: Vec<crate::subscription::saved::SavedNode>,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManualNode {
    pub id: Uuid,
    pub name: String,
    pub protocol: ManualProtocol,
    pub host: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credentials: Option<Credentials>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManualProtocol {
    Http,
    Socks5,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub username: String,
    pub password_secret_id: Uuid,
}

pub fn validate_proxy_credentials(
    protocol: &ManualProtocol,
    username: &str,
    password: &str,
) -> Result<()> {
    if username.is_empty()
        || username.contains('\0')
        || password.contains('\0')
        || (matches!(protocol, ManualProtocol::Http) && username.contains(':'))
        || (matches!(protocol, ManualProtocol::Socks5)
            && (username.len() > 255 || password.is_empty() || password.len() > 255))
    {
        return Err(ValidationError("INVALID_PROXY_CREDENTIALS"));
    }
    Ok(())
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub test_url: String,
    pub exit_url: String,
    pub health_policy: HealthPolicy,
    pub download_network: NetworkBinding,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HealthPolicy {
    pub expected_statuses: Vec<u16>,
    pub follow_redirects: bool,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integrations {
    pub shortcuts: Vec<Shortcut>,
    pub guard_login_task: Option<LoginTask>,
    pub ifeo: Vec<IfeoRegistration>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shortcut {
    pub instance_id: Uuid,
    pub path: PathBuf,
    pub target: PathBuf,
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginTask {
    pub name: String,
    pub target: PathBuf,
    pub args: Vec<String>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IfeoRegistration {
    pub id: Uuid,
    pub revision: u64,
    pub application_id: Uuid,
    pub default_instance_id: Uuid,
    pub desired: Desired,
    pub owner_sid: String,
    pub store_id: Uuid,
    pub installed_target: InstalledTarget,
    pub registration_generation: Uuid,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstalledTarget {
    pub path: PathBuf,
    pub file_identity: FileIdentity,
    pub package_full_name: Option<String>,
}

impl Manifest {
    pub fn empty(owner_sid: String) -> Self {
        Self {
            format: FORMAT.into(),
            schema_version: SCHEMA_VERSION,
            store_id: Uuid::new_v4(),
            revision: 1,
            owner_sid,
            applications: vec![],
            instances: vec![],
            profiles: vec![],
            settings: Settings {
                test_url: "https://www.gstatic.com/generate_204".into(),
                exit_url: "https://api.ipify.org".into(),
                health_policy: HealthPolicy {
                    expected_statuses: vec![200, 204],
                    follow_redirects: false,
                },
                download_network: NetworkBinding::Direct {},
            },
            integrations: Integrations::default(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.format != FORMAT || self.schema_version != SCHEMA_VERSION {
            return Err(ValidationError("UNSUPPORTED_FORMAT"));
        }
        if self.store_id.is_nil() || self.revision == 0 || !self.owner_sid.starts_with("S-1-") {
            return Err(ValidationError("INVALID_STORE_HEADER"));
        }
        let mut ids = HashSet::from([self.store_id]);
        let mut originals = HashSet::new();
        let mut endpoints = HashSet::new();
        let mut locations = HashSet::new();
        for app in &self.applications {
            entity(&mut ids, app.id, &app.name, app.revision)?;
            match &app.locator {
                ApplicationLocator::Exe { path } => {
                    absolute(path)?;
                    if !path
                        .extension()
                        .is_some_and(|s| s.eq_ignore_ascii_case("exe"))
                    {
                        return Err(ValidationError("EXE_REQUIRED"));
                    }
                }
                ApplicationLocator::Msix {
                    family_name,
                    app_id,
                } => {
                    atom(family_name)?;
                    atom(app_id)?;
                }
            }
        }
        for profile in &self.profiles {
            entity(&mut ids, profile.id, &profile.name, profile.revision)?;
            if !profile.endpoint.host.is_loopback()
                || profile.endpoint.port == 0
                || !endpoints.insert(profile.endpoint.clone())
            {
                return Err(ValidationError("INVALID_OR_DUPLICATE_ENDPOINT"));
            }
            let mut node_ids = HashSet::new();
            let mut names = HashSet::new();
            match &profile.source {
                ProxySource::Manual { nodes } => {
                    for node in nodes {
                        if node.id.is_nil()
                            || !node_ids.insert(node.id)
                            || node.name.trim().is_empty()
                            || !names.insert(&node.name)
                            || node.host.is_empty()
                            || node.host.contains(['\0', '/', '\\', '@'])
                            || node.host.chars().any(char::is_whitespace)
                            || node.port == 0
                        {
                            return Err(ValidationError("INVALID_NODE"));
                        }
                        if let Some(credentials) = &node.credentials
                            && (credentials.username.contains('\0')
                                || credentials.password_secret_id.is_nil())
                        {
                            return Err(ValidationError("INVALID_CREDENTIAL_REFERENCE"));
                        }
                    }
                }
                ProxySource::Subscription {
                    url_secret_id,
                    revision,
                    nodes,
                } => {
                    if url_secret_id.is_nil()
                        || *revision == 0
                        || nodes.len() > crate::subscription::NODE_LIMIT
                    {
                        return Err(ValidationError("INVALID_SUBSCRIPTION_SOURCE"));
                    }
                    for node in nodes {
                        node.validate()
                            .map_err(|_| ValidationError("INVALID_SUBSCRIPTION_NODE"))?;
                        if !node_ids.insert(node.id) || !names.insert(&node.name) {
                            return Err(ValidationError("INVALID_SUBSCRIPTION_NODE"));
                        }
                    }
                }
            }
            if !node_ids.contains(&profile.selected_node_id) {
                return Err(ValidationError("SELECTED_NODE_NOT_FOUND"));
            }
        }
        for instance in &self.instances {
            entity(&mut ids, instance.id, &instance.name, instance.revision)?;
            let app = self
                .applications
                .iter()
                .find(|a| a.id == instance.application_id)
                .ok_or(ValidationError("APPLICATION_NOT_FOUND"))?;
            match &instance.data {
                InstanceData::Original {} => {
                    if !originals.insert(app.id) {
                        return Err(ValidationError("DUPLICATE_ORIGINAL"));
                    }
                }
                InstanceData::Isolated { location } => {
                    if !app.template_ref.supports_isolation() {
                        return Err(ValidationError("ISOLATION_UNSUPPORTED"));
                    }
                    let (root, path) = match location {
                        StorageLocation::Store { relative_path } => {
                            (String::from("store"), relative_path)
                        }
                        StorageLocation::PackageLocalState {
                            family_name,
                            namespace,
                            relative_path,
                        } => {
                            if !matches!(&app.locator, ApplicationLocator::Msix { family_name: f, .. } if f == family_name)
                                || namespace != &self.store_id.to_string()
                            {
                                return Err(ValidationError("INVALID_PACKAGE_STORAGE"));
                            }
                            (format!("package:{family_name}:{namespace}"), relative_path)
                        }
                    };
                    safe_relative(path)?;
                    let assigned = PathBuf::from("instances").join(instance.id.to_string());
                    if path != &assigned {
                        return Err(ValidationError("INSTANCE_STORAGE_NOT_ASSIGNED"));
                    }
                    if !locations.insert((root, path.clone())) {
                        return Err(ValidationError("DUPLICATE_INSTANCE_DATA"));
                    }
                }
            }
            if let WorkingDirectory::Explicit { path } = &instance.cwd {
                validate_working_directory(path)?;
            }
            self.validate_network(instance.network)?;
            if instance.guard.desired == Desired::Enabled
                && (!app.template_ref.supports_guard()
                    || matches!(instance.network, NetworkBinding::Direct {}))
            {
                return Err(ValidationError("GUARD_REQUIRES_SUPPORTED_PROXY"));
            }
            validate_instance_input(&instance.args, &instance.env)?;
        }
        self.validate_network(self.settings.download_network)?;
        for raw in [&self.settings.test_url, &self.settings.exit_url] {
            let url = url::Url::parse(raw).map_err(|_| ValidationError("INVALID_HEALTH_URL"))?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(ValidationError("INVALID_HEALTH_URL"));
            }
        }
        if self.settings.health_policy.follow_redirects
            || self.settings.health_policy.expected_statuses.is_empty()
            || self
                .settings
                .health_policy
                .expected_statuses
                .iter()
                .any(|s| !(200..=599).contains(s) || (300..400).contains(s))
        {
            return Err(ValidationError("INVALID_HEALTH_POLICY"));
        }
        let mut ifeo_apps = HashSet::new();
        for registration in &self.integrations.ifeo {
            entity(&mut ids, registration.id, "ifeo", registration.revision)?;
            let instance = self
                .instances
                .iter()
                .find(|i| i.id == registration.default_instance_id)
                .ok_or(ValidationError("IFEO_INSTANCE_NOT_FOUND"))?;
            if !matches!(instance.data, InstanceData::Original {})
                || instance.application_id != registration.application_id
                || instance.guard.desired != Desired::Enabled
                || registration.owner_sid != self.owner_sid
                || registration.store_id != self.store_id
                || registration.registration_generation.is_nil()
                || !ifeo_apps.insert(registration.application_id)
            {
                return Err(ValidationError("INVALID_IFEO_REGISTRATION"));
            }
            absolute(&registration.installed_target.path)?;
        }
        let mut shortcut_paths = HashSet::new();
        for shortcut in &self.integrations.shortcuts {
            if !self.instances.iter().any(|i| i.id == shortcut.instance_id) {
                return Err(ValidationError("SHORTCUT_INSTANCE_NOT_FOUND"));
            }
            absolute(&shortcut.path)?;
            absolute(&shortcut.target)?;
            if !shortcut_paths.insert(&shortcut.path)
                || shortcut.args.iter().any(|a| a.contains('\0'))
            {
                return Err(ValidationError("INVALID_SHORTCUT"));
            }
        }
        if let Some(task) = &self.integrations.guard_login_task {
            atom(&task.name)?;
            absolute(&task.target)?;
            if task.args.iter().any(|a| a.contains('\0')) {
                return Err(ValidationError("INVALID_TASK"));
            }
        }
        Ok(())
    }

    fn validate_network(&self, binding: NetworkBinding) -> Result<()> {
        if let NetworkBinding::Profile { profile_id } = binding
            && !self.profiles.iter().any(|p| p.id == profile_id)
        {
            return Err(ValidationError("PROFILE_NOT_FOUND"));
        }
        Ok(())
    }

    pub fn secret_ids(&self) -> HashSet<Uuid> {
        let mut ids = HashSet::new();
        for instance in &self.instances {
            for value in instance.env.set.values() {
                if let EnvValue::SecretRef { id } = value {
                    ids.insert(*id);
                }
            }
        }
        for profile in &self.profiles {
            match &profile.source {
                ProxySource::Manual { nodes } => {
                    for node in nodes {
                        if let Some(c) = &node.credentials {
                            ids.insert(c.password_secret_id);
                        }
                    }
                }
                ProxySource::Subscription {
                    url_secret_id,
                    nodes,
                    ..
                } => {
                    ids.insert(*url_secret_id);
                    ids.extend(nodes.iter().map(|node| node.secret_id));
                }
            }
        }
        ids
    }
}

pub const MANAGED_ENV: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "CODEX_HOME",
    "CODEX_ELECTRON_USER_DATA_PATH",
    "CLAUDE_CONFIG_DIR",
];
pub fn validate_instance_input(args: &[String], env: &SavedEnvironment) -> Result<()> {
    for arg in args {
        expand_placeholders(arg, |_| Ok(String::new()))?;
        // Match Chromium's Windows switch spelling before checking reserved names.
        let trimmed = arg.trim();
        let switch = trimmed
            .strip_prefix("--")
            .or_else(|| trimmed.strip_prefix(['-', '/']));
        let key = switch
            .unwrap_or("")
            .split('=')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        if arg.contains('\0')
            || trimmed == "--"
            || matches!(
                key.as_str(),
                "user-data-dir"
                    | "proxy-server"
                    | "proxy-pac-url"
                    | "proxy-bypass-list"
                    | "no-proxy-server"
                    | "proxy-auto-detect"
                    | "single-argument"
            )
        {
            return Err(ValidationError("MANAGED_OR_INVALID_ARGUMENT"));
        }
    }
    let mut patch = EnvPatch {
        set: BTreeMap::new(),
        unset: env.unset.clone(),
    };
    for (name, value) in &env.set {
        let literal = match value {
            EnvValue::Literal { value } => value.clone(),
            EnvValue::SecretRef { id } => {
                if id.is_nil() {
                    return Err(ValidationError("INVALID_SECRET_REFERENCE"));
                }
                String::new()
            }
        };
        patch.set.insert(name.clone(), literal);
    }
    patch
        .validate()
        .map_err(|_| ValidationError("INVALID_ENV_PATCH"))?;
    if env.set.keys().chain(env.unset.iter()).any(|name| {
        MANAGED_ENV
            .iter()
            .any(|managed| name.eq_ignore_ascii_case(managed))
    }) {
        return Err(ValidationError("MANAGED_ENV_CONFLICT"));
    }
    Ok(())
}

pub(crate) fn expand_placeholders(
    input: &str,
    mut resolve: impl FnMut(&str) -> Result<String>,
) -> Result<String> {
    let mut output = String::new();
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let name_and_tail = &rest[start + 2..];
        let end = name_and_tail
            .find('}')
            .ok_or(ValidationError("INVALID_PATH_VARIABLE"))?;
        let name = &name_and_tail[..end];
        if !matches!(name, "app_dir" | "instance_root" | "user_data" | "app_home") {
            return Err(ValidationError("UNKNOWN_PATH_VARIABLE"));
        }
        output.push_str(&resolve(name)?);
        rest = &name_and_tail[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn validate_working_directory(path: &Path) -> Result<()> {
    let value = path
        .to_str()
        .ok_or(ValidationError("ABSOLUTE_UNICODE_PATH_REQUIRED"))?;
    if value.contains('\0') {
        return Err(ValidationError("ABSOLUTE_UNICODE_PATH_REQUIRED"));
    }
    // All four substitutions resolve to absolute paths, never shell fragments.
    let substituted = expand_placeholders(value, |_| Ok(String::from("C:\\template-root")))?;
    absolute(Path::new(&substituted))
}

fn entity(ids: &mut HashSet<Uuid>, id: Uuid, name: &str, revision: u64) -> Result<()> {
    if id.is_nil()
        || !ids.insert(id)
        || name.trim().is_empty()
        || name.contains('\0')
        || revision == 0
    {
        return Err(ValidationError("INVALID_OR_DUPLICATE_ENTITY"));
    }
    Ok(())
}
fn atom(value: &str) -> Result<()> {
    if value.is_empty()
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(ValidationError("INVALID_IDENTIFIER"));
    }
    Ok(())
}
fn absolute(path: &Path) -> Result<()> {
    if !path.is_absolute() || path.to_str().is_none_or(|p| p.contains('\0')) {
        return Err(ValidationError("ABSOLUTE_UNICODE_PATH_REQUIRED"));
    }
    Ok(())
}
pub fn safe_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
        || path.to_str().is_none_or(|s| {
            s.contains(['\0', ':'])
                || s.split(['/', '\\'])
                    .any(|p| p.is_empty() || p.ends_with(['.', ' ']))
        })
    {
        return Err(ValidationError("UNSAFE_RELATIVE_PATH"));
    }
    Ok(())
}

fn unique_map<'de, D, T>(deserializer: D) -> std::result::Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Unique<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Unique<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("an object with unique keys")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("DUPLICATE_ENV_KEY"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}
