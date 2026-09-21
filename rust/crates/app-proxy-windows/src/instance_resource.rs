//! Per-user cross-store reservations. A kernel lock serializes owners; the
//! protected claim survives its owner's death and never expires by elapsed time.
//! This coordinates this product only; external process discovery is separate.
use crate::{
    Error, Result, identity,
    installation::ResolvedApplication,
    instance_data::{self, PreparedData},
    process, storage_security as security, store,
};
use app_proxy_core::{FileIdentity, ProcessIdentity, launch::LaunchPhase};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    sync::Arc,
};
use uuid::Uuid;
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE, MOVEFILE_WRITE_THROUGH,
    MoveFileExW,
};

const MARKER: &str = ".app-proxy-rust-resources.json";
const FORMAT: &str = "app-proxy-rust-resources";
const LIMIT: usize = 16384;

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum InstallationKey {
    Exe { image: FileIdentity },
    Msix { family_name: String, app_id: String },
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum DataKey {
    Original {},
    Isolated { directory: FileIdentity },
}
#[derive(Serialize)]
struct Key {
    version: u32,
    user_sid: String,
    session_id: u32,
    installation: InstallationKey,
    data: DataKey,
}

/// Constructed from resolved, pinned installation/data identities, not editable
/// labels or a caller-supplied hash. Keep both inputs alive through launch.
pub struct InstanceResource {
    key: [u8; 32],
    user_sid: String,
    session_id: u32,
    executable: PathBuf,
    image: FileIdentity,
    installation_image: Option<FileIdentity>,
}
impl InstanceResource {
    pub fn resolve(application: &ResolvedApplication, data: Option<&PreparedData>) -> Result<Self> {
        identity::assert_ordinary_user()?;
        application.verify_current()?;
        let caller = identity::current()?;
        let key = Key {
            version: 1,
            user_sid: caller.user_sid.clone(),
            session_id: caller.session_id,
            installation: match application.package() {
                Some(package) => InstallationKey::Msix {
                    family_name: package.family_name.to_ascii_lowercase(),
                    app_id: package.app_id.to_ascii_lowercase(),
                },
                None => InstallationKey::Exe {
                    image: application.image().clone(),
                },
            },
            data: match data {
                Some(data) => DataKey::Isolated {
                    directory: data.physical_identity()?,
                },
                None => DataKey::Original {},
            },
        };
        Ok(Self {
            key: Sha256::digest(store::encode(&key, LIMIT)?).into(),
            user_sid: caller.user_sid,
            session_id: caller.session_id,
            executable: application.executable().to_owned(),
            image: application.image().clone(),
            installation_image: application
                .package()
                .is_none()
                .then(|| application.image().clone()),
        })
    }
    pub fn digest(&self) -> [u8; 32] {
        self.key
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceOwner {
    pub store_id: Uuid,
    pub attempt_id: Uuid,
    pub epoch: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResourcePhase {
    Reserved {},
    SpawnRequested {},
    Confirmed { process: ProcessIdentity },
    Released {},
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceClaim {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    guard_stops: Vec<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    guard_dispatch_id: Option<Uuid>,
    schema_version: u32,
    resource_key: [u8; 32],
    user_sid: String,
    session_id: u32,
    executable: PathBuf,
    image: FileIdentity,
    pub owner: ResourceOwner,
    dispatch_id: Option<Uuid>,
    #[serde(default)]
    local_confirmed: bool,
    pub phase: ResourcePhase,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Marker {
    format: String,
    schema_version: u32,
    owner_sid: String,
}

#[derive(Clone)]
pub struct ResourceRegistry {
    root: PathBuf,
    sid: String,
    _directory: Arc<OwnedHandle>,
}
impl ResourceRegistry {
    /// Isolated fixture registries; normal builds always use the user-wide root.
    #[cfg(feature = "test-support")]
    pub fn for_test_at(root: &Path) -> Result<Self> {
        Self::open_at(root)
    }

    pub fn open() -> Result<Self> {
        Self::open_at(&instance_data::local_app_data()?.join("AppProxyRustResources"))
    }

    fn open_at(root: &Path) -> Result<Self> {
        identity::assert_ordinary_user()?;
        if !root.is_absolute()
            || root
                .components()
                .any(|p| matches!(p, std::path::Component::ParentDir))
        {
            return Err(Error::Invalid("ABSOLUTE_RESOURCE_ROOT_REQUIRED"));
        }
        let sid = identity::current()?.user_sid;
        if !root.try_exists()? {
            let parent = root
                .parent()
                .ok_or(Error::Invalid("RESOURCE_PARENT_REQUIRED"))?;
            security::no_reparse(parent)?;
            // Publish a complete protected directory, so concurrent initializers
            // never see a half-written ownership marker at the final name.
            let staging = tempfile::Builder::new()
                .prefix(".app-proxy-resources-")
                .tempdir_in(parent)?;
            let pin = security::directory(staging.path(), true)?;
            security::protect(&pin, &sid)?;
            let marker = Marker {
                format: FORMAT.into(),
                schema_version: 1,
                owner_sid: sid.clone(),
            };
            store::write_new(
                &staging.path().join(MARKER),
                &store::encode(&marker, LIMIT)?,
                &sid,
            )?;
            drop(pin);
            let source = crate::wide(staging.path().as_os_str())?;
            let destination = crate::wide(root.as_os_str())?;
            // SAFETY: live terminated paths; no replace flag. Never overwrite a
            // directory another initializer or the user created at this name.
            if unsafe {
                MoveFileExW(
                    source.as_ptr(),
                    destination.as_ptr(),
                    MOVEFILE_WRITE_THROUGH,
                )
            } == 0
            {
                let error = crate::last_error("PublishResourceRegistry");
                if !root.try_exists()? {
                    return Err(error);
                }
            }
        }
        let directory = security::directory(root, false)?;
        security::verify(directory.as_raw_handle(), &sid, true)?;
        let marker: Marker =
            store::decode(&store::read_protected(&root.join(MARKER), &sid, LIMIT)?)?;
        if marker.format != FORMAT || marker.schema_version != 1 || marker.owner_sid != sid {
            return Err(Error::Invalid("RESOURCE_REGISTRY_OWNER_MISMATCH"));
        }
        Ok(Self {
            root: root.to_owned(),
            sid,
            _directory: Arc::new(directory),
        })
    }

    pub fn acquire(&self, resource: InstanceResource) -> Result<ResourceReservation> {
        self.acquire_checked(resource, false)
    }

    fn acquire_checked(
        &self,
        resource: InstanceResource,
        historical: bool,
    ) -> Result<ResourceReservation> {
        let caller = identity::current()?;
        if self.sid != resource.user_sid
            || self.sid != caller.user_sid
            || (!historical && resource.session_id != caller.session_id)
        {
            return Err(Error::IdentityMismatch);
        }
        security::verify(self._directory.as_raw_handle(), &self.sid, true)?;
        let name = resource
            .key
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        let path = self.root.join(format!("{name}.lock"));
        if path.try_exists()? {
            security::no_reparse(&path)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&path)?;
        security::verify(lock.as_raw_handle(), &self.sid, false)?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => {
                return Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.into()),
        }
        let mut reservation = ResourceReservation {
            registry: self.clone(),
            resource,
            name: format!("{name}.json"),
            _lock: lock,
            claim: None,
        };
        reservation.claim = reservation.read_current()?;
        Ok(reservation)
    }

    /// Recover only protected historical evidence. Never resolve current config
    /// into a replacement identity or issue a new dispatch during recovery.
    pub fn reconcile_launch(&self, store: &mut store::Store, id: Uuid) -> Result<()> {
        let attempt = store
            .launch_request(id)?
            .ok_or(Error::Invalid("LAUNCH_ATTEMPT_NOT_FOUND"))?;
        let Some(binding) = attempt.binding else {
            return Ok(());
        };
        let header = store.load()?;
        let resource = InstanceResource {
            key: binding.resource_key,
            user_sid: header.owner_sid,
            session_id: binding.session_id,
            executable: binding.executable,
            image: binding.image,
            installation_image: None,
        };
        // Kept private to this protected-store recovery path. No caller receives
        // a cross-session reservation from which it could request a new spawn.
        let mut reservation = self.acquire_checked(resource, true)?;
        let expected = ResourceOwner {
            store_id: header.store_id,
            attempt_id: attempt.id,
            epoch: attempt.epoch,
        };
        if reservation.claim().is_some_and(|c| c.owner != expected) {
            // An acknowledged/released claim may already have a new owner.
            if matches!(attempt.phase, LaunchPhase::Confirmed { .. })
                || matches!(&attempt.phase, LaunchPhase::Failed { code } if code == "APPLICATION_NOT_CREATED")
            {
                store.finish_resource_sync(attempt.id)?;
            }
            return Ok(());
        }
        reservation.reconcile(store)
    }
}

/// Moveable across async executor threads. Drop releases only the kernel lock;
/// it does not erase a claim, release an unknown attempt or stop an application.
pub struct ResourceReservation {
    registry: ResourceRegistry,
    resource: InstanceResource,
    name: String,
    _lock: File,
    claim: Option<ResourceClaim>,
}

/// Keeps the physical reservation borrowed and the store owner lease alive until
/// creation returns. Only a fresh local dispatch and global intent can issue it.
pub struct AuthorizedSpawn<'a> {
    _reservation: &'a mut ResourceReservation,
    dispatch: crate::launch_state::LaunchDispatch,
}

pub struct AuthorizedGuardStop<'a> {
    pub(crate) dispatch: crate::launch_state::GuardStopDispatch,
    _reservation: &'a mut ResourceReservation,
}
impl AuthorizedSpawn<'_> {
    pub(crate) fn package_request(&self) -> Option<&Path> {
        self.dispatch.package_request()
    }
    pub(crate) fn context(&self) -> crate::launch_state::DispatchIdentity {
        self.dispatch.context()
    }
    pub(crate) fn binding(&self) -> &app_proxy_core::launch::LaunchBinding {
        self.dispatch.binding()
    }
}
impl ResourceReservation {
    pub fn claim(&self) -> Option<&ResourceClaim> {
        self.claim.as_ref()
    }

    pub fn reserve(&mut self, owner: ResourceOwner) -> Result<()> {
        if self
            .claim
            .as_ref()
            .is_some_and(|c| c.phase != (ResourcePhase::Released {}))
        {
            return Err(Error::Invalid("INSTANCE_RESOURCE_RECOVERY_REQUIRED"));
        }
        let claim = ResourceClaim {
            guard_stops: self
                .claim
                .as_ref()
                .map(|c| c.guard_stops.clone())
                .unwrap_or_default(),
            guard_dispatch_id: None,
            schema_version: 1,
            resource_key: self.resource.key,
            user_sid: self.resource.user_sid.clone(),
            session_id: self.resource.session_id,
            executable: self.resource.executable.clone(),
            image: self.resource.image.clone(),
            owner,
            dispatch_id: None,
            local_confirmed: false,
            phase: ResourcePhase::Reserved {},
        };
        self.write(claim)
    }

    /// Consume the local one-use intent and persist the physical instance's
    /// cooldown across store/owner changes before any native close request.
    pub fn authorize_guard_stop(
        &mut self,
        dispatch: crate::launch_state::GuardStopDispatch,
    ) -> Result<AuthorizedGuardStop<'_>> {
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| Error::Invalid("CLOCK_BEFORE_EPOCH"))?
            .as_secs();
        self.publish_guard_stop(&dispatch, at)?;
        Ok(AuthorizedGuardStop {
            dispatch,
            _reservation: self,
        })
    }

    fn publish_guard_stop(
        &mut self,
        dispatch: &crate::launch_state::GuardStopDispatch,
        at: u64,
    ) -> Result<()> {
        self.publish_guard_stop_record(dispatch.owner, &dispatch.target.process, dispatch.nonce, at)
    }

    /// Restart throttling and resource bookkeeping happen after an event stop.
    /// Failure prevents restart; it cannot undo or repeat the confirmed stop.
    pub fn record_observed_guard_stop(
        &mut self,
        owner: ResourceOwner,
        receipt: &crate::process_stop::ObservedGuardStop,
    ) -> Result<()> {
        self.publish_guard_stop_record(
            owner,
            &receipt.target.process,
            receipt.nonce,
            receipt.started_at,
        )
    }

    fn publish_guard_stop_record(
        &mut self,
        owner: ResourceOwner,
        target: &app_proxy_core::ProcessIdentity,
        nonce: Uuid,
        at: u64,
    ) -> Result<()> {
        let mut claim = self.require_owner(owner)?.clone();
        if claim.phase != (ResourcePhase::Reserved {})
            || claim.guard_dispatch_id.is_some()
            || target.image_file != claim.image
            || target.user_sid != claim.user_sid
            || target.session_id != claim.session_id
        {
            return Err(Error::Invalid("GUARD_RESOURCE_MISMATCH"));
        }
        if claim.guard_stops.last().is_some_and(|last| *last > at) {
            return Err(Error::Invalid("GUARD_CLOCK_ROLLBACK"));
        }
        claim
            .guard_stops
            .retain(|last| last.saturating_add(60) > at);
        if claim
            .guard_stops
            .last()
            .is_some_and(|last| last.saturating_add(5) > at)
        {
            return Err(Error::Invalid("GUARD_COOLDOWN"));
        }
        if claim.guard_stops.len() >= 3 {
            return Err(Error::Invalid("GUARD_RATE_LIMIT"));
        }
        claim.guard_stops.push(at);
        claim.guard_dispatch_id = Some(nonce);
        self.write(claim)
    }

    /// Persist after the local attempt's SpawnRequested and before invoking any
    /// process/helper creation. Both intents must succeed before dispatch.
    pub fn authorize_spawn(
        &mut self,
        dispatch: crate::launch_state::LaunchDispatch,
    ) -> std::result::Result<AuthorizedSpawn<'_>, process::SpawnFailure> {
        if let Err(error) = self.publish_spawn_intent(&dispatch) {
            return Err(process::not_dispatched(dispatch, error));
        }
        Ok(AuthorizedSpawn {
            _reservation: self,
            dispatch,
        })
    }

    fn publish_spawn_intent(
        &mut self,
        dispatch: &crate::launch_state::LaunchDispatch,
    ) -> Result<()> {
        let context = dispatch.context();
        let mut claim = self.require_owner(context.owner)?.clone();
        if claim.phase != (ResourcePhase::Reserved {}) {
            return Err(Error::Invalid("INVALID_RESOURCE_TRANSITION"));
        }
        let binding = dispatch.binding();
        if binding.resource_key != self.resource.key
            || binding.image != claim.image
            || binding.executable != claim.executable
            || binding.session_id != claim.session_id
        {
            return Err(Error::Invalid("RESOURCE_LAUNCH_BINDING_MISMATCH"));
        }
        claim.dispatch_id = Some(context.dispatch_id);
        claim.phase = ResourcePhase::SpawnRequested {};
        self.write(claim)
    }

    /// Exact newly-created handle/helper receipt evidence only. Write this before
    /// the local Confirmed receipt so owner recovery can recover a lost local ACK.
    pub fn confirm(&mut self, owner: ResourceOwner, process: ProcessIdentity) -> Result<()> {
        let mut claim = self.require_owner(owner)?.clone();
        if claim.phase != (ResourcePhase::SpawnRequested {}) {
            return Err(Error::Invalid("INVALID_RESOURCE_TRANSITION"));
        }
        claim.phase = ResourcePhase::Confirmed { process };
        self.write(claim)
    }

    pub fn release_before_spawn(&mut self, owner: ResourceOwner) -> Result<()> {
        let mut claim = self.require_owner(owner)?.clone();
        if claim.phase != (ResourcePhase::Reserved {}) {
            return Err(Error::Invalid("RESOURCE_SPAWN_RESULT_UNKNOWN"));
        }
        claim.phase = ResourcePhase::Released {};
        self.write(claim)
    }

    pub fn release_not_created(
        &mut self,
        owner: ResourceOwner,
        evidence: &process::NoProcessCreated,
    ) -> Result<()> {
        let mut claim = self.require_owner(owner)?.clone();
        if claim.phase != (ResourcePhase::SpawnRequested {})
            || evidence.context().owner != owner
            || claim.dispatch_id != Some(evidence.context().dispatch_id)
        {
            return Err(Error::Invalid("LAUNCH_NO_CREATION_EVIDENCE_MISMATCH"));
        }
        claim.phase = ResourcePhase::Released {};
        self.write(claim)
    }

    pub fn release_exited(&mut self, owner: ResourceOwner) -> Result<()> {
        let mut claim = self.require_owner(owner)?.clone();
        let ResourcePhase::Confirmed { ref process } = claim.phase else {
            return Err(Error::Invalid("RESOURCE_SPAWN_RESULT_UNKNOWN"));
        };
        if !claim.local_confirmed {
            return Err(Error::Invalid("INSTANCE_RESOURCE_RECOVERY_REQUIRED"));
        }
        if process::is_running_exact(process)? {
            return Err(Error::Invalid("INSTANCE_STILL_RUNNING"));
        }
        claim.phase = ResourcePhase::Released {};
        self.write(claim)
    }

    /// Complete the two-journal handoff while retaining this resource's lock.
    /// A matching durable no-creation result is as strong as its original token;
    /// a mere timeout, PID observation or unrelated terminal result is not.
    pub fn reconcile(&mut self, store: &mut store::Store) -> Result<()> {
        let Some(mut claim) = self.claim.clone() else {
            return Ok(());
        };
        if store.load()?.store_id != claim.owner.store_id {
            return Ok(());
        }
        let Some(attempt) = store.launch_request(claim.owner.attempt_id)? else {
            return Ok(());
        };
        if attempt.epoch == claim.owner.epoch
            && attempt.dispatch_id.is_none()
            && matches!(attempt.phase, LaunchPhase::Failed { .. })
            && matches!(
                claim.phase,
                ResourcePhase::Reserved {} | ResourcePhase::Released {}
            )
        {
            if matches!(claim.phase, ResourcePhase::Reserved {}) {
                self.release_before_spawn(claim.owner)?;
            }
            return store.finish_resource_sync(attempt.id);
        }
        let Some(binding) = &attempt.binding else {
            return Ok(());
        };
        if attempt.epoch != claim.owner.epoch
            || binding.resource_key != claim.resource_key
            || binding.image != claim.image
            || binding.executable != claim.executable
            || binding.session_id != claim.session_id
        {
            return Err(Error::Invalid("RESOURCE_LAUNCH_BINDING_MISMATCH"));
        }
        if !matches!(claim.phase, ResourcePhase::Reserved {})
            && attempt.dispatch_id != claim.dispatch_id
        {
            return Err(Error::Invalid("RESOURCE_LAUNCH_BINDING_MISMATCH"));
        }
        if matches!(claim.phase, ResourcePhase::SpawnRequested {})
            && matches!(
                attempt.phase,
                LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {}
            )
            && let Some(ticket) = store.package_launch_ticket(attempt.id)?
        {
            // The caller holds the physical resource lock: no original worker
            // can still publish/activate a helper. Revocation shares the helper's
            // gate, and never converts Consuming or a missing receipt to failure.
            match ticket.revoke()? {
                crate::package_launch::PackageOutcome::Created(process) => {
                    self.confirm(claim.owner, process)?;
                    return self.reconcile(store);
                }
                crate::package_launch::PackageOutcome::NotCreated(evidence) => {
                    store.fail_launch_not_created(attempt.id, attempt.epoch, &evidence)?;
                    return self.reconcile(store);
                }
                _ => {}
            }
        }
        match (&claim.phase, &attempt.phase) {
            (ResourcePhase::Confirmed { process }, phase) => {
                match phase {
                    LaunchPhase::Confirmed { process: local } if local == process => {}
                    LaunchPhase::SpawnRequested {}
                    | LaunchPhase::AwaitingIdentity {}
                    | LaunchPhase::Indeterminate {} => {
                        store.confirm_launch(attempt.id, attempt.epoch, process.clone())?;
                    }
                    _ => return Err(Error::Invalid("RESOURCE_LAUNCH_BINDING_MISMATCH")),
                }
                if !claim.local_confirmed {
                    claim.local_confirmed = true;
                    self.write(claim)?;
                }
                store.finish_resource_sync(attempt.id)?;
            }
            (
                ResourcePhase::Reserved {} | ResourcePhase::SpawnRequested {},
                LaunchPhase::Failed { code },
            ) if attempt.dispatch_id.is_some() && code == "APPLICATION_NOT_CREATED" => {
                claim.phase = ResourcePhase::Released {};
                // Reserved means publishing global intent failed before dispatch.
                claim.dispatch_id = attempt.dispatch_id;
                self.write(claim)?;
                store.finish_resource_sync(attempt.id)?;
            }
            (
                ResourcePhase::Released {},
                LaunchPhase::Failed { .. } | LaunchPhase::Confirmed { .. },
            ) => {
                store.finish_resource_sync(attempt.id)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn require_owner(&self, owner: ResourceOwner) -> Result<&ResourceClaim> {
        self.claim
            .as_ref()
            .filter(|c| c.owner == owner)
            .ok_or(Error::Invalid("INSTANCE_RESOURCE_OWNER_MISMATCH"))
    }
    fn write(&mut self, claim: ResourceClaim) -> Result<()> {
        self.validate(&claim)?;
        security::verify(
            self.registry._directory.as_raw_handle(),
            &self.registry.sid,
            true,
        )?;
        if store::encode(&self.read_current()?, LIMIT)? != store::encode(&self.claim, LIMIT)? {
            return Err(Error::Invalid("INSTANCE_RESOURCE_CLAIM_CHANGED"));
        }
        store::replace_protected(
            &self.registry.root,
            &self.registry.sid,
            &self.name,
            &store::encode(&claim, LIMIT)?,
            LIMIT,
        )?;
        self.claim = Some(claim);
        Ok(())
    }
    fn read_current(&self) -> Result<Option<ResourceClaim>> {
        let path = self.registry.root.join(&self.name);
        if !path.try_exists()? {
            return Ok(None);
        }
        let claim = store::decode(&store::read_protected(&path, &self.registry.sid, LIMIT)?)?;
        self.validate(&claim)?;
        Ok(Some(claim))
    }
    fn validate(&self, claim: &ResourceClaim) -> Result<()> {
        if claim.schema_version != 1
            || claim.guard_stops.len() > 3
            || claim.guard_stops.contains(&0)
            || claim.guard_stops.windows(2).any(|pair| pair[0] >= pair[1])
            || claim.guard_dispatch_id.is_some_and(|id| id.is_nil())
            || (claim.guard_dispatch_id.is_some() && claim.guard_stops.is_empty())
            || claim.resource_key != self.resource.key
            || claim.user_sid != self.registry.sid
            || claim.session_id != self.resource.session_id
            || claim.owner.store_id.is_nil()
            || claim.owner.attempt_id.is_nil()
            || claim.owner.epoch.is_nil()
            || self
                .resource
                .installation_image
                .as_ref()
                .is_some_and(|image| *image != claim.image)
            || !claim.executable.is_absolute()
            || claim.executable.to_str().is_none_or(|p| p.contains('\0'))
        {
            return Err(Error::Invalid("INVALID_RESOURCE_CLAIM"));
        }
        if claim.dispatch_id.is_some_and(|id| id.is_nil())
            || (claim.local_confirmed
                && !matches!(
                    claim.phase,
                    ResourcePhase::Confirmed { .. } | ResourcePhase::Released {}
                ))
            || (matches!(claim.phase, ResourcePhase::Reserved {}) && claim.dispatch_id.is_some())
            || (matches!(
                claim.phase,
                ResourcePhase::SpawnRequested {} | ResourcePhase::Confirmed { .. }
            ) && claim.dispatch_id.is_none())
        {
            return Err(Error::Invalid("INVALID_RESOURCE_CLAIM"));
        }
        if let ResourcePhase::Confirmed { process } = &claim.phase
            && (process.pid == 0
                || process.creation_time == 0
                || process.user_sid != claim.user_sid
                || process.session_id != claim.session_id
                || process.image_file != claim.image
                || !process.image_path.is_absolute()
                || process.image_path.to_str().is_none_or(|p| p.contains('\0')))
        {
            return Err(Error::Invalid("INVALID_RESOURCE_PROCESS"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
