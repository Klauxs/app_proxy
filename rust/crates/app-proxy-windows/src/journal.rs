//! Pieces shared by every durable request journal in the store.
//!
//! Each journal keeps its own record types and rules; what they have in common
//! lives here so that one journal cannot drift from the others.
use crate::{Error, Result};
use std::time::{SystemTime, UNIX_EPOCH};

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
