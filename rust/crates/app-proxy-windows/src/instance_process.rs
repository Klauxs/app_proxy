//! Read-only instance attribution for an exact process. No result authorizes a
//! stop, adoption, proxy change or a claim that the complete system is vacant.
use crate::{
    Error, Result, identity, installation::ResolvedApplication, instance_data::PreparedData,
    process, process_query,
};
use app_proxy_core::{FileIdentity, ProcessIdentity, model::Template};
use std::{
    ffi::OsString,
    net::SocketAddr,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::{Component, Path, PathBuf, Prefix},
    ptr,
};
use windows_sys::Win32::{
    Foundation::INVALID_HANDLE_VALUE,
    Storage::FileSystem::*,
    System::WindowsProgramming::{DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOVABLE},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessRole {
    Main,
    Auxiliary,
    Unknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstanceRelation {
    Target,
    Other,
    Unknown,
}

/// Argument evidence only, never a network health result or stop authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProxyArguments {
    Matching,
    Mismatched,
    Unknown,
}

/// Contains no arguments or environment. Auxiliary identities are never main
/// targets; a parent PID alone cannot promote one to a managed main process.
pub struct InstanceObservation {
    pub identity: ProcessIdentity,
    pub role: ProcessRole,
    pub relation: InstanceRelation,
    pub proxy: ProxyArguments,
}

/// Installation/data handles borrowed here remain pinned through observation.
pub struct InstanceTarget<'a> {
    application: &'a ResolvedApplication,
    data: Option<&'a PreparedData>,
    template: Template,
}
impl<'a> InstanceTarget<'a> {
    /// Fast Guard main attribution. No candidate enumeration, ancestor query or
    /// WMI call. The caller retains this exact process through stop dispatch.
    pub fn inspect_pinned(
        &self,
        process: &crate::native_process::PinnedProcess,
        endpoint: SocketAddr,
    ) -> Result<InstanceObservation> {
        process.verify()?;
        if process.identity().image_file != *self.application.image()
            || self.template == Template::Environment
            || !endpoint.ip().is_loopback()
            || endpoint.port() == 0
        {
            return Err(Error::IdentityMismatch);
        }
        let (role, relation) = classify(
            process.arguments(),
            self.data.map(|d| d.paths.user_data.as_path()),
        )?;
        let proxy = if role == ProcessRole::Main && relation == InstanceRelation::Target {
            proxy_arguments(process.arguments(), endpoint)
        } else {
            ProxyArguments::Unknown
        };
        Ok(InstanceObservation {
            identity: process.identity().clone(),
            role,
            relation,
            proxy,
        })
    }
    pub fn new(
        application: &'a ResolvedApplication,
        data: Option<&'a PreparedData>,
        template: Template,
    ) -> Result<Self> {
        if data.is_some() && !template.supports_isolation() {
            return Err(Error::Invalid("ISOLATION_UNSUPPORTED"));
        }
        Ok(Self {
            application,
            data,
            template,
        })
    }

    pub async fn inspect(&self, expected: &ProcessIdentity) -> Result<InstanceObservation> {
        self.inspect_with_proxy(expected, None).await
    }

    /// Use the running session's recorded endpoint when it has one, rather than
    /// substituting a newly edited binding. No proxy connection is attempted.
    pub async fn inspect_proxy(
        &self,
        expected: &ProcessIdentity,
        endpoint: SocketAddr,
    ) -> Result<InstanceObservation> {
        if !endpoint.ip().is_loopback() || endpoint.port() == 0 {
            return Err(Error::Invalid("INVALID_PROXY_ENDPOINT"));
        }
        self.inspect_with_proxy(expected, Some(endpoint)).await
    }

    /// One bounded WMI query for this scan's exact candidates. Parent rows are
    /// shared only within this call; every native identity is still rechecked.
    pub async fn inspect_candidates(
        &self,
        candidates: &[ProcessIdentity],
        endpoint: Option<SocketAddr>,
    ) -> Result<Vec<InstanceObservation>> {
        if endpoint.is_some_and(|e| !e.ip().is_loopback() || e.port() == 0) {
            return Err(Error::Invalid("INVALID_PROXY_ENDPOINT"));
        }
        let mut result = Vec::new();
        let mut queried = Vec::new();
        for expected in candidates {
            if expected.image_file != *self.application.image()
                || self.template == Template::Environment
            {
                result.push(self.inspect_with_proxy(expected, endpoint).await?);
            } else {
                queried.push(expected.clone());
            }
        }
        if queried.is_empty() {
            return Ok(result);
        }
        let data = self.data.map(|data| data.paths.user_data.clone());
        let inputs = queried.clone();
        let inspected = process_query::inspect_group_with(&queried, move |batch, deadline| {
            inputs
                .into_iter()
                .map(|expected| {
                    batch.inspect(expected, deadline, |observed| {
                        let (role, relation) =
                            classify_family(batch, &observed, data.as_deref(), deadline, 0)?;
                        let proxy =
                            if role == ProcessRole::Main && relation == InstanceRelation::Target {
                                endpoint.map_or(ProxyArguments::Unknown, |endpoint| {
                                    proxy_arguments(observed.arguments.as_deref(), endpoint)
                                })
                            } else {
                                ProxyArguments::Unknown
                            };
                        Ok(InstanceObservation {
                            identity: observed.identity,
                            role,
                            relation,
                            proxy,
                        })
                    })
                })
                .collect::<Result<Vec<_>>>()
        })
        .await?;
        result.extend(inspected);
        Ok(result)
    }

    async fn inspect_with_proxy(
        &self,
        expected: &ProcessIdentity,
        endpoint: Option<SocketAddr>,
    ) -> Result<InstanceObservation> {
        identity::assert_ordinary_user()?;
        let caller = identity::current()?;
        if expected.user_sid != caller.user_sid || expected.session_id != caller.session_id {
            return Err(Error::IdentityMismatch);
        }
        // A stale or fabricated identity must not produce even an exclusion that
        // could allow another launch. Observation errors remain unknown upstream.
        if !process::is_running_exact(expected)? {
            return Err(Error::Invalid("PROCESS_EXITED_DURING_INSPECTION"));
        }
        if expected.image_file != *self.application.image() {
            let relation = if same_path(&expected.image_path, self.application.executable())
                || (self.application.package().is_some()
                    && expected
                        .image_path
                        .file_name()
                        .zip(self.application.executable().file_name())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b)))
            {
                // A changed image or an older package version is not evidence
                // that this is another installation. Package recovery resolves it.
                InstanceRelation::Unknown
            } else {
                InstanceRelation::Other
            };
            return Ok(InstanceObservation {
                identity: expected.clone(),
                role: ProcessRole::Unknown,
                relation,
                proxy: ProxyArguments::Unknown,
            });
        }
        if self.template == Template::Environment {
            return Ok(InstanceObservation {
                identity: expected.clone(),
                role: ProcessRole::Main,
                relation: InstanceRelation::Target,
                proxy: ProxyArguments::Unknown,
            });
        }
        let data = self.data.map(|data| data.paths.user_data.clone());
        let process = expected.clone();
        process_query::inspect_group_with(std::slice::from_ref(expected), move |batch, deadline| {
            batch.inspect(process, deadline, |observed| {
                let (role, relation) =
                    classify_family(batch, &observed, data.as_deref(), deadline, 0)?;
                let proxy = if role == ProcessRole::Main && relation == InstanceRelation::Target {
                    endpoint.map_or(ProxyArguments::Unknown, |endpoint| {
                        proxy_arguments(observed.arguments.as_deref(), endpoint)
                    })
                } else {
                    ProxyArguments::Unknown
                };
                Ok(InstanceObservation {
                    identity: observed.identity,
                    role,
                    relation,
                    proxy,
                })
            })
        })
        .await
    }

    #[cfg(test)]
    fn classify(&self, arguments: Option<&[OsString]>) -> Result<(ProcessRole, InstanceRelation)> {
        classify(
            arguments,
            self.data.map(|data| data.paths.user_data.as_path()),
        )
    }
}

/// Only a known auxiliary without its own directory may inherit a relation.
/// Every ancestor remains pinned and is rechecked after the recursive query;
/// an exited/reused parent, different image or missing arguments stays unknown.
fn classify_family(
    batch: &process_query::Inspection,
    observed: &process_query::ProcessObservation,
    data: Option<&Path>,
    deadline: std::time::Instant,
    depth: usize,
) -> Result<(ProcessRole, InstanceRelation)> {
    let result = classify(observed.arguments.as_deref(), data)?;
    if result != (ProcessRole::Auxiliary, InstanceRelation::Unknown)
        || depth >= 8
        || !observed
            .arguments
            .as_deref()
            .and_then(chromium_switches)
            .is_some_and(|switches| switches.user_data.is_none())
    {
        return Ok(result);
    }
    let parent = identity::inspect(observed.parent_pid)?;
    if !valid_parent(&observed.identity, &parent) {
        return Ok(result);
    }
    batch.inspect(parent, deadline, |parent| {
        let (_, relation) = classify_family(batch, &parent, data, deadline, depth + 1)?;
        Ok((ProcessRole::Auxiliary, relation))
    })
}

fn valid_parent(child: &ProcessIdentity, parent: &ProcessIdentity) -> bool {
    child.pid != parent.pid
        && parent.creation_time < child.creation_time
        && parent.user_sid == child.user_sid
        && parent.session_id == child.session_id
        && parent.image_file == child.image_file
}

fn classify(
    arguments: Option<&[OsString]>,
    data: Option<&Path>,
) -> Result<(ProcessRole, InstanceRelation)> {
    let Some(switches) = arguments.and_then(chromium_switches) else {
        return Ok((ProcessRole::Unknown, InstanceRelation::Unknown));
    };
    let role = match switches.process_type.as_deref() {
        None => ProcessRole::Main,
        Some("renderer" | "gpu-process" | "utility" | "zygote" | "crashpad-handler") => {
            ProcessRole::Auxiliary
        }
        Some(_) => ProcessRole::Unknown,
    };
    let relation = match switches.user_data {
        None if role != ProcessRole::Main => InstanceRelation::Unknown,
        None if data.is_none() => InstanceRelation::Target,
        None => InstanceRelation::Other,
        Some(path) => {
            let Some(expected) = data else {
                // An external explicit directory may be the application's
                // default directory. No path means we have no default-data
                // identity to compare; do not declare the original vacant.
                return Ok((role, InstanceRelation::Unknown));
            };
            match directory_identity(&path) {
                Ok(actual) => {
                    if actual == directory_identity(expected)? {
                        InstanceRelation::Target
                    } else {
                        InstanceRelation::Other
                    }
                }
                Err(_) => InstanceRelation::Unknown,
            }
        }
    };
    Ok((role, relation))
}

struct ChromiumSwitches {
    user_data: Option<PathBuf>,
    process_type: Option<String>,
    proxy_server: Option<String>,
    proxy_conflict: bool,
}

fn proxy_arguments(arguments: Option<&[OsString]>, endpoint: SocketAddr) -> ProxyArguments {
    let Some(switches) = arguments.and_then(chromium_switches) else {
        return ProxyArguments::Unknown;
    };
    if switches.proxy_conflict {
        return ProxyArguments::Mismatched;
    }
    let Some(value) = switches.proxy_server.as_deref().filter(|s| !s.is_empty()) else {
        return ProxyArguments::Mismatched;
    };
    if value.eq_ignore_ascii_case("direct://") {
        return ProxyArguments::Mismatched;
    }
    // Chromium defaults a bare host:port to HTTP. Named hosts, per-scheme maps
    // and fallback lists can be equivalent, but are outside this small parser;
    // do not turn an interpretation we cannot prove into correction evidence.
    if value.contains([';', ',', '=', ' ', '@']) {
        return ProxyArguments::Unknown;
    }
    let (scheme, address) = value.split_once("://").unwrap_or(("http", value));
    let Ok(actual) = address.parse::<SocketAddr>() else {
        return ProxyArguments::Unknown;
    };
    if !scheme.eq_ignore_ascii_case("http") {
        return if ["https", "socks", "socks4", "socks5"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
        {
            ProxyArguments::Mismatched
        } else {
            ProxyArguments::Unknown
        };
    }
    if actual == endpoint {
        ProxyArguments::Matching
    } else {
        ProxyArguments::Mismatched
    }
}

fn chromium_switches(arguments: &[OsString]) -> Option<ChromiumSwitches> {
    if arguments.first()?.is_empty() {
        return None;
    }
    let mut result = ChromiumSwitches {
        user_data: None,
        process_type: None,
        proxy_server: None,
        proxy_conflict: false,
    };
    for word in &arguments[1..] {
        let word = word.to_str()?.trim();
        if word.contains('\0') {
            return None;
        }
        if word == "--" {
            break;
        }
        let Some(switch) = word
            .strip_prefix("--")
            .or_else(|| word.strip_prefix(['-', '/']))
        else {
            continue;
        };
        let (name, value) = switch.split_once('=').unwrap_or((switch, ""));
        match name.to_ascii_lowercase().as_str() {
            // Chromium's Windows parser has a raw-command-line special case;
            // a flattened argv is insufficient to classify it safely.
            "single-argument" => return None,
            "proxy-server" => {
                if result.proxy_server.is_some() {
                    result.proxy_conflict = true;
                }
                result.proxy_server = Some(value.into());
            }
            "proxy-pac-url" | "proxy-auto-detect" | "proxy-bypass-list" | "no-proxy-server" => {
                result.proxy_conflict = true;
            }
            "user-data-dir" => {
                if value.is_empty() || result.user_data.is_some() {
                    return None;
                }
                result.user_data = Some(value.into());
            }
            "type" => {
                if result.process_type.is_some() {
                    return None;
                }
                result.process_type = Some(value.into());
            }
            _ => {}
        }
    }
    Some(result)
}

/// Compare local directory identities without adopting them or contacting a
/// remote path found in another process's arguments. Spelling aliases still match;
/// reparse points and paths requiring a remote cwd remain unknown.
fn directory_identity(path: &Path) -> Result<FileIdentity> {
    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"));
    };
    let drive = match prefix.kind() {
        Prefix::Disk(drive) | Prefix::VerbatimDisk(drive) => drive,
        _ => return Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED")),
    };
    if components.next() != Some(Component::RootDir) {
        return Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"));
    }
    let drive_root = PathBuf::from(format!("{}:\\", char::from(drive)));
    let root = crate::wide(drive_root.as_os_str())?;
    // SAFETY: terminated local drive root only; UNC/device prefixes rejected above.
    let drive_type = unsafe { GetDriveTypeW(root.as_ptr()) };
    if !matches!(drive_type, DRIVE_FIXED | DRIVE_REMOVABLE | DRIVE_RAMDISK) {
        return Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"));
    }
    // Keep verbatim semantics: stripping the prefix could turn `name.` into
    // `name` and falsely attribute a different physical directory to this clone.
    let mut current = match prefix.kind() {
        Prefix::VerbatimDisk(_) => PathBuf::from(format!("\\\\?\\{}:\\", char::from(drive))),
        _ => drive_root,
    };
    let mut paths = vec![current.clone()];
    for component in components {
        let Component::Normal(part) = component else {
            return Err(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"));
        };
        if paths.len() >= 128 {
            return Err(Error::Invalid("INSTANCE_DIRECTORY_DEPTH_LIMIT"));
        }
        current.push(part);
        paths.push(current.clone());
    }
    let mut parents = Vec::new();
    let mut result = None;
    for path in paths {
        let name = crate::wide(path.as_os_str())?;
        // SAFETY: inspect from root to leaf without following reparse points.
        // Retained parent handles prevent replacement while descending.
        let (handle, info) = unsafe {
            let raw = CreateFileW(
                name.as_ptr(),
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                ptr::null_mut(),
            );
            if raw == INVALID_HANDLE_VALUE {
                return Err(crate::last_error("InspectInstanceDirectory"));
            }
            let handle = OwnedHandle::from_raw_handle(raw);
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            if GetFileInformationByHandle(handle.as_raw_handle(), &mut info) == 0 {
                return Err(crate::last_error("InstanceDirectoryIdentity"));
            }
            (handle, info)
        };
        if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(Error::Invalid("INSTANCE_DIRECTORY_REPARSE"));
        }
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(Error::Invalid("INSTANCE_DIRECTORY_REQUIRED"));
        }
        result = Some(FileIdentity {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        });
        parents.push(handle);
    }
    result.ok_or(Error::Invalid("INSTANCE_DIRECTORY_UNRESOLVED"))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let normalize = |path: &Path| {
        path.to_str().map(|p| {
            p.strip_prefix(r"\\?\")
                .unwrap_or(p)
                .replace('/', "\\")
                .to_ascii_lowercase()
        })
    };
    matches!((normalize(a), normalize(b)), (Some(a), Some(b)) if a == b)
}

#[cfg(test)]
mod tests;
