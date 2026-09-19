//! Fixed, demand-start elevated event task. No arbitrary task name, executable,
//! arguments, credentials, trigger, update or forced-stop API is exposed.
//! Synchronous COM calls belong on the caller's bounded blocking worker.
use crate::{Error, Result, guard_deployment::Deployment, identity};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;
use windows::{
    Win32::{
        Foundation::{VARIANT_BOOL, VARIANT_FALSE, VARIANT_TRUE},
        System::{Com::*, TaskScheduler::*, Variant::VARIANT},
    },
    core::{BSTR, Interface},
};

mod security;
const MARKER: &str = "app-proxy-rust:event-task:v1";
const MISSING: u32 = 0x80070002;

#[derive(Serialize)]
pub struct RunReceipt {
    pub task_name: String,
    pub task_instance: Uuid,
}

struct Spec {
    name: String,
    sid: String,
    path: String,
    args: String,
    cwd: String,
    uri: String,
}
impl Spec {
    fn deployment(deployment: &Deployment) -> Result<Self> {
        Self::new(
            &identity::current()?.user_sid,
            deployment.store_id(),
            deployment.generation(),
            deployment.host_path(),
        )
    }
    fn new(sid: &str, store: Uuid, generation: Uuid, path: &Path) -> Result<Self> {
        crate::ipc::address(store, sid)?;
        if generation.is_nil() {
            return Err(Error::Invalid("GUARD_DEPLOYMENT_ID_REQUIRED"));
        }
        let scope = format!("{:x}", Sha256::digest(sid.as_bytes()));
        let path_text = path.to_str().ok_or(Error::Invalid("GUARD_TASK_PATH"))?;
        let cwd = path
            .parent()
            .and_then(Path::to_str)
            .ok_or(Error::Invalid("GUARD_TASK_PATH"))?;
        if !path.is_absolute() || path_text.contains(['\0', '%', '"']) || path_text.contains("$(") {
            return Err(Error::Invalid("GUARD_TASK_PATH"));
        }
        Ok(Self {
            name: format!("AppProxyRust-Event-{}-{store}", &scope[..16]),
            sid: sid.into(),
            path: path_text.into(),
            cwd: cwd.into(),
            args: format!("event-listen --store {store} --generation {generation}"),
            uri: format!("{MARKER}:{sid}:{store}:{generation}"),
        })
    }
}

/// Creates only if absent. An existing exact task is reused; any mismatch is a
/// conflict, never silently overwritten. Registration does not start the task.
pub fn register(deployment: &Deployment) -> Result<()> {
    identity::assert_elevated_user()?;
    let spec = Spec::deployment(deployment)?;
    let session = Session::connect()?;
    if let Some(task) = session.find(&spec)? {
        return verify(&task, &spec);
    }
    let definition = build(&session.service, &spec)?;
    let descriptor = security::sddl(&spec.sid);
    // SAFETY: COM objects and owned strings/variants live through the synchronous
    // registration. CREATE refuses a concurrent same-name task; principal ACL
    // additions are disabled so an ordinary user cannot gain write permission.
    let task = unsafe {
        session.root.RegisterTaskDefinition(
            &BSTR::from(&spec.name),
            &definition,
            TASK_CREATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
            &VARIANT::from(spec.sid.as_str()),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::from(descriptor.as_str()),
        )
    }
    .map_err(com_error)?;
    // A failed readback keeps the task for recovery; never delete an unverified
    // task or claim that registration definitely had no side effect.
    verify(&task, &spec)
}

/// Checks definition and ACL in addition to the caller-held protected files.
/// This proves a registered task, not that its listener is running or healthy.
pub fn verify_registered(deployment: &Deployment) -> Result<()> {
    let spec = Spec::deployment(deployment)?;
    let session = Session::connect()?;
    verify(
        &session
            .find(&spec)?
            .ok_or(Error::Invalid("GUARD_TASK_MISSING"))?,
        &spec,
    )
}

/// Ordinary coordinator requests its own current session only. The returned
/// scheduler instance GUID is not a listener PID or authenticated event stream.
pub fn run(deployment: &Deployment) -> Result<RunReceipt> {
    identity::assert_ordinary_user()?;
    let coordinator = deployment.coordinator()?;
    let current = identity::current()?;
    if current.image_file != *coordinator.image() {
        return Err(Error::Invalid("GUARD_COORDINATOR_CHANGED"));
    }
    let spec = Spec::deployment(deployment)?;
    let session = Session::connect()?;
    let task = session
        .find(&spec)?
        .ok_or(Error::Invalid("GUARD_TASK_MISSING"))?;
    verify(&task, &spec)?;
    let session_id =
        i32::try_from(current.session_id).map_err(|_| Error::Invalid("GUARD_TASK_SESSION"))?;
    if session_id <= 0 {
        return Err(Error::Invalid("GUARD_TASK_SESSION"));
    }
    // SAFETY: task has an exact verified definition; no substitution parameters,
    // alternate credentials, arbitrary user or session are supplied.
    let running = unsafe {
        task.RunEx(
            &VARIANT::default(),
            TASK_RUN_USE_SESSION_ID.0,
            session_id,
            &BSTR::new(),
        )
    }
    .map_err(com_error)?;
    // SAFETY: registered running-task object is retained for this query.
    let guid = text(unsafe { running.InstanceGuid() }.map_err(com_error)?)?;
    let task_instance =
        Uuid::parse_str(&guid).map_err(|_| Error::Invalid("GUARD_TASK_INSTANCE_UNKNOWN"))?;
    if task_instance.is_nil() {
        return Err(Error::Invalid("GUARD_TASK_INSTANCE_UNKNOWN"));
    }
    Ok(RunReceipt {
        task_name: spec.name,
        task_instance,
    })
}

/// Removal requires elevation and a matching, idle task. It does not stop a
/// listener, application, or unknown scheduler instance, nor remove the helper.
/// The integration transaction must first quiesce its ordinary run requests;
/// scheduler readback and deletion are not an atomic lock against another Run.
pub fn remove_idle(deployment: &Deployment) -> Result<()> {
    identity::assert_elevated_user()?;
    let spec = Spec::deployment(deployment)?;
    let session = Session::connect()?;
    let Some(task) = session.find(&spec)? else {
        return Ok(());
    };
    verify(&task, &spec)?;
    // SAFETY: live verified COM task; only enumerates its instances and removes
    // its fixed registration. Privileged concurrent modifications are not atomic.
    unsafe {
        if task
            .GetInstances(0)
            .map_err(com_error)?
            .Count()
            .map_err(com_error)?
            != 0
        {
            return Err(Error::Invalid("GUARD_TASK_RUNNING"));
        }
        session
            .root
            .DeleteTask(&BSTR::from(&spec.name), 0)
            .map_err(com_error)?;
    }
    if session.find(&spec)?.is_some() {
        return Err(Error::Invalid("GUARD_TASK_REMOVE_UNCONFIRMED"));
    }
    Ok(())
}

struct Apartment;
impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances successful CoInitializeEx on the same calling thread.
        unsafe { CoUninitialize() }
    }
}
struct Session {
    service: ITaskService,
    root: ITaskFolder,
    _apartment: Apartment,
}
impl Session {
    fn connect() -> Result<Self> {
        // SAFETY: all COM objects stay in the current synchronous operation and
        // drop before the apartment. No server/user/password strings are accepted.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(com_error)?;
            let apartment = Apartment;
            let service: ITaskService =
                CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER).map_err(com_error)?;
            let empty = VARIANT::default();
            service
                .Connect(&empty, &empty, &empty, &empty)
                .map_err(com_error)?;
            let root = service.GetFolder(&BSTR::from("\\")).map_err(com_error)?;
            Ok(Self {
                service,
                root,
                _apartment: apartment,
            })
        }
    }
    fn find(&self, spec: &Spec) -> Result<Option<IRegisteredTask>> {
        // SAFETY: same-apartment root object and fixed task-name BSTR.
        match unsafe { self.root.GetTask(&BSTR::from(&spec.name)) } {
            Ok(task) => Ok(Some(task)),
            Err(error) if error.code().0 as u32 == MISSING => Ok(None),
            Err(error) => Err(com_error(error)),
        }
    }
}
fn com_error(error: windows::core::Error) -> Error {
    Error::Windows {
        operation: "GuardTaskScheduler",
        code: error.code().0 as u32,
    }
}
fn text(value: BSTR) -> Result<String> {
    if value.len() > 65536 {
        return Err(Error::Invalid("GUARD_TASK_TEXT_SIZE"));
    }
    String::from_utf16(&value).map_err(|_| Error::Invalid("GUARD_TASK_TEXT_ENCODING"))
}

fn build(service: &ITaskService, spec: &Spec) -> Result<ITaskDefinition> {
    // SAFETY: the definition is an owned, unregistered local COM object. All
    // values are fixed policy or derived from the held protected deployment.
    unsafe {
        let task = service.NewTask(0).map_err(com_error)?;
        task.SetData(&BSTR::from(MARKER)).map_err(com_error)?;
        let info = task.RegistrationInfo().map_err(com_error)?;
        info.SetAuthor(&BSTR::from("AppProxyRust"))
            .map_err(com_error)?;
        info.SetURI(&BSTR::from(&spec.uri)).map_err(com_error)?;
        let principal = task.Principal().map_err(com_error)?;
        principal
            .SetId(&BSTR::from("AppProxyRustOwner"))
            .map_err(com_error)?;
        principal
            .SetUserId(&BSTR::from(&spec.sid))
            .map_err(com_error)?;
        principal
            .SetLogonType(TASK_LOGON_INTERACTIVE_TOKEN)
            .map_err(com_error)?;
        principal
            .SetRunLevel(TASK_RUNLEVEL_HIGHEST)
            .map_err(com_error)?;
        let settings = task.Settings().map_err(com_error)?;
        settings
            .SetCompatibility(TASK_COMPATIBILITY_V2)
            .map_err(com_error)?;
        settings
            .SetAllowDemandStart(VARIANT_TRUE)
            .map_err(com_error)?;
        settings
            .SetMultipleInstances(TASK_INSTANCES_IGNORE_NEW)
            .map_err(com_error)?;
        settings
            .SetDisallowStartIfOnBatteries(VARIANT_FALSE)
            .map_err(com_error)?;
        settings
            .SetStopIfGoingOnBatteries(VARIANT_FALSE)
            .map_err(com_error)?;
        settings
            .SetAllowHardTerminate(VARIANT_FALSE)
            .map_err(com_error)?;
        settings
            .SetStartWhenAvailable(VARIANT_FALSE)
            .map_err(com_error)?;
        settings
            .SetRunOnlyIfNetworkAvailable(VARIANT_FALSE)
            .map_err(com_error)?;
        settings
            .SetRunOnlyIfIdle(VARIANT_FALSE)
            .map_err(com_error)?;
        settings.SetWakeToRun(VARIANT_FALSE).map_err(com_error)?;
        settings.SetRestartCount(0).map_err(com_error)?;
        settings
            .SetExecutionTimeLimit(&BSTR::from("PT0S"))
            .map_err(com_error)?;
        settings.SetHidden(VARIANT_TRUE).map_err(com_error)?;
        settings.SetEnabled(VARIANT_TRUE).map_err(com_error)?;
        let actions = task.Actions().map_err(com_error)?;
        actions
            .SetContext(&BSTR::from("AppProxyRustOwner"))
            .map_err(com_error)?;
        let action: IExecAction = actions
            .Create(TASK_ACTION_EXEC)
            .map_err(com_error)?
            .cast()
            .map_err(com_error)?;
        action.SetPath(&BSTR::from(&spec.path)).map_err(com_error)?;
        action
            .SetArguments(&BSTR::from(&spec.args))
            .map_err(com_error)?;
        action
            .SetWorkingDirectory(&BSTR::from(&spec.cwd))
            .map_err(com_error)?;
        Ok(task)
    }
}

fn verify(task: &IRegisteredTask, spec: &Spec) -> Result<()> {
    // SAFETY: retained registered task, same-apartment property reads only.
    unsafe {
        if text(task.Path().map_err(com_error)?)? != format!("\\{}", spec.name) {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
        let descriptor = text(task.GetSecurityDescriptor(5).map_err(com_error)?)?; // OWNER | DACL
        security::verify(&descriptor, &spec.sid)?;
        verify_definition(&task.Definition().map_err(com_error)?, spec)
    }
}

fn verify_definition(task: &ITaskDefinition, spec: &Spec) -> Result<()> {
    // SAFETY: same-apartment retained definition; correctly typed output buffers.
    unsafe {
        let mut value = BSTR::new();
        task.Data(&mut value).map_err(com_error)?;
        let mut matches = text(value)? == MARKER;
        let info = task.RegistrationInfo().map_err(com_error)?;
        let mut uri = BSTR::new();
        info.URI(&mut uri).map_err(com_error)?;
        matches &= text(uri)? == spec.uri;
        let principal = task.Principal().map_err(com_error)?;
        let mut user = BSTR::new();
        let mut principal_id = BSTR::new();
        principal.Id(&mut principal_id).map_err(com_error)?;
        let mut logon = TASK_LOGON_TYPE::default();
        let mut level = TASK_RUNLEVEL_TYPE::default();
        let mut group = BSTR::new();
        principal.UserId(&mut user).map_err(com_error)?;
        principal.GroupId(&mut group).map_err(com_error)?;
        principal.LogonType(&mut logon).map_err(com_error)?;
        principal.RunLevel(&mut level).map_err(com_error)?;
        matches &= text(principal_id)? == "AppProxyRustOwner"
            && security::user_matches(&text(user)?, &spec.sid)?
            && group.is_empty()
            && logon == TASK_LOGON_INTERACTIVE_TOKEN
            && level == TASK_RUNLEVEL_HIGHEST;
        let mut count = 0;
        task.Triggers()
            .map_err(com_error)?
            .Count(&mut count)
            .map_err(com_error)?;
        matches &= count == 0;
        let actions = task.Actions().map_err(com_error)?;
        let mut context = BSTR::new();
        actions.Context(&mut context).map_err(com_error)?;
        matches &= text(context)? == "AppProxyRustOwner";
        actions.Count(&mut count).map_err(com_error)?;
        if count != 1 {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
        let action: IExecAction = actions
            .get_Item(1)
            .map_err(com_error)?
            .cast()
            .map_err(com_error)?;
        let (mut path, mut args, mut cwd) = (BSTR::new(), BSTR::new(), BSTR::new());
        action.Path(&mut path).map_err(com_error)?;
        action.Arguments(&mut args).map_err(com_error)?;
        action.WorkingDirectory(&mut cwd).map_err(com_error)?;
        matches &= text(path)? == spec.path && text(args)? == spec.args && text(cwd)? == spec.cwd;
        let settings = task.Settings().map_err(com_error)?;
        let mut compatibility = TASK_COMPATIBILITY::default();
        settings
            .Compatibility(&mut compatibility)
            .map_err(com_error)?;
        matches &= compatibility == TASK_COMPATIBILITY_V2;
        type BoolGetter = unsafe fn(&ITaskSettings, *mut VARIANT_BOOL) -> windows::core::Result<()>;
        let checks: [(BoolGetter, VARIANT_BOOL); 10] = [
            (ITaskSettings::AllowDemandStart, VARIANT_TRUE),
            (ITaskSettings::DisallowStartIfOnBatteries, VARIANT_FALSE),
            (ITaskSettings::StopIfGoingOnBatteries, VARIANT_FALSE),
            (ITaskSettings::AllowHardTerminate, VARIANT_FALSE),
            (ITaskSettings::StartWhenAvailable, VARIANT_FALSE),
            (ITaskSettings::RunOnlyIfNetworkAvailable, VARIANT_FALSE),
            (ITaskSettings::RunOnlyIfIdle, VARIANT_FALSE),
            (ITaskSettings::WakeToRun, VARIANT_FALSE),
            (ITaskSettings::Hidden, VARIANT_TRUE),
            (ITaskSettings::Enabled, VARIANT_TRUE),
        ];
        for (getter, expected) in checks {
            let mut actual = VARIANT_BOOL::default();
            getter(&settings, &mut actual).map_err(com_error)?;
            matches &= actual == expected;
        }
        let mut policy = TASK_INSTANCES_POLICY::default();
        settings.MultipleInstances(&mut policy).map_err(com_error)?;
        let mut restarts = 0;
        settings.RestartCount(&mut restarts).map_err(com_error)?;
        let (mut limit, mut deletion) = (BSTR::new(), BSTR::new());
        settings.ExecutionTimeLimit(&mut limit).map_err(com_error)?;
        settings
            .DeleteExpiredTaskAfter(&mut deletion)
            .map_err(com_error)?;
        matches &= policy == TASK_INSTANCES_IGNORE_NEW
            && restarts == 0
            && text(limit)? == "PT0S"
            && deletion.is_empty();
        if !matches {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
