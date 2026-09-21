//! Pieces shared by every durable request journal in the store.
//!
//! Each journal keeps its own record types and rules; what they have in common
//! lives here so that one journal cannot drift from the others.
use crate::{Error, Result, store::Store};
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
