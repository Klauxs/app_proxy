//! Store-scoped login integration. Native COM runs outside the configuration gate.
use crate::configuration::Configuration;
use app_proxy_windows::{
    Error, Result,
    guard_deployment::Deployment,
    guard_task::login::{
        Prepared, Registration,
        journal::{Preparation, Request, Status},
    },
};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub revision: u64,
    pub integration: Option<Integration>,
    pub ready: bool,
    pub diagnostic: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Integration {
    pub request: Request,
    pub status: Status,
}

pub fn apply(configuration: &Configuration, root: &Path, request: &Request) -> Result<Status> {
    let admission = configuration.lock()?.begin_login(request, None);
    let admission = match admission {
        Err(Error::Invalid(app_proxy_core::error_code::GUARD_LOGIN_AUTHORIZATION_REQUIRED)) => {
            // Admission already checked the request, revision and enabled intent.
            // It is repeated after native preparation to catch concurrent edits.
            let deployment = Deployment::listener(configuration.snapshot()?.store_id)?;
            let prepared = Prepared::authorized(&deployment, root)?;
            let admission = configuration
                .lock()?
                .begin_login(request, Some(&prepared))?;
            return finish(configuration, admission, |job| job.execute(Some(&prepared)));
        }
        other => other?,
    };
    finish(configuration, admission, |job| job.execute_authorized())
}

pub fn resume(configuration: &Configuration, id: Uuid) -> Result<Status> {
    let admission = configuration.lock()?.resume_login(id)?;
    finish(configuration, admission, |job| job.execute_authorized())
}

fn finish(
    configuration: &Configuration,
    admission: Preparation,
    execute: impl FnOnce(
        app_proxy_windows::guard_task::login::journal::Job,
    ) -> Result<app_proxy_windows::guard_task::login::journal::Completion>,
) -> Result<Status> {
    match admission {
        Preparation::Complete(status) => Ok(status),
        Preparation::Pending(job) => {
            let completion = execute(job)?;
            configuration.lock()?.complete_login(completion)
        }
    }
}

pub fn status(configuration: &Configuration, home: &Path) -> Result<View> {
    status_with(configuration, |registration| registration.ready(home))
}

/// Completed replies never wait behind unrelated native work. Full-payload
/// validation and terminal lookup share the same store lock.
pub(crate) fn replay(
    configuration: &Configuration,
    id: Uuid,
    request: Option<&Request>,
) -> Result<Option<Status>> {
    let mut store = configuration.lock()?;
    if !matches!(
        store.login_request_status(id)?,
        Some(Status::Created { .. } | Status::Removed { .. } | Status::Cancelled {})
    ) {
        return Ok(None);
    }
    let prepared = match request {
        Some(request) => store.begin_login(request, None)?,
        None => store.resume_login(id)?,
    };
    match prepared {
        Preparation::Complete(status) => Ok(Some(status)),
        Preparation::Pending(_) => Err(Error::Invalid("GUARD_LOGIN_STATE_CHANGED")),
    }
}
fn status_with(
    configuration: &Configuration,
    verify: impl FnOnce(&Registration) -> Result<bool>,
) -> Result<View> {
    let (revision, record, metadata) = {
        let store = configuration.lock()?;
        let manifest = store.load()?;
        (
            manifest.revision,
            store.login_registration()?,
            manifest.integrations.guard_login_task,
        )
    };
    let (ready, diagnostic) = match &record {
        None if metadata.is_some() => (false, Some("GUARD_LOGIN_OWNERSHIP_UNAVAILABLE".into())),
        None => (false, None),
        Some((_, state, registration)) => {
            let metadata_matches = match &metadata {
                Some(metadata) => registration.matches_metadata(metadata)?,
                None => false,
            };
            if metadata.is_some() && !metadata_matches {
                (false, Some("GUARD_LOGIN_METADATA_CONFLICT".into()))
            } else {
                match verify(registration) {
                    Ok(true) if matches!(state, Status::Created { .. }) && metadata_matches => {
                        (true, None)
                    }
                    Ok(true) => (false, Some("GUARD_LOGIN_OPERATION_PENDING".into())),
                    Ok(false) => (false, Some("GUARD_LOGIN_TASK_MISSING".into())),
                    Err(Error::Invalid(code)) => (false, Some(code.into())),
                    Err(_) => (false, Some("GUARD_LOGIN_CHECK_FAILED".into())),
                }
            }
        }
    };
    // A native read may race a removal or configuration write, including an
    // admitted operation which changes the journal but not the manifest yet.
    let store = configuration.lock()?;
    if store.load()?.revision != revision || store.login_registration()? != record {
        return Err(Error::Invalid("GUARD_LOGIN_STATE_CHANGED"));
    }
    Ok(View {
        revision,
        integration: record.map(|(request, status, _)| Integration { request, status }),
        ready,
        diagnostic,
    })
}

#[cfg(test)]
mod tests;
