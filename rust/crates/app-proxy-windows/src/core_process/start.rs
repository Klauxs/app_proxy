//! Durable launch witness. Atomic job assignment closes the spawn -> identity
//! write gap without discovering or adopting a process by name/port.
use super::*;
use crate::{
    core_state::CoreState,
    store::{self, Store},
};
use app_proxy_core::FileIdentity;
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    os::windows::io::FromRawHandle,
    path::{Path, PathBuf},
    ptr,
};
use uuid::Uuid;
use windows_sys::Win32::{
    Security::{
        Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW, SECURITY_ATTRIBUTES,
    },
    System::{JobObjects::*, SystemServices::JOB_OBJECT_QUERY},
};

const PATH: &str = "state/core/start.json";
// Includes the full normalized Start action (up to 1024 profiles) and paths.
const LIMIT: usize = 1024 * 1024;
#[cfg(test)]
mod tests;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Witness {
    version: u32,
    store_id: Uuid,
    generation: Uuid,
    nonce: Uuid,
    creator: ProcessIdentity,
    executable: PathBuf,
    image: FileIdentity,
    request: Option<(Uuid, app_proxy_core::core_control::CoreAction)>,
}
impl Witness {
    fn name(&self) -> String {
        format!("Local\\AppProxyRust.Core.{}.{}", self.store_id, self.nonce)
    }
}

pub struct PreparedCore {
    job: OwnedHandle,
    witness: Witness,
}
impl CoreProcess {
    /// Caller holds the lifecycle gate, writes Starting after this succeeds,
    /// then consumes this one-use value without an await before recording PID.
    pub fn prepare(
        store: &mut Store,
        binary: &CoreBinary,
        generation: &CoreGeneration,
    ) -> Result<PreparedCore> {
        identity::assert_ordinary_user()?;
        binary.verify_current()?;
        crate::creation_guard::ensure_plain_creation(binary.executable())?;
        prepare(store, binary.executable(), generation)
    }
}
fn prepare(
    store: &mut Store,
    executable: &Path,
    generation: &CoreGeneration,
) -> Result<PreparedCore> {
    if matches!(store.core_state()?, CoreState::Starting { .. }) {
        return Err(Error::Invalid("CORE_START_RESULT_UNKNOWN"));
    }
    let creator = identity::current()?;
    let witness = Witness {
        version: 1,
        store_id: store.load()?.store_id,
        generation: generation.id(),
        nonce: Uuid::new_v4(),
        image: identity::file_identity(executable)?,
        executable: executable.into(),
        creator,
        request: None,
    };
    let job = create_job(&witness)?;
    store.replace_bounded(PATH, &store::encode(&witness, LIMIT)?, LIMIT)?;
    Ok(PreparedCore { job, witness })
}

impl PreparedCore {
    pub fn bind_request(
        mut self,
        store: &mut Store,
        id: Uuid,
        mut action: app_proxy_core::core_control::CoreAction,
    ) -> Result<Self> {
        action.normalize().map_err(|e| Error::Invalid(e.0))?;
        if id.is_nil()
            || !matches!(
                action,
                app_proxy_core::core_control::CoreAction::Start { .. }
            )
        {
            return Err(Error::Invalid("INVALID_CORE_REQUEST"));
        }
        self.witness.request = Some((id, action));
        store.replace_bounded(PATH, &store::encode(&self.witness, LIMIT)?, LIMIT)?;
        Ok(self)
    }

    pub fn spawn(self, binary: &CoreBinary, generation: &CoreGeneration) -> Result<CoreProcess> {
        binary.verify_current()?;
        if generation.id() != self.witness.generation
            || binary.executable() != self.witness.executable
        {
            return Err(Error::Invalid("CORE_START_BINDING_MISMATCH"));
        }
        self.spawn_words(
            &[
                OsStr::new("run"),
                OsStr::new("-c"),
                generation.config_path().as_os_str(),
            ],
            generation.config_path().parent().unwrap(),
        )
    }
    fn spawn_words(self, args: &[&OsStr], cwd: &Path) -> Result<CoreProcess> {
        crate::creation_guard::ensure_plain_creation(&self.witness.executable)?;
        if identity::file_identity(&self.witness.executable)? != self.witness.image {
            return Err(Error::Invalid("CORE_START_IMAGE_CHANGED"));
        }
        let application = crate::wide(self.witness.executable.as_os_str())?;
        let cwd = crate::wide(cwd.as_os_str())?;
        let mut command = crate::process::quote_windows_word(self.witness.executable.as_os_str())?;
        for arg in args {
            command.push(b' ' as u16);
            command.extend(crate::process::quote_windows_word(arg)?);
        }
        command.push(0);
        if command.len() > 32767 {
            return Err(Error::Invalid("CORE_COMMAND_TOO_LONG"));
        }
        // SAFETY: aligned attribute storage and all values outlive the native
        // call. Only the query-only job lease is inherited. Kill-on-close makes
        // a missing lease evidence of absence, rather than an orphaned process.
        unsafe {
            let mut inherited = ptr::null_mut();
            if DuplicateHandle(
                GetCurrentProcess(),
                self.job.as_raw_handle(),
                GetCurrentProcess(),
                &mut inherited,
                JOB_OBJECT_QUERY,
                1,
                0,
            ) == 0
            {
                return Err(last_error("CoreJobLease"));
            }
            let lease = OwnedHandle::from_raw_handle(inherited);
            let mut bytes = 0;
            InitializeProcThreadAttributeList(ptr::null_mut(), 2, 0, &mut bytes);
            if bytes == 0 {
                return Err(last_error("CoreAttributeSize"));
            }
            let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
            let list = storage.as_mut_ptr().cast();
            if InitializeProcThreadAttributeList(list, 2, 0, &mut bytes) == 0 {
                return Err(last_error("CoreAttributeInit"));
            }
            struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
            impl Drop for Attributes {
                fn drop(&mut self) {
                    // SAFETY: uniquely owned initialized list; storage is live.
                    unsafe { DeleteProcThreadAttributeList(self.0) }
                }
            }
            let job = self.job.as_raw_handle();
            let inherited = lease.as_raw_handle();
            let attributes = Attributes(list);
            if UpdateProcThreadAttribute(
                attributes.0,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                (&job as *const HANDLE).cast(),
                size_of::<HANDLE>(),
                ptr::null_mut(),
                ptr::null(),
            ) == 0
            {
                return Err(last_error("CoreJobAttribute"));
            }
            if UpdateProcThreadAttribute(
                attributes.0,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                (&inherited as *const HANDLE).cast(),
                size_of::<HANDLE>(),
                ptr::null_mut(),
                ptr::null(),
            ) == 0
            {
                return Err(last_error("CoreJobLeaseAttribute"));
            }
            let mut startup: STARTUPINFOEXW = std::mem::zeroed();
            startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
            startup.lpAttributeList = attributes.0;
            let mut info: PROCESS_INFORMATION = std::mem::zeroed();
            if CreateProcessW(
                application.as_ptr(),
                command.as_mut_ptr(),
                ptr::null(),
                ptr::null(),
                1,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT | DETACHED_PROCESS,
                ptr::null(),
                cwd.as_ptr(),
                &startup.StartupInfo,
                &mut info,
            ) == 0
            {
                return Err(last_error("CreateCoreInJob"));
            }
            let handle = OwnedHandle::from_raw_handle(info.hProcess);
            drop(OwnedHandle::from_raw_handle(info.hThread));
            let actual = identity::inspect_handle(handle.as_raw_handle())?;
            verify_process(&self.witness, &actual)?;
            Ok(CoreProcess {
                handle,
                identity: actual,
            })
        }
        // The core owns the remaining lease, keeping its job name reopenable.
    }
}
fn create_job(witness: &Witness) -> Result<OwnedHandle> {
    let name = crate::wide(OsStr::new(&witness.name()))?;
    let sddl = crate::wide(OsStr::new(&format!(
        "D:P(A;;GA;;;SY)(A;;GA;;;{})",
        witness.creator.user_sid
    )))?;
    // SAFETY: valid SDDL/terminated name, descriptor remains live through create;
    // owned non-inheritable handle; a preexisting name is never accepted.
    unsafe {
        let mut descriptor = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(last_error("CoreJobSecurity"));
        }
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        let handle = CreateJobObjectW(&attributes, name.as_ptr());
        let code = GetLastError();
        LocalFree(descriptor);
        if handle.is_null() {
            return Err(Error::Windows {
                operation: "CreateCoreJob",
                code,
            });
        }
        let handle = OwnedHandle::from_raw_handle(handle);
        if code == ERROR_ALREADY_EXISTS {
            return Err(Error::Invalid("CORE_JOB_NAME_CONFLICT"));
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if SetInformationJobObject(
            handle.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of_val(&limits) as u32,
        ) == 0
        {
            return Err(last_error("CoreJobLifetime"));
        }
        Ok(handle)
    }
}
fn verify_process(witness: &Witness, process: &ProcessIdentity) -> Result<()> {
    if process.user_sid != witness.creator.user_sid
        || process.session_id != witness.creator.session_id
        || process.image_file != witness.image
        || process.creation_time < witness.creator.creation_time
    {
        return Err(Error::Invalid("CORE_JOB_PROCESS_UNCONFIRMED"));
    }
    Ok(())
}
impl Store {
    pub fn resolve_core_start_request(
        &mut self,
        generation: Uuid,
        process: Option<ProcessIdentity>,
    ) -> Result<()> {
        use crate::core_requests::CoreRequestPhase;
        use app_proxy_core::core_control::CoreOutcome;
        let header = self.load()?;
        let witness: Witness = store::decode(&store::read_protected(
            &self.root().join(PATH),
            &header.owner_sid,
            LIMIT,
        )?)?;
        if witness.version != 1
            || witness.store_id != header.store_id
            || witness.generation != generation
        {
            return Err(Error::Invalid("CORE_START_WITNESS_MISMATCH"));
        }
        if let Some((id, action)) = witness.request {
            if !matches!(
                action,
                app_proxy_core::core_control::CoreAction::Start { .. }
            ) {
                return Err(Error::Invalid("CORE_START_WITNESS_MISMATCH"));
            }
            if matches!(
                self.core_request_status(id)?,
                Some(
                    CoreRequestPhase::Pending { .. }
                        | CoreRequestPhase::Complete {
                            outcome: CoreOutcome::Indeterminate { .. },
                            ..
                        }
                )
            ) {
                self.resolve_core_preparation_receipt(
                    id,
                    &action,
                    CoreOutcome::Reconciled {
                        generation,
                        process,
                    },
                )?;
            }
        }
        Ok(())
    }

    /// Read-only reconciliation evidence. Caller serializes lifecycle work and
    /// performs the state CAS; this function never spawns or terminates anything.
    pub fn inspect_core_start(&self, generation: Uuid) -> Result<Option<CoreProcess>> {
        let _pins = self.core_directories(false)?;
        self.open_core_generation(generation)?;
        if !self.root().join(PATH).try_exists()? {
            return Err(Error::Invalid("CORE_START_WITNESS_MISSING"));
        }
        let header = self.load()?;
        let witness: Witness = store::decode(&store::read_protected(
            &self.root().join(PATH),
            &header.owner_sid,
            LIMIT,
        )?)?;
        let current = identity::current()?;
        if witness.version != 1
            || witness.store_id != header.store_id
            || witness.generation != generation
            || witness.nonce.is_nil()
            || witness.creator.user_sid != current.user_sid
            || witness.creator.session_id != current.session_id
            || witness.creator.pid == 0
            || witness.creator.creation_time == 0
            || !witness.executable.is_absolute()
        {
            return Err(Error::Invalid("CORE_START_WITNESS_MISMATCH"));
        }
        if witness.creator != current && CoreProcess::recover(&witness.creator)?.is_some() {
            return Err(Error::Invalid("CORE_START_CREATOR_STILL_RUNNING"));
        }
        let name = crate::wide(OsStr::new(&witness.name()))?;
        // SAFETY: name terminated; querying an ACL-protected exact named job;
        // fixed capacity allows only the one original process, never children.
        unsafe {
            let handle = OpenJobObjectW(JOB_OBJECT_QUERY, 0, name.as_ptr());
            if handle.is_null() {
                let code = GetLastError();
                if code == ERROR_FILE_NOT_FOUND {
                    return Ok(None);
                }
                return Err(Error::Windows {
                    operation: "OpenCoreJob",
                    code,
                });
            }
            let job = OwnedHandle::from_raw_handle(handle);
            let mut accounting: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
            if QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicAccountingInformation,
                (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                size_of_val(&accounting) as u32,
                ptr::null_mut(),
            ) == 0
            {
                return Err(last_error("QueryCoreJobAccounting"));
            }
            if accounting.ActiveProcesses == 0 {
                return Ok(None);
            }
            if accounting.TotalProcesses != 1 || accounting.ActiveProcesses != 1 {
                return Err(Error::Invalid("CORE_JOB_MEMBERS_UNCONFIRMED"));
            }
            let mut members: JOBOBJECT_BASIC_PROCESS_ID_LIST = std::mem::zeroed();
            if QueryInformationJobObject(
                job.as_raw_handle(),
                JobObjectBasicProcessIdList,
                (&mut members as *mut JOBOBJECT_BASIC_PROCESS_ID_LIST).cast(),
                size_of_val(&members) as u32,
                ptr::null_mut(),
            ) == 0
            {
                return Err(last_error("QueryCoreJobMembers"));
            }
            if members.NumberOfProcessIdsInList == 0 {
                return Ok(None);
            }
            if members.NumberOfAssignedProcesses != 1 || members.NumberOfProcessIdsInList != 1 {
                return Err(Error::Invalid("CORE_JOB_MEMBERS_UNCONFIRMED"));
            }
            let pid = u32::try_from(members.ProcessIdList[0])
                .map_err(|_| Error::Invalid("CORE_JOB_MEMBERS_UNCONFIRMED"))?;
            let process = identity::open(
                pid,
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE | PROCESS_TERMINATE,
            )?;
            let mut member = 0;
            if IsProcessInJob(process.as_raw_handle(), job.as_raw_handle(), &mut member) == 0
                || member == 0
            {
                return Err(Error::Invalid("CORE_JOB_PROCESS_UNCONFIRMED"));
            }
            let actual = identity::inspect_handle(process.as_raw_handle())?;
            verify_process(&witness, &actual)?;
            Ok(Some(CoreProcess {
                handle: process,
                identity: actual,
            }))
        }
    }
}
