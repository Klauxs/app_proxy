use super::*;
use crate::etw::{self, ProcessListener};

const FORMAT: &str = "app-proxy-rust-event-owner-v1";
const LIMIT: u64 = 4096;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventOwner {
    format: String,
    store: Uuid,
    sid: String,
    session: u32,
    epoch: Uuid,
}

/// Field order matters: trace stops before the exclusive journal handle closes.
/// The deployment borrow also keeps all protected parent directories pinned.
pub(crate) struct EventTrace<'a> {
    pub(crate) listener: ProcessListener,
    _journal: File,
    _deployment: &'a Deployment,
}
impl<'a> EventTrace<'a> {
    pub(super) fn start(deployment: &'a Deployment, current: &ProcessIdentity) -> Result<Self> {
        // Store-wide across helper generations, separately scoped to each session.
        let root = deployment
            .host_path()
            .parent()
            .and_then(Path::parent)
            .ok_or(Error::Invalid("GUARD_PARENT_REQUIRED"))?;
        let mut journal =
            security::event_journal(&root.join(format!("events-{}.json", current.session_id)))?;
        let epoch = prepare(
            &mut journal,
            deployment.store_id(),
            current,
            etw::recover_owned,
        )?;
        // The durable epoch precedes StartTrace, including the crash window
        // before StartTrace returns a handle. Failure leaves recoverable evidence.
        let listener = ProcessListener::start(deployment.store_id(), epoch)?;
        Ok(Self {
            listener,
            _journal: journal,
            _deployment: deployment,
        })
    }
}

fn prepare(
    journal: &mut File,
    store: Uuid,
    current: &ProcessIdentity,
    recover: impl FnOnce(Uuid, Uuid) -> Result<()>,
) -> Result<Uuid> {
    if store.is_nil() || current.session_id == 0 {
        return Err(Error::Invalid("INVALID_EVENT_OWNER_SCOPE"));
    }
    if journal.metadata()?.len() > LIMIT {
        return Err(Error::Invalid("EVENT_OWNER_SIZE"));
    }
    journal.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(journal)
        .take(LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LIMIT {
        return Err(Error::Invalid("EVENT_OWNER_SIZE"));
    }
    if !bytes.is_empty() {
        let old: EventOwner =
            serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("INVALID_EVENT_OWNER"))?;
        if old.format != FORMAT
            || old.store != store
            || old.sid != current.user_sid
            || old.session != current.session_id
            || old.epoch.is_nil()
        {
            return Err(Error::Invalid("EVENT_OWNER_MISMATCH"));
        }
        recover(store, old.epoch)?;
    }
    let epoch = Uuid::new_v4();
    let next = EventOwner {
        format: FORMAT.into(),
        store,
        sid: current.user_sid.clone(),
        session: current.session_id,
        epoch,
    };
    // The old trace is now absent. A partial write blocks future starts, without
    // losing ownership of a live trace. No automatic adoption of malformed data.
    let bytes = serde_json::to_vec(&next)?;
    journal.set_len(0)?;
    journal.seek(SeekFrom::Start(0))?;
    journal.write_all(&bytes)?;
    journal.sync_all()?;
    Ok(epoch)
}

#[cfg(test)]
mod tests;
