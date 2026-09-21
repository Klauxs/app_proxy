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
    pub(crate) fn write_install_diagnostic(
        store: Uuid,
        request: Uuid,
        error: Option<String>,
    ) -> Result<()> {
        identity::assert_elevated_user()?;
        ids(store, request)?;
        let (root, _directories) = location(&identity::current()?.user_sid, store, true)?;
        let mut file = security::new_file(&root.join(format!("install-result-{request}.json")))?;
        file.write_all(&serde_json::to_vec(&error)?)?;
        file.sync_all()?;
        Ok(())
    }

    pub(crate) fn install_diagnostic(store: Uuid, request: Uuid) -> Result<Option<String>> {
        ids(store, request)?;
        let (root, _directories) = location(&identity::current()?.user_sid, store, false)?;
        let file = security::read_file(&root.join(format!("install-result-{request}.json")))?;
        let mut bytes = Vec::new();
        file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > RECORD_LIMIT {
            return Err(Error::Invalid("GUARD_INSTALL_DIAGNOSTIC_SIZE"));
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

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
        upgrade_from: Option<Uuid>,
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
                if upgrade_from != Some(deployment.generation()) {
                    return Err(Error::Invalid("GUARD_LISTENER_RELEASE_CONFLICT"));
                }
                if deployment.record.coordinator_path != source.path {
                    return Err(Error::Invalid("GUARD_UPDATE_DIRECTORY_CHANGED"));
                }
                return upgrade_listener(
                    store,
                    source,
                    deployment,
                    &root,
                    &current.user_sid,
                    &issuer_handle,
                );
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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Upgrade {
    version: u32,
    from: Uuid,
    to: Uuid,
}

fn read_upgrade(path: &Path) -> Result<Option<Upgrade>> {
    // The reparse-point preflight uses std::fs and returns Error::Io, whereas
    // CreateFile returns Error::Windows. Both missing-file forms mean a new
    // upgrade, rather than a failed attempt to resume an existing one.
    let file = match security::read_file(path).map_err(missing) {
        Ok(file) => file,
        Err(Error::Invalid("GUARD_LISTENER_MISSING")) => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(RECORD_LIMIT + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(Error::Invalid("GUARD_UPGRADE_RECORD_SIZE"));
    }
    Ok(Some(serde_json::from_slice(&bytes)?))
}

fn upgrade_listener(
    store: Uuid,
    source: Source,
    previous: Deployment,
    root: &Path,
    sid: &str,
    issuer: &OwnedHandle,
) -> Result<Deployment> {
    let pending = root.join("listener-upgrade.json");
    let next = match read_upgrade(&pending)? {
        Some(plan) => {
            if plan.version == 1 && plan.to == previous.generation() && plan.from != plan.to {
                std::fs::remove_file(&pending)?;
                return upgrade_listener(store, source, previous, root, sid, issuer);
            }
            if plan.version != 1
                || plan.from != previous.generation()
                || plan.to.is_nil()
                || plan.to == plan.from
            {
                return Err(Error::Invalid("GUARD_UPGRADE_CONFLICT"));
            }
            let next = Deployment::open(store, plan.to)?;
            if !matches_source(&next, &source) {
                return Err(Error::Invalid("GUARD_UPGRADE_SOURCE_CHANGED"));
            }
            next
        }
        None => {
            let (base, directories) = location(sid, store, false)?;
            let next = stage_source(store, source, sid, base, directories)?;
            let plan = Upgrade {
                version: 1,
                from: previous.generation(),
                to: next.generation(),
            };
            let mut file = security::new_file(&pending)?;
            file.write_all(&serde_json::to_vec(&plan)?)?;
            file.sync_all()?;
            next
        }
    };
    issuer_alive(issuer)?;
    guard_task::retire_for_upgrade(&previous)?;
    drop(previous);
    issuer_alive(issuer)?;
    let record = ListenerRecord {
        format: FORMAT.into(),
        store,
        owner_sid: sid.into(),
        generation: next.generation(),
    };
    let stage = root.join(format!("listener-{}.tmp", Uuid::new_v4()));
    let mut file = security::new_file(&stage)?;
    file.write_all(&serde_json::to_vec(&record)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&stage, root.join(LISTENER))?;
    // After this switch, re-running setup can complete registration even if
    // the installer was interrupted. The old generation remains for diagnosis.
    guard_task::register(&next)?;
    guard_task::verify_registered(&next)?;
    std::fs::remove_file(pending)?;
    Ok(next)
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
