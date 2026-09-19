use super::*;
use crate::guard_task;

const LISTENER: &str = "listener.json";
const FORMAT: &str = "app-proxy-rust-listener-v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenerRecord {
    format: String,
    store: Uuid,
    owner_sid: String,
    generation: Uuid,
}

impl Deployment {
    /// Reads a protected installation intent; task verification and an
    /// authenticated stream are separate requirements, never inferred here.
    pub fn listener(store: Uuid) -> Result<Self> {
        let sid = identity::current()?.user_sid;
        Self::listener_inner(store, &sid)
    }

    fn listener_inner(store: Uuid, sid: &str) -> Result<Self> {
        if store.is_nil() {
            return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
        }
        let (root, directories) = location(sid, store, false).map_err(missing)?;
        let mut file = security::read_file(&root.join(LISTENER)).map_err(missing)?;
        let record = read_record(&mut file, store, sid)?;
        let mut deployment = Self::open_at(&root, directories, sid, store, record.generation)?;
        deployment._listener_record = Some(file);
        Ok(deployment)
    }

    /// Fixed listener installation only. Keeps one durable generation intent
    /// before registration, so an unknown COM result can be verified/reused on
    /// the next explicit foreground authorization without replacing the action.
    pub(crate) fn install_listener(
        store: Uuid,
        issuer: &ProcessIdentity,
        expected: &SourceExpectation,
    ) -> Result<Self> {
        identity::assert_elevated_user()?;
        let current = identity::current()?;
        if store.is_nil() {
            return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
        }
        let issuer_handle = verify_issuer(&current, issuer)?;
        if !current
            .image_path
            .file_name()
            .is_some_and(|name| name.eq_ignore_ascii_case(HOST))
        {
            return Err(Error::Invalid("GUARD_HOST_EXECUTABLE_REQUIRED"));
        }
        let source = Source::expected(&current.image_path, expected)?;
        issuer_alive(&issuer_handle)?;
        let (root, directories) = location(&current.user_sid, store, true)?;
        // Same strict ACL/share policy as the event journal, but a distinct lock.
        let _lock = security::event_journal(&root.join("listener-install.lock"))?;
        let existing = match security::read_file(&root.join(LISTENER)) {
            Ok(mut file) => Some(read_record(&mut file, store, &current.user_sid)?),
            Err(error) => match missing(error) {
                Error::Invalid("GUARD_LISTENER_MISSING") => None,
                other => return Err(other),
            },
        };
        let deployment = if let Some(record) = existing {
            let deployment = Self::open_at(
                &root,
                directories,
                &current.user_sid,
                store,
                record.generation,
            )?;
            if !matches_source(&deployment, &source) {
                return Err(Error::Invalid("GUARD_LISTENER_RELEASE_CONFLICT"));
            }
            deployment
        } else {
            let deployment =
                stage_source(store, source, &current.user_sid, root.clone(), directories)?;
            issuer_alive(&issuer_handle)?;
            let record = ListenerRecord {
                format: FORMAT.into(),
                store,
                owner_sid: current.user_sid.clone(),
                generation: deployment.generation(),
            };
            let mut file = security::new_file(&root.join(LISTENER))?;
            file.write_all(&serde_json::to_vec(&record)?)?;
            file.sync_all()?;
            drop(file);
            // Re-read the immutable intent before any task side effect.
            let observed = Self::listener_inner(store, &current.user_sid)?;
            if observed.generation() != deployment.generation() {
                return Err(Error::Invalid("GUARD_LISTENER_INTENT_CHANGED"));
            }
            observed
        };
        issuer_alive(&issuer_handle)?;
        guard_task::register(&deployment)?;
        guard_task::verify_registered(&deployment)?;
        issuer_alive(&issuer_handle)?;
        Ok(deployment)
    }
}

fn missing(error: Error) -> Error {
    match error {
        Error::Windows { code: 2 | 3, .. } => Error::Invalid("GUARD_LISTENER_MISSING"),
        Error::Io(ref io) if matches!(io.raw_os_error(), Some(2 | 3)) => {
            Error::Invalid("GUARD_LISTENER_MISSING")
        }
        other => other,
    }
}

impl InstallerSource {
    pub(crate) fn matches_listener(&self, deployment: &Deployment) -> bool {
        matches_source(deployment, &self.source)
    }
}
fn matches_source(deployment: &Deployment, source: &Source) -> bool {
    let record = &deployment.record;
    record.coordinator_path == source.path
        && record.coordinator_image == source.image
        && record.image_size == source.size
        && record.image_sha256 == source.hash
}

fn read_record(file: &mut File, store: Uuid, sid: &str) -> Result<ListenerRecord> {
    if file.metadata()?.len() > RECORD_LIMIT {
        return Err(Error::Invalid("GUARD_LISTENER_RECORD_SIZE"));
    }
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(Error::Invalid("GUARD_LISTENER_RECORD_SIZE"));
    }
    let record: ListenerRecord = serde_json::from_slice(&bytes)
        .map_err(|_| Error::Invalid("INVALID_GUARD_LISTENER_RECORD"))?;
    if record.format != FORMAT
        || record.store != store
        || record.owner_sid != sid
        || record.generation.is_nil()
    {
        return Err(Error::Invalid("GUARD_LISTENER_RECORD_MISMATCH"));
    }
    Ok(record)
}

#[cfg(test)]
mod tests;
