//! Restore only a missing, previously owned link at its recorded location.
//! Repair never reinterprets a historical Create as a new request or overwrites
//! a changed link. Every publication has a durable, independently queryable ID.
use super::*;

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Repair {
    pub request: Request,
    pub staged: Option<staging::Staged>,
    pub revision: Option<u64>,
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Unregistered,
    Pending,
    Verified,
    Missing,
    Blocked,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub instance_id: Uuid,
    pub revision: u64,
    pub state: CheckState,
    pub request_id: Option<Uuid>,
    pub diagnostic: Option<String>,
}

fn assets(store: &Store, entry: &Entry) -> Result<()> {
    if entry.plan.spec.home != store.root() {
        return Err(Error::Invalid("SHORTCUT_STORE_PATH_CHANGED"));
    }
    security::no_reparse(&entry.plan.spec.host)?;
    if !std::fs::metadata(&entry.plan.spec.host)?.is_file() {
        return Err(Error::Invalid("SHORTCUT_HOST_UNAVAILABLE"));
    }
    identity::file_identity(&entry.plan.spec.host)
        .map_err(|_| Error::Invalid("SHORTCUT_HOST_UNAVAILABLE"))?;
    let icon = &entry.plan.spec.icon;
    if icon.parent() != Some(store.root().join("state").as_path()) {
        return Err(Error::Invalid("SHORTCUT_ICON_UNAVAILABLE"));
    }
    let owner = store::describe(store.root())?;
    let bytes = store::read_protected(icon, &owner.owner_sid, 16 * 1024 * 1024)
        .map_err(|_| Error::Invalid("SHORTCUT_ICON_UNAVAILABLE"))?;
    let expected = format!("icon-{:x}.ico", Sha256::digest(&bytes));
    if icon.file_name() != Some(std::ffi::OsStr::new(&expected)) {
        return Err(Error::Invalid("SHORTCUT_ICON_CHANGED"));
    }
    Ok(())
}

fn metadata(store: &Store, entry: &Entry) -> Result<()> {
    let manifest = store.load()?;
    let expected = entry.metadata();
    if !manifest
        .integrations
        .shortcuts
        .iter()
        .any(|s| same_metadata(s, &expected))
    {
        return Err(Error::Invalid("SHORTCUT_METADATA_CONFLICT"));
    }
    Ok(())
}

impl Store {
    /// Read-only evidence; never resumes pending requests or repairs a file.
    pub fn check_shortcut(&self, instance_id: Uuid) -> Result<Check> {
        if instance_id.is_nil() {
            return Err(Error::Invalid("INVALID_SHORTCUT_REQUEST"));
        }
        let manifest = self.load()?;
        if !manifest.instances.iter().any(|i| i.id == instance_id) {
            return Err(Error::Invalid("INSTANCE_NOT_FOUND"));
        }
        let journal = self.read_shortcuts()?;
        let entry = journal
            .entries
            .iter()
            .find(|e| e.create.instance_id == instance_id && e.removed_revision.is_none());
        let mut view = Check {
            instance_id,
            revision: manifest.revision,
            state: CheckState::Unregistered,
            request_id: None,
            diagnostic: None,
        };
        let Some(entry) = entry else {
            if manifest
                .integrations
                .shortcuts
                .iter()
                .any(|s| s.instance_id == instance_id)
            {
                view.state = CheckState::Blocked;
                view.diagnostic = Some("SHORTCUT_OWNERSHIP_UNAVAILABLE".into());
            }
            return Ok(view);
        };
        let pending = entry
            .removal
            .as_ref()
            .or_else(|| {
                entry
                    .repairs
                    .last()
                    .filter(|r| r.revision.is_none())
                    .map(|r| &r.request)
            })
            .or_else(|| entry.created_revision.is_none().then_some(&entry.create));
        if let Some(request) = pending {
            view.state = CheckState::Pending;
            view.request_id = Some(request.id);
            return Ok(view);
        }
        view.request_id = Some(entry.create.id);
        let checked = (|| {
            metadata(self, entry)?;
            assets(self, entry)?;
            present(
                &entry.plan.path,
                &entry.plan.spec,
                &entry.staged.as_ref().unwrap().receipt,
            )
        })();
        match checked {
            Ok(true) => view.state = CheckState::Verified,
            Ok(false) => view.state = CheckState::Missing,
            Err(error) => {
                view.state = CheckState::Blocked;
                view.diagnostic = Some(
                    match error {
                        Error::Invalid(code) => code,
                        _ => "SHORTCUT_CHECK_FAILED",
                    }
                    .into(),
                );
            }
        }
        Ok(view)
    }

    pub(super) fn resume_shortcut_repair(
        &mut self,
        mut journal: Journal,
        index: usize,
        id: Uuid,
        checkpoint: impl Fn(Point) -> Result<()>,
    ) -> Result<Status> {
        self.ensure_core_update_idle()?;
        let slot = journal.entries[index]
            .repairs
            .iter()
            .position(|r| r.request.id == id)
            .unwrap();
        let entry = &journal.entries[index];
        metadata(self, entry)?;
        assets(self, entry)?;
        if entry.removal.is_some() || entry.created_revision.is_none() {
            return Err(Error::Invalid("SHORTCUT_OPERATION_PENDING"));
        }
        if let Some(staged) = &entry.repairs[slot].staged {
            let target = present(&entry.plan.path, &entry.plan.spec, &staged.receipt)?;
            let temporary = staging::present(staged, &entry.plan.spec)?;
            if target && temporary {
                return Err(Error::Invalid("SHORTCUT_LOCATIONS_CONFLICT"));
            }
            if !target && !temporary {
                journal.entries[index].repairs[slot].staged = None;
                self.write_shortcuts(&journal)?;
            }
        }
        let entry = &journal.entries[index];
        if entry.repairs[slot].staged.is_none() {
            let old = entry.staged.as_ref().unwrap();
            // An intact link needs no filesystem write. A foreign replacement
            // fails exact identity/content verification even if it looks alike.
            if present(&entry.plan.path, &entry.plan.spec, &old.receipt)? {
                journal.entries[index].repairs[slot].staged = Some(old.clone());
            } else {
                let bytes = encode(&entry.plan.spec)?;
                let staged = staging::prepare(&entry.plan.path, &entry.plan.spec, &bytes)?;
                checkpoint(Point::Staged)?;
                journal.entries[index].repairs[slot].staged = Some(staged);
            }
            self.write_shortcuts(&journal)?;
            checkpoint(Point::StageRecorded)?;
        }
        let entry = &journal.entries[index];
        let staged = entry.repairs[slot].staged.as_ref().unwrap();
        if !present(&entry.plan.path, &entry.plan.spec, &staged.receipt)? {
            staging::publish(staged, &entry.plan.path, &entry.plan.spec)?;
        }
        checkpoint(Point::Published)?;
        metadata(self, entry)?;
        journal.entries[index].staged = Some(staged.clone());
        journal.entries[index].repairs[slot].revision = Some(self.load()?.revision);
        self.write_shortcuts(&journal)?;
        checkpoint(Point::Completed)?;
        Ok(journal.entries[index].status(id))
    }
}

pub(super) fn begin(
    store: &Store,
    journal: &mut Journal,
    index: usize,
    request: &Request,
) -> Result<()> {
    if journal
        .entries
        .iter()
        .map(|e| e.repairs.len())
        .sum::<usize>()
        >= ENTRY_LIMIT
    {
        return Err(Error::Invalid("SHORTCUT_RECORD_LIMIT"));
    }
    let entry = &mut journal.entries[index];
    if request.expected_creation != Some(entry.create.id) {
        return Err(Error::Invalid("SHORTCUT_REGISTRATION_CHANGED"));
    }
    if entry.created_revision.is_none()
        || entry.removal.is_some()
        || entry.repairs.iter().any(|r| r.revision.is_none())
    {
        return Err(Error::Invalid("SHORTCUT_OPERATION_PENDING"));
    }
    metadata(store, entry)?;
    assets(store, entry)?;
    present(
        &entry.plan.path,
        &entry.plan.spec,
        &entry.staged.as_ref().unwrap().receipt,
    )?;
    entry.repairs.push(Repair {
        request: request.clone(),
        staged: None,
        revision: None,
    });
    reserve_completion_capacity(journal)
}

pub(super) fn validate(entry: &Entry, ids: &mut HashSet<Uuid>) -> Result<()> {
    if entry.repairs.len() > ENTRY_LIMIT {
        return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
    }
    for (index, repair) in entry.repairs.iter().enumerate() {
        let r = &repair.request;
        if r.id.is_nil()
            || !ids.insert(r.id)
            || r.action != Action::Repair
            || r.instance_id != entry.create.instance_id
            || r.expected_revision == 0
            || r.expected_creation != Some(entry.create.id)
            || entry.created_revision.is_none()
            || repair.revision == Some(0)
            || (repair.revision.is_some() && repair.staged.is_none())
            || (repair.revision.is_none()
                && (index + 1 != entry.repairs.len() || entry.removal.is_some()))
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
        }
        if let Some(staged) = &repair.staged {
            staging::stage_path(&staged.path)?;
            if staged.path.parent() != entry.plan.path.parent() {
                return Err(Error::Invalid("SHORTCUT_JOURNAL_INVALID"));
            }
        }
    }
    Ok(())
}

pub(super) fn validate_revisions(entry: &Entry, current: u64) -> Result<()> {
    let mut previous = entry
        .created_revision
        .unwrap_or(entry.create.expected_revision);
    for repair in &entry.repairs {
        if repair.request.expected_revision < previous
            || repair.request.expected_revision > current
            || repair
                .revision
                .is_some_and(|r| r < repair.request.expected_revision || r > current)
        {
            return Err(Error::Invalid("SHORTCUT_JOURNAL_REVISION_INVALID"));
        }
        previous = repair.revision.unwrap_or(repair.request.expected_revision);
    }
    if entry
        .removal
        .as_ref()
        .is_some_and(|r| r.expected_revision < previous)
    {
        return Err(Error::Invalid("SHORTCUT_JOURNAL_REVISION_INVALID"));
    }
    Ok(())
}

pub(super) fn reserve_capacity(entry: &mut Entry) {
    for repair in &mut entry.repairs {
        if repair.revision.is_none() {
            repair.revision = Some(u64::MAX);
            repair.staged = entry.staged.clone();
        }
    }
}
