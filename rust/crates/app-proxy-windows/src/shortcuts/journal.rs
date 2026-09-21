//! Persistent shortcut ownership and explicit recovery. Completed creation
//! records remain for as long as the integration exists, not just request TTL.
use super::*;
use crate::journal::{RETENTION, RequestJournal, now};
use crate::store::{self, Store};
use app_proxy_core::{
    model::{Manifest, Shortcut},
    registry::ConfigAction,
};
use std::collections::HashSet;

const PATH: &str = "state/shortcuts.json";
const JOURNAL_LIMIT: usize = 8 * 1024 * 1024;
const ENTRY_LIMIT: usize = 1024;

mod repair;
pub use repair::{Check, CheckState};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Create,
    Remove,
    Repair,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub expected_revision: u64,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_creation: Option<Uuid>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub path: PathBuf,
    pub spec: Spec,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Status {
    Pending { action: Action, path: PathBuf },
    Created { path: PathBuf, revision: u64 },
    Removed { path: PathBuf, revision: u64 },
    Cancelled { path: PathBuf },
    Repaired { path: PathBuf, revision: u64 },
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
    plan: Plan,
    staged: Option<staging::Staged>,
    created_revision: Option<u64>,
    removal: Option<Request>,
    removed_revision: Option<u64>,
    removed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    repairs: Vec<repair::Repair>,
}
impl Entry {
    fn request(&self, id: Uuid) -> Option<&Request> {
        if self.create.id == id {
            Some(&self.create)
        } else {
            self.removal.as_ref().filter(|r| r.id == id).or_else(|| {
                self.repairs
                    .iter()
                    .find(|r| r.request.id == id)
                    .map(|r| &r.request)
            })
        }
    }
    fn status(&self, id: Uuid) -> Status {
        let path = self.plan.path.clone();
        if let Some(repair) = self.repairs.iter().find(|r| r.request.id == id) {
            return match repair.revision {
                Some(revision) => Status::Repaired { path, revision },
                None => Status::Pending {
                    action: Action::Repair,
                    path,
                },
            };
        }
        if self.create.id == id {
            if let Some(revision) = self.created_revision {
                Status::Created { path, revision }
            } else if self.removed_revision.is_some() {
                Status::Cancelled { path }
            } else {
                Status::Pending {
                    action: Action::Create,
                    path,
                }
            }
        } else if let Some(revision) = self.removed_revision {
            Status::Removed { path, revision }
        } else {
            Status::Pending {
                action: Action::Remove,
                path,
            }
        }
    }
    fn metadata(&self) -> Shortcut {
        Shortcut {
            instance_id: self.create.instance_id,
            path: self.plan.path.clone(),
            target: self.plan.spec.host.clone(),
            args: vec![
                "launch".into(),
                self.create.instance_id.to_string(),
                "--home".into(),
                self.plan.spec.home.to_string_lossy().into_owned(),
                "--notify".into(),
            ],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Point {
    Accepted,
    Staged,
    StageRecorded,
    Published,
    ManifestCommitted,
    Completed,
}

impl Store {
    /// Plan is used only for a new Create. A replay uses the durable original
    /// plan, even after an application update or display-name change.
    pub fn apply_shortcut(&mut self, request: &Request, plan: Option<Plan>) -> Result<Status> {
        self.apply_shortcut_with(request, plan, |_| Ok(()))
    }
    fn apply_shortcut_with(
        &mut self,
        request: &Request,
        plan: Option<Plan>,
        checkpoint: impl Fn(Point) -> Result<()>,
    ) -> Result<Status> {
        self.begin_shortcut(request, plan)?;
        checkpoint(Point::Accepted)?;
        self.resume_shortcut_with(request.id, checkpoint)
    }
    pub fn shortcut_request_status(&self, id: Uuid) -> Result<Option<Status>> {
        if id.is_nil() {
            return Err(Error::Invalid("INVALID_REQUEST_ID"));
        }
        Ok(self
            .read_shortcuts()?
            .entries
            .iter()
            .find(|e| e.request(id).is_some())
            .map(|e| e.status(id)))
    }
    pub fn resume_shortcut(&mut self, id: Uuid) -> Result<Status> {
        self.resume_shortcut_with(id, |_| Ok(()))
    }
    /// Current owned integration, including interrupted work. Creation receipts
    /// are historical; this query does not inspect or change desktop files.
    pub fn instance_shortcut(&self, instance_id: Uuid) -> Result<Option<(Request, Status)>> {
        if instance_id.is_nil() {
            return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
        }
        Ok(self
            .read_shortcuts()?
            .entries
            .into_iter()
            .find(|e| e.create.instance_id == instance_id && e.removed_revision.is_none())
            .map(|e| {
                let request = e
                    .removal
                    .as_ref()
                    .or_else(|| {
                        e.repairs
                            .last()
                            .filter(|r| r.revision.is_none())
                            .map(|r| &r.request)
                    })
                    .unwrap_or(&e.create)
                    .clone();
                let status = e.status(request.id);
                (request, status)
            }))
    }
    pub(crate) fn shortcut_edit_rejection(
        &self,
        action: &ConfigAction,
    ) -> Result<Option<&'static str>> {
        if let ConfigAction::RemoveInstance { instance_id } = action
            && self
                .read_shortcuts()?
                .entries
                .iter()
                .any(|e| e.create.instance_id == *instance_id && e.removed_revision.is_none())
        {
            return Ok(Some("INTEGRATION_CLEANUP_REQUIRED"));
        }
        Ok(None)
    }
    pub(crate) fn ensure_shortcut_instances(&self, target: &Manifest) -> Result<()> {
        if self.read_shortcuts()?.entries.iter().any(|e| {
            e.removed_revision.is_none()
                && !target
                    .instances
                    .iter()
                    .any(|i| i.id == e.create.instance_id)
        }) {
            return Err(Error::Invalid("INTEGRATION_CLEANUP_REQUIRED"));
        }
        Ok(())
    }

    fn begin_shortcut(&mut self, request: &Request, plan: Option<Plan>) -> Result<()> {
        if request.id.is_nil() || request.instance_id.is_nil() || request.expected_revision == 0 {
            return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
        }
        // Existing durable pure edits must finish before reserving the instance.
        self.recover_config_requests()?;
        let mut journal = self.read_shortcuts()?;
        if let Some(prior) = journal.entries.iter().find_map(|e| e.request(request.id)) {
            return if prior == request {
                Ok(())
            } else {
                Err(Error::Invalid("REQUEST_ID_CONFLICT"))
            };
        }
        self.ensure_request_id_unused_elsewhere(request.id, RequestJournal::Shortcut)?;
        self.ensure_core_update_idle()?;
        let manifest = self.load()?;
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
        let existing = journal.entries.iter().position(|e| {
            e.create.instance_id == request.instance_id && e.removed_revision.is_none()
        });
        match request.action {
            Action::Create => {
                if request.expected_creation.is_some() {
                    return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
                }
                if existing.is_some()
                    || manifest
                        .integrations
                        .shortcuts
                        .iter()
                        .any(|s| s.instance_id == request.instance_id)
                {
                    return Err(Error::Invalid("SHORTCUT_ALREADY_REGISTERED"));
                }
                let plan = plan.ok_or(Error::Invalid("SHORTCUT_PLAN_REQUIRED"))?;
                validate_plan(&plan, manifest.store_id, request.instance_id)?;
                if plan.spec.home != self.root() {
                    return Err(Error::Invalid("SHORTCUT_STORE_PATH_CHANGED"));
                }
                if journal
                    .entries
                    .iter()
                    .any(|e| e.removed_revision.is_none() && e.plan.path == plan.path)
                {
                    return Err(Error::Invalid("SHORTCUT_PATH_REGISTERED"));
                }
                let now = now()?;
                journal.entries.retain(|e| {
                    e.removed_at
                        .is_none_or(|at| now.saturating_sub(at) < RETENTION)
                });
                if journal.entries.len() >= ENTRY_LIMIT {
                    return Err(Error::Invalid("SHORTCUT_RECORD_LIMIT"));
                }
                // Refuse obvious conflicts before accepting any external work.
                let _parents = parent(&plan.path)?;
                match std::fs::symlink_metadata(&plan.path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.into()),
                    Ok(_) => return Err(Error::Invalid("SHORTCUT_PATH_OCCUPIED")),
                }
                journal.entries.push(Entry {
                    create: request.clone(),
                    plan,
                    staged: None,
                    created_revision: None,
                    removal: None,
                    removed_revision: None,
                    removed_at: None,
                    repairs: vec![],
                });
                reserve_completion_capacity(&journal)?;
            }
            Action::Remove => {
                let index = existing.ok_or(Error::Invalid("SHORTCUT_OWNERSHIP_UNAVAILABLE"))?;
                if request
                    .expected_creation
                    .is_some_and(|id| id != journal.entries[index].create.id)
                {
                    return Err(Error::Invalid("SHORTCUT_REGISTRATION_CHANGED"));
                }
                if journal.entries[index].removal.is_some() {
                    return Err(Error::Invalid("SHORTCUT_OPERATION_PENDING"));
                }
                if journal.entries[index]
                    .repairs
                    .iter()
                    .any(|r| r.revision.is_none())
                {
                    return Err(Error::Invalid("SHORTCUT_OPERATION_PENDING"));
                }
                journal.entries[index].removal = Some(request.clone());
            }
            Action::Repair => {
                let index = existing.ok_or(Error::Invalid("SHORTCUT_OWNERSHIP_UNAVAILABLE"))?;
                repair::begin(self, &mut journal, index, request)?;
            }
        }
        self.write_shortcuts(&journal)
    }

    fn resume_shortcut_with(
        &mut self,
        id: Uuid,
        checkpoint: impl Fn(Point) -> Result<()>,
    ) -> Result<Status> {
        self.recover_config_requests()?;
        let mut journal = self.read_shortcuts()?;
        let index = journal
            .entries
            .iter()
            .position(|e| e.request(id).is_some())
            .ok_or(Error::Invalid("SHORTCUT_REQUEST_NOT_FOUND"))?;
        if !matches!(journal.entries[index].status(id), Status::Pending { .. }) {
            return Ok(journal.entries[index].status(id));
        }
        if journal.entries[index]
            .repairs
            .iter()
            .any(|r| r.request.id == id)
        {
            return self.resume_shortcut_repair(journal, index, id, checkpoint);
        }
        // Resuming a cancelled creation must never act as permission to remove.
        if journal.entries[index]
            .removal
            .as_ref()
            .is_some_and(|r| r.id != id)
        {
            return Err(Error::Invalid("SHORTCUT_REMOVAL_PENDING"));
        }
        self.ensure_core_update_idle()?;
        if journal.entries[index].removal.is_none() {
            // Both missing permits preparing a new identity. Keep the old record
            // until absence is established under pinned directory chains.
            if let Some(staged) = &journal.entries[index].staged {
                let entry = &journal.entries[index];
                let target = present(&entry.plan.path, &entry.plan.spec, &staged.receipt)?;
                let temporary = staging::present(staged, &entry.plan.spec)?;
                if target && temporary {
                    return Err(Error::Invalid("SHORTCUT_LOCATIONS_CONFLICT"));
                }
                if !target && !temporary {
                    journal.entries[index].staged = None;
                    self.write_shortcuts(&journal)?;
                }
            }
            if journal.entries[index].staged.is_none() {
                let entry = &journal.entries[index];
                let bytes = encode(&entry.plan.spec)?;
                let staged = staging::prepare(&entry.plan.path, &entry.plan.spec, &bytes)?;
                checkpoint(Point::Staged)?;
                journal.entries[index].staged = Some(staged);
                self.write_shortcuts(&journal)?;
                checkpoint(Point::StageRecorded)?;
            }
            let entry = &journal.entries[index];
            let staged = entry.staged.as_ref().unwrap();
            if !present(&entry.plan.path, &entry.plan.spec, &staged.receipt)? {
                staging::publish(staged, &entry.plan.path, &entry.plan.spec)?;
            }
            checkpoint(Point::Published)?;
            let mut manifest = self.load()?;
            let expected = entry.metadata();
            let revision = match manifest
                .integrations
                .shortcuts
                .iter()
                .find(|s| s.path == expected.path || s.instance_id == expected.instance_id)
            {
                Some(s) if same_metadata(s, &expected) => manifest.revision,
                Some(_) => return Err(Error::Invalid("SHORTCUT_METADATA_CONFLICT")),
                None => {
                    manifest.integrations.shortcuts.push(expected);
                    self.commit(manifest.revision, manifest)?
                }
            };
            checkpoint(Point::ManifestCommitted)?;
            journal.entries[index].created_revision = Some(revision);
        } else {
            let entry = &journal.entries[index];
            if let Some(staged) = &entry.staged {
                let target = present(&entry.plan.path, &entry.plan.spec, &staged.receipt)?;
                let temporary =
                    entry.created_revision.is_none() && staging::present(staged, &entry.plan.spec)?;
                if target && temporary {
                    return Err(Error::Invalid("SHORTCUT_LOCATIONS_CONFLICT"));
                }
                if target {
                    super::remove(&entry.plan.path, &entry.plan.spec, &staged.receipt)?;
                }
                if temporary {
                    staging::remove(staged, &entry.plan.spec)?;
                }
            }
            // No staged receipt means publication was never authorized. A later
            // foreign file at the target is not ours and is deliberately retained.
            checkpoint(Point::Published)?;
            let mut manifest = self.load()?;
            let expected = entry.metadata();
            let revision = match manifest
                .integrations
                .shortcuts
                .iter()
                .position(|s| s.path == expected.path || s.instance_id == expected.instance_id)
            {
                Some(position)
                    if same_metadata(&manifest.integrations.shortcuts[position], &expected) =>
                {
                    manifest.integrations.shortcuts.remove(position);
                    self.commit(manifest.revision, manifest)?
                }
                Some(_) => return Err(Error::Invalid("SHORTCUT_METADATA_CONFLICT")),
                None => manifest.revision,
            };
            checkpoint(Point::ManifestCommitted)?;
            journal.entries[index].removed_revision = Some(revision);
            journal.entries[index].removed_at = Some(now()?);
        }
        self.write_shortcuts(&journal)?;
        checkpoint(Point::Completed)?;
        Ok(journal.entries[index].status(id))
    }

    fn read_shortcuts(&self) -> Result<Journal> {
        let owner = store::describe(self.root())?;
        let Some(journal) = self.read_journal::<Journal>(PATH, &owner.owner_sid, JOURNAL_LIMIT)?
        else {
            return Ok(Journal {
                version: 1,
                store_id: owner.store_id,
                entries: vec![],
            });
        };
        validate(&journal, owner.store_id)?;
        validate_revisions(&journal, self.load()?.revision)?;
        Ok(journal)
    }
    fn write_shortcuts(&self, journal: &Journal) -> Result<()> {
        validate(journal, store::describe(self.root())?.store_id)?;
        validate_revisions(journal, self.load()?.revision)?;
        self.write_journal(PATH, journal, JOURNAL_LIMIT)
    }
}

fn same_metadata(a: &Shortcut, b: &Shortcut) -> bool {
    a.instance_id == b.instance_id && a.path == b.path && a.target == b.target && a.args == b.args
}
fn validate_plan(plan: &Plan, store: Uuid, instance: Uuid) -> Result<()> {
    plan.spec.validate()?;
    absolute(&plan.path)?;
    if plan.spec.store_id != store
        || plan.spec.instance_id != instance
        || !plan
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("lnk"))
    {
        return Err(Error::Invalid("SHORTCUT_PLAN_INVALID"));
    }
    Ok(())
}
fn validate(journal: &Journal, store: Uuid) -> Result<()> {
    if journal.version != 1
        || journal.store_id != store
        || journal.entries.len() > ENTRY_LIMIT
        || journal
            .entries
            .iter()
            .map(|e| e.repairs.len())
            .sum::<usize>()
            > ENTRY_LIMIT
    {
        return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
    }
    let mut ids = HashSet::new();
    let mut active = HashSet::new();
    let mut paths = HashSet::new();
    for entry in &journal.entries {
        let create = &entry.create;
        if create.id.is_nil()
            || create.instance_id.is_nil()
            || create.expected_revision == 0
            || create.action != Action::Create
            || create.expected_creation.is_some()
            || !ids.insert(create.id)
            || entry.created_revision == Some(0)
            || entry.removed_revision == Some(0)
            || entry.created_revision.is_some() && entry.staged.is_none()
            || entry.removed_revision.is_some() != entry.removed_at.is_some()
            || entry.removed_revision.is_some() && entry.removal.is_none()
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
        }
        validate_plan(&entry.plan, store, create.instance_id)?;
        repair::validate(entry, &mut ids)?;
        if let Some(staged) = &entry.staged {
            staging::stage_path(&staged.path)?;
            if staged.path.parent() != entry.plan.path.parent() {
                return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
            }
        }
        if let Some(remove) = &entry.removal
            && (remove.id.is_nil()
                || remove.instance_id != create.instance_id
                || remove.expected_revision == 0
                || remove.action != Action::Remove
                || remove.expected_creation.is_some_and(|id| id != create.id)
                || !ids.insert(remove.id))
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
        }
        if entry.removed_revision.is_none()
            && (!active.insert(create.instance_id) || !paths.insert(&entry.plan.path))
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
        }
    }
    Ok(())
}
fn validate_revisions(journal: &Journal, current: u64) -> Result<()> {
    for entry in &journal.entries {
        repair::validate_revisions(entry, current)?;
        if entry.create.expected_revision > current
            || entry
                .created_revision
                .is_some_and(|r| r > current || r <= entry.create.expected_revision)
            || entry.removal.as_ref().is_some_and(|r| {
                r.expected_revision > current
                    || r.expected_revision
                        < entry
                            .created_revision
                            .unwrap_or(entry.create.expected_revision)
            })
            || entry.removed_revision.is_some_and(|r| {
                r > current
                    || r < entry.removal.as_ref().unwrap().expected_revision
                    || r < entry
                        .created_revision
                        .unwrap_or(entry.create.expected_revision)
            })
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_REVISION_INVALID"));
        }
    }
    Ok(())
}
// Reserve the fully populated serialized state of every active entry before
// accepting another creation. Numeric values use their maximum widths; stage
// basenames are the fixed ASCII prefix + 16 random characters + suffix.
fn reserve_completion_capacity(journal: &Journal) -> Result<()> {
    let mut complete = journal.clone();
    for entry in &mut complete.entries {
        if entry.removed_revision.is_none() {
            let path = entry
                .plan
                .path
                .parent()
                .ok_or(Error::Invalid("SHORTCUT_PLAN_INVALID"))?
                .join(".app-proxy-stage-XXXXXXXXXXXXXXXX.tmp");
            let receipt = Receipt {
                file: FileIdentity {
                    volume_serial: u32::MAX,
                    file_index: u64::MAX,
                },
                sha256: [255; 32],
            };
            let mut staged = staging::Staged {
                path,
                receipt: receipt.clone(),
            };
            if let Some(prior) = &entry.staged
                && store::encode(&prior.path, JOURNAL_LIMIT)?.len()
                    > store::encode(&staged.path, JOURNAL_LIMIT)?.len()
            {
                staged.path = prior.path.clone();
            }
            entry.staged = Some(staged);
            entry.created_revision = Some(u64::MAX);
            entry.removal = Some(Request {
                id: entry.create.id,
                instance_id: entry.create.instance_id,
                expected_revision: u64::MAX,
                action: Action::Remove,
                expected_creation: Some(entry.create.id),
            });
            entry.removed_revision = Some(u64::MAX);
            entry.removed_at = Some(u64::MAX);
            repair::reserve_capacity(entry);
        }
    }
    store::encode(&complete, JOURNAL_LIMIT).map_err(|_| Error::Invalid("SHORTCUT_RECORD_LIMIT"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
