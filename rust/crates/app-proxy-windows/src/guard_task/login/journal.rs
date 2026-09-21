//! Intent and ownership survive lost replies. Native work executes outside the
//! configuration mutex; an operation lease serializes it until completion.
use super::*;
use crate::journal::{RETENTION, RequestJournal, now};
use crate::{storage_security, store::Store};
use app_proxy_core::model::{Desired, LoginTask};
use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle},
    sync::Arc,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const PATH: &str = "state/login-task.json";
const LIMIT: usize = 2 * 1024 * 1024;
const ENTRIES: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Remove,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: Uuid,
    pub expected_revision: u64,
    pub action: Action,
    pub expected_creation: Option<Uuid>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Status {
    Pending { action: Action },
    Created { revision: u64 },
    Removed { revision: u64 },
    Cancelled {},
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u32,
    store_id: Uuid,
    entries: Vec<Entry>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    create: Request,
    registration: Registration,
    created_revision: Option<u64>,
    removal: Option<Request>,
    removed_revision: Option<u64>,
    removed_at: Option<u64>,
}
impl Entry {
    fn request(&self, id: Uuid) -> Option<&Request> {
        if self.create.id == id {
            Some(&self.create)
        } else {
            self.removal.as_ref().filter(|r| r.id == id)
        }
    }
    fn status(&self, id: Uuid) -> Status {
        if id == self.create.id {
            if let Some(revision) = self.created_revision {
                Status::Created { revision }
            } else if self.removed_revision.is_some() {
                Status::Cancelled {}
            } else {
                Status::Pending {
                    action: Action::Create,
                }
            }
        } else if let Some(revision) = self.removed_revision {
            Status::Removed { revision }
        } else {
            Status::Pending {
                action: Action::Remove,
            }
        }
    }
}

pub enum Preparation {
    Complete(Status),
    Pending(Job),
}
pub struct Job {
    request: Request,
    registration: Registration,
    create_allowed: bool,
    _operation: File,
    _owner: Arc<File>,
}
/// Only successful native verification can create a completion. The same lease
/// prevents a delayed creation from crossing a subsequently admitted removal.
pub struct Completion {
    job: Job,
}
impl Job {
    /// Resume using the recorded home and fixed protected deployment. Existing
    /// tasks and removals do not depend on an installed/current release.
    pub fn execute_authorized(self) -> Result<Completion> {
        self.execute_with(
            Registration::exists_verified,
            |registration| {
                let deployment = Deployment::listener(registration.store_id)?;
                let prepared = Prepared::authorized(&deployment, &registration.home)?;
                if prepared.registration() != registration {
                    return Err(Error::Invalid("GUARD_LOGIN_REGISTRATION_CHANGED"));
                }
                prepared.register()
            },
            Registration::remove_idle,
        )
    }
    pub fn execute(self, prepared: Option<&Prepared<'_>>) -> Result<Completion> {
        self.execute_with(
            Registration::exists_verified,
            |registration| {
                let prepared = prepared.ok_or(Error::Invalid(
                    app_proxy_core::error_code::GUARD_LOGIN_AUTHORIZATION_REQUIRED,
                ))?;
                if prepared.registration() != registration {
                    return Err(Error::Invalid("GUARD_LOGIN_REGISTRATION_CHANGED"));
                }
                prepared.register()
            },
            Registration::remove_idle,
        )
    }
    fn execute_with(
        self,
        exists: impl FnOnce(&Registration) -> Result<bool>,
        register: impl FnOnce(&Registration) -> Result<()>,
        remove: impl FnOnce(&Registration) -> Result<()>,
    ) -> Result<Completion> {
        match self.request.action {
            Action::Create => {
                if !exists(&self.registration)? {
                    if !self.create_allowed {
                        return Err(Error::Invalid("GUARD_LOGIN_NOT_NEEDED"));
                    }
                    register(&self.registration)?;
                }
            }
            Action::Remove => remove(&self.registration)?,
        }
        Ok(Completion { job: self })
    }
}

impl Store {
    pub fn login_request_status(&self, id: Uuid) -> Result<Option<Status>> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        Ok(self
            .read_login()?
            .entries
            .iter()
            .find(|e| e.request(id).is_some())
            .map(|e| e.status(id)))
    }
    /// Historical registration, not a claim that Task Scheduler still matches.
    pub fn login_registration(&self) -> Result<Option<(Request, Status, Registration)>> {
        Ok(self
            .read_login()?
            .entries
            .into_iter()
            .find(|e| e.removed_revision.is_none())
            .map(|e| {
                let request = e.removal.as_ref().unwrap_or(&e.create).clone();
                let status = e.status(request.id);
                (request, status, e.registration)
            }))
    }
    pub fn begin_login(
        &mut self,
        request: &Request,
        prepared: Option<&Prepared<'_>>,
    ) -> Result<Preparation> {
        self.begin_login_record(request, prepared.map(|p| p.registration().clone()))
    }
    fn begin_login_record(
        &mut self,
        request: &Request,
        plan: Option<Registration>,
    ) -> Result<Preparation> {
        valid_request(request)?;
        if let Some(entry) = self
            .read_login()?
            .entries
            .iter()
            .find(|e| e.request(request.id).is_some())
        {
            if entry.request(request.id) != Some(request) {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
            let status = entry.status(request.id);
            if !matches!(status, Status::Pending { .. }) {
                return Ok(Preparation::Complete(status));
            }
        }
        let lease = self.login_lease()?;
        self.recover_config_requests()?;
        let mut journal = self.read_login()?;
        if let Some(entry) = journal
            .entries
            .iter()
            .find(|e| e.request(request.id).is_some())
        {
            if entry.request(request.id) != Some(request) {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
            return self.login_preparation(&journal, request.id, lease);
        }
        self.ensure_request_id_unused_elsewhere(request.id, RequestJournal::Login)?;
        self.ensure_core_update_idle()?;
        let manifest = self.load()?;
        if manifest.revision != request.expected_revision {
            return Err(Error::Invalid("STALE_MANIFEST_REVISION"));
        }
        let active = journal
            .entries
            .iter()
            .position(|e| e.removed_revision.is_none());
        match request.action {
            Action::Create => {
                if active.is_some() || manifest.integrations.guard_login_task.is_some() {
                    return Err(Error::Invalid("GUARD_LOGIN_ALREADY_REGISTERED"));
                }
                if !manifest
                    .instances
                    .iter()
                    .any(|i| i.guard.desired == Desired::Enabled)
                {
                    return Err(Error::Invalid("GUARD_LOGIN_NOT_NEEDED"));
                }
                let registration = plan.ok_or(Error::Invalid(
                    app_proxy_core::error_code::GUARD_LOGIN_AUTHORIZATION_REQUIRED,
                ))?;
                registration.spec()?;
                if registration.store_id != manifest.store_id
                    || std::fs::canonicalize(&registration.home)?
                        != std::fs::canonicalize(self.root())?
                {
                    return Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"));
                }
                let cutoff = now()?.saturating_sub(RETENTION);
                journal
                    .entries
                    .retain(|e| e.removed_at.is_none_or(|t| t >= cutoff));
                if journal.entries.len() >= ENTRIES {
                    return Err(Error::Invalid("GUARD_LOGIN_RECORD_LIMIT"));
                }
                journal.entries.push(Entry {
                    create: request.clone(),
                    registration,
                    created_revision: None,
                    removal: None,
                    removed_revision: None,
                    removed_at: None,
                });
                reserve_capacity(&journal)?;
            }
            Action::Remove => {
                let entry = &mut journal.entries
                    [active.ok_or(Error::Invalid("GUARD_LOGIN_OWNERSHIP_UNAVAILABLE"))?];
                if request.expected_creation != Some(entry.create.id) {
                    return Err(Error::Invalid("GUARD_LOGIN_REGISTRATION_CHANGED"));
                }
                if entry.removal.is_some() {
                    return Err(Error::Invalid("GUARD_LOGIN_OPERATION_PENDING"));
                }
                entry.removal = Some(request.clone());
            }
        }
        self.write_login(&journal)?;
        self.login_preparation(&journal, request.id, lease)
    }
    pub fn resume_login(&mut self, id: Uuid) -> Result<Preparation> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        let journal = self.read_login()?;
        let entry = journal
            .entries
            .iter()
            .find(|e| e.request(id).is_some())
            .ok_or(Error::Invalid("GUARD_LOGIN_REQUEST_NOT_FOUND"))?;
        let status = entry.status(id);
        if !matches!(status, Status::Pending { .. }) {
            return Ok(Preparation::Complete(status));
        }
        let lease = self.login_lease()?;
        self.recover_config_requests()?;
        self.login_preparation(&self.read_login()?, id, lease)
    }
    fn login_preparation(&self, journal: &Journal, id: Uuid, lease: File) -> Result<Preparation> {
        let entry = journal
            .entries
            .iter()
            .find(|e| e.request(id).is_some())
            .ok_or(Error::Invalid("GUARD_LOGIN_REQUEST_NOT_FOUND"))?;
        let status = entry.status(id);
        if !matches!(status, Status::Pending { .. }) {
            return Ok(Preparation::Complete(status));
        }
        if entry.removal.as_ref().is_some_and(|r| r.id != id) {
            return Err(Error::Invalid("GUARD_LOGIN_REMOVAL_PENDING"));
        }
        self.ensure_core_update_idle()?;
        Ok(Preparation::Pending(Job {
            request: entry.request(id).unwrap().clone(),
            registration: entry.registration.clone(),
            create_allowed: self
                .load()?
                .instances
                .iter()
                .any(|i| i.guard.desired == Desired::Enabled),
            _operation: lease,
            _owner: self.owner_lease(),
        }))
    }
    pub fn complete_login(&mut self, completion: Completion) -> Result<Status> {
        self.complete_login_with(completion, || Ok(()))
    }
    fn complete_login_with(
        &mut self,
        completion: Completion,
        checkpoint: impl FnOnce() -> Result<()>,
    ) -> Result<Status> {
        let job = &completion.job;
        self.recover_config_requests()?;
        self.ensure_core_update_idle()?;
        let mut journal = self.read_login()?;
        let entry = journal
            .entries
            .iter_mut()
            .find(|e| e.request(job.request.id).is_some())
            .ok_or(Error::Invalid("GUARD_LOGIN_REQUEST_NOT_FOUND"))?;
        if entry.request(job.request.id) != Some(&job.request)
            || entry.registration != job.registration
        {
            return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
        }
        if !matches!(entry.status(job.request.id), Status::Pending { .. }) {
            return Ok(entry.status(job.request.id));
        }
        if entry
            .removal
            .as_ref()
            .is_some_and(|r| r.id != job.request.id)
        {
            return Err(Error::Invalid("GUARD_LOGIN_REMOVAL_PENDING"));
        }
        let mut manifest = self.load()?;
        let expected = entry.registration.metadata()?;
        if manifest
            .integrations
            .guard_login_task
            .as_ref()
            .is_some_and(|t| !same_metadata(t, &expected))
        {
            return Err(Error::Invalid("GUARD_LOGIN_METADATA_CONFLICT"));
        }
        let present = manifest.integrations.guard_login_task.is_some();
        let create = job.request.action == Action::Create;
        let revision = if create != present {
            manifest.integrations.guard_login_task = if create { Some(expected) } else { None };
            self.commit(manifest.revision, manifest)?
        } else {
            manifest.revision
        };
        checkpoint()?;
        if create {
            entry.created_revision = Some(revision);
        } else {
            entry.removed_revision = Some(revision);
            entry.removed_at = Some(now()?);
        }
        let status = entry.status(job.request.id);
        self.write_login(&journal)?;
        Ok(status)
    }
    fn login_lease(&self) -> Result<File> {
        let path = self.root().join("state/login-operation.lock");
        storage_security::no_reparse(&self.root().join("state"))?;
        if path.try_exists()? {
            storage_security::no_reparse(&path)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        storage_security::verify(
            file.as_raw_handle(),
            &store::describe(self.root())?.owner_sid,
            false,
        )?;
        file.try_lock()
            .map_err(|_| Error::Invalid("GUARD_LOGIN_BUSY"))?;
        Ok(file)
    }
    fn read_login(&self) -> Result<Journal> {
        let owner = store::describe(self.root())?;
        let bytes = match store::read_protected(&self.root().join(PATH), &owner.owner_sid, LIMIT) {
            Ok(b) => b,
            Err(Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Journal {
                    version: 1,
                    store_id: owner.store_id,
                    entries: vec![],
                });
            }
            Err(e) => return Err(e),
        };
        let journal: Journal = store::decode(&bytes)?;
        validate(&journal, owner.store_id, self.load()?.revision)?;
        Ok(journal)
    }
    fn write_login(&self, journal: &Journal) -> Result<()> {
        validate(
            journal,
            store::describe(self.root())?.store_id,
            self.load()?.revision,
        )?;
        self.replace_bounded(PATH, &store::encode(journal, LIMIT)?, LIMIT)
    }
}
fn same_metadata(a: &LoginTask, b: &LoginTask) -> bool {
    a.name == b.name && a.target == b.target && a.args == b.args
}
fn valid_request(r: &Request) -> Result<()> {
    if r.id.is_nil()
        || r.expected_revision == 0
        || match r.action {
            Action::Create => r.expected_creation.is_some(),
            Action::Remove => r.expected_creation.is_none_or(|id| id.is_nil()),
        }
    {
        return Err(Error::Invalid("INVALID_GUARD_LOGIN_REQUEST"));
    }
    Ok(())
}
fn validate(journal: &Journal, store: Uuid, current: u64) -> Result<()> {
    if journal.version != 1 || journal.store_id != store || journal.entries.len() > ENTRIES {
        return Err(Error::Invalid("GUARD_LOGIN_JOURNAL_INVALID"));
    }
    let mut ids = HashSet::new();
    let mut active = 0;
    for e in &journal.entries {
        valid_request(&e.create)?;
        e.registration.spec()?;
        if e.create.action != Action::Create
            || e.registration.store_id != store
            || !ids.insert(e.create.id)
            || e.create.expected_revision > current
            || e.created_revision
                .is_some_and(|r| r <= e.create.expected_revision || r > current)
            || e.removed_revision.is_some() != e.removed_at.is_some()
            || e.removed_revision.is_some() && e.removal.is_none()
        {
            return Err(Error::Invalid("GUARD_LOGIN_JOURNAL_INVALID"));
        }
        if let Some(remove) = &e.removal {
            valid_request(remove)?;
            if remove.action != Action::Remove
                || remove.expected_creation != Some(e.create.id)
                || !ids.insert(remove.id)
                || remove.expected_revision > current
                || remove.expected_revision
                    < e.created_revision.unwrap_or(e.create.expected_revision)
                || e.removed_revision
                    .is_some_and(|r| r < remove.expected_revision || r > current)
            {
                return Err(Error::Invalid("GUARD_LOGIN_JOURNAL_INVALID"));
            }
        }
        if e.removed_revision.is_none() {
            active += 1;
        }
    }
    if active > 1 {
        return Err(Error::Invalid("GUARD_LOGIN_JOURNAL_INVALID"));
    }
    Ok(())
}
fn reserve_capacity(journal: &Journal) -> Result<()> {
    let mut complete = journal.clone();
    for e in &mut complete.entries {
        if e.removed_revision.is_none() {
            e.created_revision = Some(u64::MAX);
            e.removal = Some(Request {
                id: e.create.id,
                expected_revision: u64::MAX,
                action: Action::Remove,
                expected_creation: Some(e.create.id),
            });
            e.removed_revision = Some(u64::MAX);
            e.removed_at = Some(u64::MAX);
        }
    }
    store::encode(&complete, LIMIT).map_err(|_| Error::Invalid("GUARD_LOGIN_RECORD_LIMIT"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
