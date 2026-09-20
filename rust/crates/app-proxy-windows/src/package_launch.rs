//! One-use package helper requests. The request gate, not a timeout or the
//! bridge's exit code, determines whether creation can still happen.
use crate::{
    Error, Result, identity,
    installation::ResolvedApplication,
    instance_resource::AuthorizedSpawn,
    launch_state::DispatchIdentity,
    process::{self, NoProcessCreated, SpawnFailure, SpawnSpec},
    storage_security as security,
    store::{self, Store},
};
use app_proxy_core::{EnvPatch, FileIdentity, ProcessIdentity, launch::LaunchBinding};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ, FILE_SHARE_WRITE,
};

const LIMIT: usize = 1024 * 1024;
const TTL: u64 = 20;

// No Debug: this protected request contains arguments and environment values.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    context: DispatchIdentity,
    binding: LaunchBinding,
    owner_sid: String,
    directory: FileIdentity,
    helper: FileIdentity,
    issuer: ProcessIdentity,
    issued_tick: u64,
    family: String,
    full_name: String,
    issued_at: u64,
    expires_at: u64,
    args: Vec<OsString>,
    cwd: PathBuf,
    environment: EnvPatch,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
enum Phase {
    Pending {},
    Consuming {},
    Created { process: ProcessIdentity },
    NotCreated {},
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    version: u32,
    context: DispatchIdentity,
    request_digest: [u8; 32],
    phase: Phase,
}

/// Durable result only; Pending/Consuming never prove that no application exists.
pub enum PackageOutcome {
    Pending,
    Indeterminate,
    Created(ProcessIdentity),
    NotCreated(NoProcessCreated),
}

/// Carries the exact request identity, and pins the directory against renaming.
/// Dropping it neither revokes execution nor terminates a created application.
pub struct PackageTicket {
    root: PathBuf,
    request: Request,
    digest: [u8; 32],
    _directory: OwnedHandle,
    _parents: Vec<OwnedHandle>,
}

impl Store {
    /// Consumes the ordinary one-use dispatch only after global intent exists.
    /// Publication uncertainty is deliberately not converted to no-creation proof.
    pub fn prepare_package_launch(
        &self,
        permit: AuthorizedSpawn<'_>,
        application: &ResolvedApplication,
        helper: &Path,
        spec: SpawnSpec,
    ) -> std::result::Result<PackageTicket, SpawnFailure> {
        let context = permit.context();
        let root = permit
            .package_request()
            .ok_or(SpawnFailure::Indeterminate {
                error: Error::Invalid("PACKAGE_DISPATCH_REQUIRED"),
            })?
            .to_owned();
        let result = (|| {
            let package = application
                .package()
                .ok_or(Error::Invalid("MSIX_REQUIRED"))?;
            let manifest = self.load()?;
            if manifest.store_id != context.owner.store_id
                || permit.binding().executable != spec.exe
                || application.executable() != spec.exe
                || *application.image() != permit.binding().image
            {
                return Err(Error::Invalid("PACKAGE_LAUNCH_BINDING_MISMATCH"));
            }
            // The engine/activation bridge resolve the current package outside
            // the store lock; this short publication check keeps the pinned image.
            if identity::file_identity(application.executable())? != *application.image() {
                return Err(Error::Invalid("LAUNCH_EXECUTABLE_CHANGED"));
            }
            crate::creation_guard::ensure_plain_creation(&spec.exe)?;
            let helper_image = identity::file_identity(helper)?;
            crate::creation_guard::ensure_plain_creation(helper)?;
            let issued_at = now()?;
            let request = Request {
                version: 1,
                context,
                binding: permit.binding().clone(),
                owner_sid: manifest.owner_sid,
                directory: FileIdentity {
                    volume_serial: 0,
                    file_index: 0,
                },
                helper: helper_image,
                issuer: identity::current()?,
                issued_tick: tick(),
                family: package.family_name.clone(),
                full_name: package.full_name.clone(),
                issued_at,
                expires_at: issued_at + TTL,
                args: spec.args,
                cwd: spec.cwd,
                environment: spec.environment,
            };
            publish(root.clone(), request)
        })();
        drop(permit);
        result.map_err(|error| publication_failure(&root, context, error))
    }

    /// Recovery binds a protected request to the original journal, never to
    /// current editable configuration or a caller-provided request filename.
    pub fn package_launch_ticket(&self, attempt: uuid::Uuid) -> Result<Option<PackageTicket>> {
        let Some(attempt) = self.launch_request(attempt)? else {
            return Ok(None);
        };
        let Some(root) = &attempt.package_request else {
            return Ok(None);
        };
        if !root.try_exists()? {
            return Ok(None);
        }
        let ticket = PackageTicket::open(root)?;
        if ticket.request.context.owner.store_id != self.load()?.store_id
            || ticket.request.context.owner.attempt_id != attempt.id
            || ticket.request.context.owner.epoch != attempt.epoch
            || Some(ticket.request.context.dispatch_id) != attempt.dispatch_id
            || attempt.binding.as_ref() != Some(&ticket.request.binding)
        {
            return Err(Error::Invalid("PACKAGE_LAUNCH_BINDING_MISMATCH"));
        }
        Ok(Some(ticket))
    }
}

fn publication_failure(root: &Path, context: DispatchIdentity, error: Error) -> SpawnFailure {
    // All writes/rename have returned; an absent final directory cannot
    // authorize a helper. Staging names are never consumable.
    if root.try_exists().is_ok_and(|exists| !exists) {
        SpawnFailure::NotCreated {
            evidence: NoProcessCreated::from_package_receipt(context),
            error,
        }
    } else {
        SpawnFailure::Indeterminate { error }
    }
}

fn publish(root: PathBuf, request: Request) -> Result<PackageTicket> {
    publish_with(root, request, |_| Ok(()))
}

fn publish_with(
    root: PathBuf,
    mut request: Request,
    before_write: impl FnOnce(&Path) -> Result<()>,
) -> Result<PackageTicket> {
    let parents = parents(&root, &request)?;
    // Complete a sibling staging directory first. The fixed helper path check
    // rejects staging names, including a crash leaving a valid Pending inside.
    let stage = tempfile::Builder::new()
        .prefix(".package-stage-")
        .tempdir_in(root.parent().unwrap())?;
    let directory = security::directory(stage.path(), false)?;
    security::verify(directory.as_raw_handle(), &request.owner_sid, false)?;
    request.directory = directory_identity(&directory)?;
    validate(&request)?;
    before_write(stage.path())?;
    let bytes = store::encode(&request, LIMIT)?;
    let digest = Sha256::digest(&bytes).into();
    store::write_new(
        &stage.path().join("request.json"),
        &bytes,
        &request.owner_sid,
    )?;
    let ticket = PackageTicket {
        root: stage.path().to_owned(),
        request,
        digest,
        _directory: directory,
        _parents: parents,
    };
    ticket.write(Phase::Pending {})?;
    // Close our non-delete-sharing pin before the rename. No helper can consume
    // the staging path; MoveFileEx without REPLACE_EXISTING preserves old attempts.
    drop(ticket);
    let source = crate::wide(stage.path().as_os_str())?;
    let destination = crate::wide(root.as_os_str())?;
    // SAFETY: two terminated paths on the same protected volume; no replacement.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            windows_sys::Win32::Storage::FileSystem::MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        return Err(crate::last_error("PublishPackageRequest"));
    }
    PackageTicket::open(&root)
}

impl PackageTicket {
    fn open(root: &Path) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let sid = identity::current()?.user_sid;
        let directory = security::directory(root, false)?;
        security::verify(directory.as_raw_handle(), &sid, false)?;
        let bytes = store::read_protected(&root.join("request.json"), &sid, LIMIT)?;
        let request: Request = store::decode(&bytes)?;
        validate(&request)?;
        if request.owner_sid != sid || request.directory != directory_identity(&directory)? {
            return Err(Error::Invalid("PACKAGE_REQUEST_DIRECTORY_MISMATCH"));
        }
        if root.file_name()
            != Some(std::ffi::OsStr::new(&format!(
                "package-{}",
                request.context.owner.attempt_id
            )))
        {
            return Err(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"));
        }
        let parents = parents(root, &request)?;
        Ok(Self {
            root: root.to_owned(),
            request,
            digest: Sha256::digest(&bytes).into(),
            _directory: directory,
            _parents: parents,
        })
    }

    pub fn request_path(&self) -> PathBuf {
        self.root.join("request.json")
    }

    /// A busy gate means a helper may be inside creation. Do not revoke by
    /// deleting its files, changing its deadline, or trusting a vanished PID.
    pub fn revoke(&self) -> Result<PackageOutcome> {
        let _gate = self.gate()?;
        let phase = self.read()?;
        if matches!(phase, Phase::Pending {}) {
            self.write(Phase::NotCreated {})?;
            return Ok(PackageOutcome::NotCreated(
                NoProcessCreated::from_package_receipt(self.request.context),
            ));
        }
        self.outcome_for(phase)
    }

    pub fn outcome(&self) -> Result<PackageOutcome> {
        self.outcome_for(self.read()?)
    }

    fn outcome_for(&self, phase: Phase) -> Result<PackageOutcome> {
        match phase {
            Phase::Pending {} => Ok(PackageOutcome::Pending),
            Phase::Consuming {} => Ok(PackageOutcome::Indeterminate),
            Phase::NotCreated {} => Ok(PackageOutcome::NotCreated(
                NoProcessCreated::from_package_receipt(self.request.context),
            )),
            Phase::Created { process } => {
                if process.pid == 0
                    || process.creation_time == 0
                    || process.user_sid != self.request.owner_sid
                    || process.session_id != self.request.binding.session_id
                    || process.image_file != self.request.binding.image
                    || !process.image_path.is_absolute()
                {
                    return Err(Error::Invalid("PACKAGE_RECEIPT_IDENTITY_MISMATCH"));
                }
                Ok(PackageOutcome::Created(process))
            }
        }
    }

    fn gate(&self) -> Result<File> {
        security::verify(
            self._directory.as_raw_handle(),
            &self.request.owner_sid,
            false,
        )?;
        let path = self.root.join("gate");
        if path.try_exists()? {
            security::no_reparse(&path)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        security::verify(file.as_raw_handle(), &self.request.owner_sid, false)?;
        match file.try_lock() {
            Ok(()) => Ok(file),
            Err(fs::TryLockError::WouldBlock) => Err(Error::Invalid("PACKAGE_REQUEST_BUSY")),
            Err(fs::TryLockError::Error(error)) => Err(error.into()),
        }
    }

    fn read(&self) -> Result<Phase> {
        let state: State = store::decode(&store::read_protected(
            &self.root.join("state.json"),
            &self.request.owner_sid,
            LIMIT,
        )?)?;
        if state.version != 1
            || state.context != self.request.context
            || state.request_digest != self.digest
        {
            return Err(Error::Invalid("PACKAGE_RECEIPT_BINDING_MISMATCH"));
        }
        Ok(state.phase)
    }

    fn write(&self, phase: Phase) -> Result<()> {
        let state = State {
            version: 1,
            context: self.request.context,
            request_digest: self.digest,
            phase,
        };
        store::replace_protected(
            &self.root,
            &self.request.owner_sid,
            "state.json",
            &store::encode(&state, LIMIT)?,
            LIMIT,
        )
    }
}

/// Fixed package-child entry; arbitrary package-less callers cannot consume a request.
pub fn run_helper(request_path: &Path) -> Result<()> {
    let family = identity::package_family()?;
    let full_name = identity::package_full_name()?;
    consume(
        request_path,
        family.as_deref(),
        full_name.as_deref(),
        |process, request| {
            if process.package_full_name()?.as_deref() != Some(request.full_name.as_str()) {
                return Err(Error::Invalid("PACKAGE_CHILD_IDENTITY_UNCONFIRMED"));
            }
            Ok(())
        },
    )
}

fn consume(
    request_path: &Path,
    family: Option<&str>,
    full_name: Option<&str>,
    verify_child: impl FnOnce(&process::StartedProcess, &Request) -> Result<()>,
) -> Result<()> {
    let started = Instant::now();
    if !request_path.is_absolute()
        || request_path.file_name() != Some(std::ffi::OsStr::new("request.json"))
    {
        return Err(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"));
    }
    let ticket = PackageTicket::open(
        request_path
            .parent()
            .ok_or(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"))?,
    )?;
    let current = identity::current()?;
    if family != Some(ticket.request.family.as_str())
        || full_name != Some(ticket.request.full_name.as_str())
        || current.image_file != ticket.request.helper
        || current.session_id != ticket.request.binding.session_id
    {
        return Err(Error::Invalid("PACKAGE_HELPER_IDENTITY_MISMATCH"));
    }
    let _gate = ticket.gate()?;
    if !matches!(ticket.read()?, Phase::Pending {}) {
        return Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"));
    }
    let prepare = (|| {
        check_deadline(&ticket.request, started)?;
        let pinned =
            crate::installation::resolve(&app_proxy_core::model::ApplicationLocator::Exe {
                path: ticket.request.binding.executable.clone(),
            })?;
        if *pinned.image() != ticket.request.binding.image {
            return Err(Error::Invalid("LAUNCH_EXECUTABLE_CHANGED"));
        }
        crate::creation_guard::ensure_plain_creation(&ticket.request.binding.executable)?;
        ticket.request.environment.validate()?;
        check_deadline(&ticket.request, started)?;
        Ok(pinned)
    })();
    let _pinned = match prepare {
        Ok(pinned) => pinned,
        Err(error) => {
            ticket.write(Phase::NotCreated {})?;
            return Err(error);
        }
    };
    // The same exclusive gate spans final authorization, creation and receipt.
    // A crash after this write stays Consuming and cannot be retried after TTL.
    ticket.write(Phase::Consuming {})?;
    let Request {
        binding,
        args,
        cwd,
        environment,
        ..
    } = &ticket.request;
    let spec = SpawnSpec {
        exe: binding.executable.clone(),
        args: args.clone(),
        cwd: cwd.clone(),
        environment: EnvPatch {
            set: environment.set.clone(),
            unset: environment.unset.clone(),
        },
    };
    if let Err(error) = check_deadline(&ticket.request, started) {
        ticket.write(Phase::NotCreated {})?;
        return Err(error);
    }
    match process::spawn_checked(spec) {
        Ok(process) => {
            verify_child(&process, &ticket.request)?;
            ticket.write(Phase::Created {
                process: process.identity,
            })
        }
        Err((error, false)) => {
            ticket.write(Phase::NotCreated {})?;
            Err(error)
        }
        Err((error, true)) => Err(error),
    }
}

fn validate(request: &Request) -> Result<()> {
    if request.version != 1
        || request.context.owner.store_id.is_nil()
        || request.context.owner.attempt_id.is_nil()
        || request.context.owner.epoch.is_nil()
        || request.context.dispatch_id.is_nil()
        || request.issuer.pid == 0
        || request.issuer.creation_time == 0
        || request.issuer.user_sid != request.owner_sid
        || request.issuer.session_id != request.binding.session_id
        || request.issued_at == 0
        || request.issued_at.checked_add(TTL) != Some(request.expires_at)
        || request.family.is_empty()
        || request.full_name.is_empty()
        || request.family.len() > 256
        || request.full_name.len() > 256
        || request.family.contains('\0')
        || request.full_name.contains('\0')
        || !request.binding.executable.is_absolute()
        || !request.cwd.is_absolute()
    {
        return Err(Error::Invalid("INVALID_PACKAGE_REQUEST"));
    }
    request.environment.validate()?;
    Ok(())
}

fn check_deadline(request: &Request, started: Instant) -> Result<()> {
    let now = now()?;
    check_clock(request, now, tick(), started.elapsed())?;
    // A recorded live issuer binds the monotonic tick to its original boot;
    // after owner death/restart, a late helper can only record NotCreated.
    if !process::is_running_exact(&request.issuer)? {
        return Err(Error::Invalid("PACKAGE_ISSUER_EXITED"));
    }
    Ok(())
}

fn check_clock(request: &Request, now: u64, tick: u64, elapsed: Duration) -> Result<()> {
    if now < request.issued_at
        || now >= request.expires_at
        || tick
            .checked_sub(request.issued_tick)
            .is_none_or(|age| age >= TTL * 1000)
        || elapsed >= Duration::from_secs(TTL)
    {
        return Err(Error::Invalid("PACKAGE_REQUEST_EXPIRED"));
    }
    Ok(())
}

fn tick() -> u64 {
    // SAFETY: no pointers; Windows uptime includes sleep/hibernation and is not UTC.
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() }
}

fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::Invalid("SYSTEM_TIME_INVALID"))
}

fn directory_identity(handle: &OwnedHandle) -> Result<FileIdentity> {
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    // SAFETY: a retained directory handle and a correctly sized output buffer.
    unsafe {
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        if GetFileInformationByHandle(handle.as_raw_handle(), &mut info) == 0 {
            return Err(crate::last_error("PackageRequestDirectoryIdentity"));
        }
        Ok(FileIdentity {
            volume_serial: info.dwVolumeSerialNumber,
            file_index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
}

fn parents(root: &Path, request: &Request) -> Result<Vec<OwnedHandle>> {
    let state = root
        .parent()
        .ok_or(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"))?;
    let base = state
        .parent()
        .ok_or(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"))?;
    if state.file_name() != Some(std::ffi::OsStr::new("state")) {
        return Err(Error::Invalid("INVALID_PACKAGE_REQUEST_PATH"));
    }
    let handles = vec![
        security::directory(base, false)?,
        security::directory(state, false)?,
    ];
    match store::describe(base) {
        Ok(descriptor)
            if descriptor.store_id == request.context.owner.store_id
                && descriptor.owner_sid == request.owner_sid => {}
        Ok(_) => return Err(Error::Invalid("PACKAGE_LAUNCH_BINDING_MISMATCH")),
        Err(_) => crate::instance_data::verify_package_control(
            base,
            request.context.owner.store_id,
            &request.owner_sid,
            &request.family,
        )?,
    }
    for handle in &handles {
        security::verify(handle.as_raw_handle(), &request.owner_sid, false)?;
    }
    Ok(handles)
}

#[cfg(test)]
pub(crate) mod tests;
