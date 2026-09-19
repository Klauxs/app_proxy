//! Read-only IFEO entry preparation. This validates the fixed debugger envelope
//! and protected routing identity; it is not a launch or continuation permission.
//! Preserve the raw target command line until the application template decides
//! which activation/auxiliary semantics it can actually support.
use crate::{Error, Result, guard_deployment::Deployment, identity, ifeo_rules, installation};
use app_proxy_core::model::ApplicationLocator;
use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::PathBuf};
use uuid::Uuid;
use windows_sys::Win32::System::Environment::GetCommandLineW;

mod context;
const COMMAND_LIMIT: usize = 32766;

// Deliberately no Debug/Serialize: activation arguments may contain credentials.
pub struct Invocation {
    registration: Uuid,
    host: PathBuf,
    target: PathBuf,
    arguments: Vec<OsString>,
    raw_target: Vec<u16>,
}

/// Keeps host/record/directories and the target file pinned while the ordinary
/// caller prepares its handoff. The coordinator must independently revalidate
/// the registration and current manifest; this object cannot authorize spawn.
pub struct VerifiedInvocation {
    invocation: Invocation,
    registration: ifeo_rules::Registration,
    deployment: Deployment,
    target: installation::ResolvedApplication,
}
impl Invocation {
    pub fn registration_id(&self) -> Uuid {
        self.registration
    }
    pub fn arguments(&self) -> &[OsString] {
        &self.arguments
    }
    pub fn raw_target_command_line(&self) -> &[u16] {
        &self.raw_target
    }
    pub fn verify(self) -> Result<VerifiedInvocation> {
        context::ordinary_entry()?;
        let current = identity::current()?;
        let (registration, deployment) = ifeo_rules::open_verified(self.registration)?;
        self.matches_record(&registration, &current)?;
        // Resolve only the protected registered path, never an untrusted argv[0].
        let target = installation::resolve(&ApplicationLocator::Exe {
            path: registration.target.clone(),
        })?;
        if target.image() != &registration.target_image {
            return Err(Error::Invalid("IFEO_TARGET_CHANGED"));
        }
        Ok(VerifiedInvocation {
            invocation: self,
            registration,
            deployment,
            target,
        })
    }
    fn matches_record(
        &self,
        record: &ifeo_rules::Registration,
        current: &app_proxy_core::ProcessIdentity,
    ) -> Result<()> {
        if self.registration != record.id || current.user_sid != record.owner_sid {
            return Err(Error::Invalid("IFEO_OWNER_MISMATCH"));
        }
        record.matches_entry_paths(&self.host, &self.target)?;
        record.matches_entry_paths(&current.image_path, &self.target)?;
        if current.image_file != record.host_image {
            return Err(Error::Invalid("IFEO_ENTRY_IMAGE_MISMATCH"));
        }
        Ok(())
    }
}
impl VerifiedInvocation {
    pub fn registration(&self) -> &ifeo_rules::Registration {
        &self.registration
    }
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }
    pub fn deployment(&self) -> &Deployment {
        &self.deployment
    }
    /// Recheck before dispatch; keeping file handles does not pin package
    /// registration or prevent an administrator from changing an IFEO rule.
    pub fn verify_current(&self) -> Result<()> {
        context::ordinary_entry()?;
        let actual = ifeo_rules::verify_registered(self.registration.id)?;
        if serde_json::to_vec(&actual)? != serde_json::to_vec(&self.registration)? {
            return Err(Error::Invalid("IFEO_REGISTRATION_CHANGED"));
        }
        self.invocation
            .matches_record(&actual, &identity::current()?)?;
        self.target.verify_current()
    }
}

/// Copies the system-owned wide string; does not reconstruct it from Rust argv.
pub fn capture() -> Result<Invocation> {
    parse(&raw_command_line()?)
}
fn raw_command_line() -> Result<Vec<u16>> {
    // SAFETY: Windows retains its NUL-terminated process command line for the
    // process lifetime. Read only, bounded by the Windows command-line limit.
    unsafe {
        let command = GetCommandLineW();
        if command.is_null() {
            return Err(Error::Invalid("IFEO_COMMAND_LINE_UNAVAILABLE"));
        }
        let mut result = Vec::new();
        for index in 0..=COMMAND_LIMIT {
            let unit = *command.add(index);
            if unit == 0 {
                return Ok(result);
            }
            result.push(unit);
        }
        Err(Error::Invalid("IFEO_COMMAND_LINE_SIZE"))
    }
}

fn parse(command: &[u16]) -> Result<Invocation> {
    let invalid = || Error::Invalid("IFEO_INVALID_ENVELOPE");
    if command.is_empty() || command.len() > COMMAND_LIMIT || command.contains(&0) {
        return Err(invalid());
    }
    // The installed Debugger command always quotes the absolute host path and
    // uses this fixed spelling. Do not search for a later `--` in attacker input.
    if command[0] != b'"' as u16 {
        return Err(invalid());
    }
    let host_end = command[1..]
        .iter()
        .position(|&c| c == b'"' as u16)
        .map(|i| i + 1)
        .ok_or_else(invalid)?;
    let host = PathBuf::from(OsString::from_wide(&command[1..host_end]));
    let fixed: Vec<u16> = " ifeo-entry --registration ".encode_utf16().collect();
    let rest = command[host_end + 1..]
        .strip_prefix(fixed.as_slice())
        .ok_or_else(invalid)?;
    let id = rest.get(..36).ok_or_else(invalid)?;
    let id = String::from_utf16(id).map_err(|_| invalid())?;
    let registration = Uuid::parse_str(&id).map_err(|_| invalid())?;
    if registration.is_nil() || registration.to_string() != id {
        return Err(invalid());
    }
    let separator: Vec<u16> = " -- ".encode_utf16().collect();
    let raw_target = rest[36..]
        .strip_prefix(separator.as_slice())
        .ok_or_else(invalid)?;
    // Parsing a command beginning with whitespace produces an empty argv[0];
    // the shared native parser rejects it rather than silently changing target.
    let mut arguments = crate::process_query::parse_arguments(raw_target)
        .map_err(|_| Error::Invalid("IFEO_INVALID_TARGET_COMMAND"))?
        .ok_or(Error::Invalid("IFEO_INVALID_TARGET_COMMAND"))?;
    let target = PathBuf::from(arguments.remove(0));
    if !host.is_absolute() || !target.is_absolute() {
        return Err(Error::Invalid("IFEO_ABSOLUTE_PATH_REQUIRED"));
    }
    Ok(Invocation {
        registration,
        host,
        target,
        arguments,
        raw_target: raw_target.to_vec(),
    })
}

#[cfg(test)]
mod tests;
