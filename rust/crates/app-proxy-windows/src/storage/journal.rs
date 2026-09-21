//! Pieces shared by every durable request journal in the store.
//!
//! Each journal keeps its own record types and rules; what they have in common
//! lives here so that one journal cannot drift from the others.
use crate::{Error, Result, store, store::Store};
use serde::{Serialize, de::DeserializeOwned};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use uuid::Uuid;

/// Finished requests answer replays for this long. Unresolved requests are
/// never pruned by age.
pub(crate) const RETENTION: u64 = 7 * 24 * 60 * 60;

/// Seconds since the Unix epoch.
pub(crate) fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .map_err(|_| Error::Invalid("SYSTEM_CLOCK_INVALID"))
}

/// IDs of the `<uuid>.json` records in a one-file-per-request directory.
///
/// Interrupted same-directory temporary writes (`.tmp*`) were never committed
/// and are skipped. Any other name is reported with the journal's own code and
/// nothing is touched: the listing completes before a caller acts on any record.
pub(crate) fn record_ids(directory: &Path, unknown: &'static str) -> Result<Vec<Uuid>> {
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let name = entry?.file_name();
        let text = name.to_str().ok_or(Error::Invalid(unknown))?;
        if text.starts_with(".tmp") {
            continue;
        }
        let id = text
            .strip_suffix(".json")
            .and_then(|stem| Uuid::parse_str(stem).ok())
            .ok_or(Error::Invalid(unknown))?;
        // Reject alternative spellings of the same UUID.
        if text != format!("{id}.json") {
            return Err(Error::Invalid(unknown));
        }
        ids.push(id);
    }
    Ok(ids)
}

impl Store {
    /// Reads a single-file journal. `None` means it was never written, which
    /// every journal treats as empty. The caller validates the content.
    pub(crate) fn read_journal<T: DeserializeOwned>(
        &self,
        relative: &str,
        owner_sid: &str,
        limit: usize,
    ) -> Result<Option<T>> {
        match store::read_protected(&self.root().join(relative), owner_sid, limit) {
            Ok(bytes) => store::decode(&bytes).map(Some),
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Replaces a single-file journal that the caller has already validated.
    pub(crate) fn write_journal<T: Serialize>(
        &self,
        relative: &str,
        journal: &T,
        limit: usize,
    ) -> Result<()> {
        self.replace_bounded(relative, &store::encode(journal, limit)?, limit)
    }
}

/// The points where the plain configuration store depends on its journals.
/// `store.rs` calls only these, so this module is the one place that knows
/// which journals exist. The journals call each other's gates directly, in the
/// order each operation needs.
impl Store {
    /// An accepted pure configuration edit is finished before the owner serves
    /// anything else.
    pub(crate) fn recover_journals_on_open(&mut self) -> Result<()> {
        self.recover_config_requests()
    }

    /// A direct commit must not interleave with a core reconfiguration plan or
    /// overtake an accepted configuration edit.
    pub(crate) fn ensure_commit_allowed(&mut self) -> Result<()> {
        self.ensure_core_update_idle()?;
        self.recover_config_requests()
    }

    /// No snapshot may drop an instance that a shortcut record still refers to.
    pub(crate) fn ensure_snapshot_keeps_journal_references(
        &self,
        manifest: &app_proxy_core::model::Manifest,
    ) -> Result<()> {
        self.ensure_shortcut_instances(manifest)
    }
}

/// Every journal that accepts caller-chosen request IDs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestJournal {
    Config,
    Core,
    Launch,
    Shortcut,
    Login,
}

impl RequestJournal {
    const ALL: [Self; 5] = [
        Self::Config,
        Self::Core,
        Self::Launch,
        Self::Shortcut,
        Self::Login,
    ];
}

impl Store {
    /// A request ID names one request in one journal for the life of its record.
    /// A journal calls this before it admits a new ID; replays of an ID it
    /// already holds are its own business. Adding a journal means adding a
    /// variant above and an arm below, which the compiler enforces.
    pub(crate) fn ensure_request_id_unused_elsewhere(
        &self,
        id: Uuid,
        own: RequestJournal,
    ) -> Result<()> {
        for journal in RequestJournal::ALL {
            if journal == own {
                continue;
            }
            let used = match journal {
                RequestJournal::Config => self.config_request_status(id)?.is_some(),
                RequestJournal::Core => self.core_request_status(id)?.is_some(),
                RequestJournal::Launch => self.launch_request(id)?.is_some(),
                RequestJournal::Shortcut => self.shortcut_request_status(id)?.is_some(),
                RequestJournal::Login => self.login_request_status(id)?.is_some(),
            };
            if used {
                return Err(Error::Invalid("REQUEST_ID_CONFLICT"));
            }
        }
        Ok(())
    }
}
