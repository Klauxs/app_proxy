//! Ordinary login entry. The caller must persist `Registration` before calling
//! register/remove and explicitly reconcile an interrupted or unknown result.
use super::*;
use crate::{
    guard_deployment::{CoordinatorImage, InstallerSource},
    process::quote_windows_word,
    store,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registration {
    store_id: Uuid,
    owner_sid: String,
    home: PathBuf,
    host: PathBuf,
}

pub mod journal;

impl Registration {
    /// Read-only readiness includes the pinned protected host and listener, not
    /// merely a historical registration or a matching Task Scheduler action.
    pub fn ready(&self, home: &Path) -> Result<bool> {
        let descriptor = store::describe(&self.home)?;
        if descriptor.store_id != self.store_id
            || descriptor.owner_sid != self.owner_sid
            || std::fs::canonicalize(home)? != std::fs::canonicalize(&self.home)?
        {
            return Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"));
        }
        if !self.exists_verified()? {
            return Ok(false);
        }
        let deployment = Deployment::listener(self.store_id)?;
        let image = deployment.coordinator()?;
        if image.path() != self.host {
            return Err(Error::Invalid("GUARD_LOGIN_REGISTRATION_CHANGED"));
        }
        super::verify_registered(&deployment)?;
        Ok(true)
    }
    pub fn matches_metadata(&self, metadata: &app_proxy_core::model::LoginTask) -> Result<bool> {
        let expected = self.metadata()?;
        Ok(expected.name == metadata.name
            && expected.target == metadata.target
            && expected.args == metadata.args)
    }
    fn metadata(&self) -> Result<app_proxy_core::model::LoginTask> {
        let spec = self.spec()?;
        Ok(app_proxy_core::model::LoginTask {
            name: spec.name,
            target: self.host.clone(),
            args: vec![
                "serve".into(),
                "--home".into(),
                self.home.to_string_lossy().into_owned(),
                "--expected-store".into(),
                self.store_id.to_string(),
            ],
        })
    }
}

/// Held executable/parent pins from the authorized deployment remain live until
/// registration completes. Query/removal intentionally need no current binary.
pub struct Prepared<'a> {
    registration: Registration,
    deployment: &'a Deployment,
    _image: CoordinatorImage,
}
impl<'a> Prepared<'a> {
    pub fn authorized(deployment: &'a Deployment, home: &Path) -> Result<Self> {
        identity::assert_ordinary_user()?;
        let descriptor = store::describe(home)?;
        if descriptor.store_id != deployment.store_id() {
            return Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"));
        }
        super::verify_registered(deployment)?;
        if !InstallerSource::capture()?.matches_listener(deployment) {
            return Err(Error::Invalid("GUARD_LISTENER_RELEASE_CONFLICT"));
        }
        let image = deployment.coordinator()?;
        let registration = Registration {
            store_id: descriptor.store_id,
            owner_sid: descriptor.owner_sid,
            home: home.into(),
            host: image.path().into(),
        };
        registration.spec()?;
        Ok(Self {
            registration,
            deployment,
            _image: image,
        })
    }
    pub fn registration(&self) -> &Registration {
        &self.registration
    }
    /// Creates only if absent; never updates, runs or replaces an existing task.
    pub fn register(&self) -> Result<()> {
        identity::assert_ordinary_user()?;
        let descriptor = store::describe(&self.registration.home)?;
        if descriptor.store_id != self.registration.store_id {
            return Err(Error::Invalid("GUARD_LOGIN_STORE_MISMATCH"));
        }
        super::verify_registered(self.deployment)?;
        register_spec(&self.registration.spec()?)
    }
}
impl Registration {
    fn spec(&self) -> Result<Spec> {
        let sid = identity::current()?.user_sid;
        if self.owner_sid != sid {
            return Err(Error::Invalid("GUARD_LOGIN_OWNER_MISMATCH"));
        }
        crate::ipc::address(self.store_id, &sid)?;
        let home = task_path(&self.home)?;
        let path = task_path(&self.host)?;
        if self.host.file_name().and_then(|n| n.to_str()) != Some("app-proxy-host.exe") {
            return Err(Error::Invalid("GUARD_LOGIN_HOST_REQUIRED"));
        }
        let quoted = quote_windows_word(std::ffi::OsStr::new(home))?;
        let args = format!(
            "serve --home {} --expected-store {}",
            String::from_utf16(&quoted).map_err(|_| Error::Invalid("GUARD_LOGIN_PATH"))?,
            self.store_id
        );
        let scope = format!("{:x}", Sha256::digest(sid.as_bytes()));
        if args.encode_utf16().count() + path.encode_utf16().count() + 4 > 32767 {
            return Err(Error::Invalid("GUARD_LOGIN_PATH"));
        }
        let name = format!("AppProxy-Login-{}-{}", &scope[..16], self.store_id);
        Ok(Spec {
            uri: format!("\\{name}"),
            name,
            sid: sid.clone(),
            path: path.into(),
            args,
            cwd: self
                .host
                .parent()
                .and_then(Path::to_str)
                .ok_or(Error::Invalid("GUARD_LOGIN_PATH"))?
                .into(),
            login: true,
        })
    }
    /// Missing is distinct from an unreadable, modified or foreign task.
    pub fn exists_verified(&self) -> Result<bool> {
        identity::assert_ordinary_user()?;
        let spec = self.spec()?;
        let session = Session::connect()?;
        let Some(task) = session.find(&spec)? else {
            return Ok(false);
        };
        verify(&task, &spec)?;
        Ok(true)
    }
    /// No Stop call. Integration maintenance must first arrange an idle task;
    /// failed verification leaves both the task and its recovery record intact.
    pub fn remove_idle(&self) -> Result<()> {
        identity::assert_ordinary_user()?;
        let spec = self.spec()?;
        let session = Session::connect()?;
        let Some(task) = session.find(&spec)? else {
            return Ok(());
        };
        verify(&task, &spec)?;
        // SAFETY: same-apartment verified task, fixed name, no force-stop.
        unsafe {
            if task
                .GetInstances(0)
                .map_err(com_error)?
                .Count()
                .map_err(com_error)?
                != 0
            {
                return Err(Error::Invalid("GUARD_LOGIN_TASK_RUNNING"));
            }
            session
                .root
                .DeleteTask(&BSTR::from(&spec.name), 0)
                .map_err(com_error)?;
        }
        if session.find(&spec)?.is_some() {
            return Err(Error::Invalid("GUARD_LOGIN_REMOVE_UNCONFIRMED"));
        }
        Ok(())
    }
}
fn task_path(path: &Path) -> Result<&str> {
    let value = path.to_str().ok_or(Error::Invalid("GUARD_LOGIN_PATH"))?;
    if !path.is_absolute()
        || value.contains(['\0', '%', '"'])
        || value.contains("$(")
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::Invalid("GUARD_LOGIN_PATH"));
    }
    Ok(value)
}
fn register_spec(spec: &Spec) -> Result<()> {
    let session = Session::connect()?;
    if let Some(task) = session.find(spec)? {
        return verify(&task, spec);
    }
    let definition = build(&session.service, spec)?;
    // SAFETY: current user only, LUA, fixed action and trigger, CREATE never
    // overwrites a concurrent task. Readback failures retain recovery evidence.
    let task = unsafe {
        session.root.RegisterTaskDefinition(
            &BSTR::from(&spec.name),
            &definition,
            TASK_CREATE.0 | TASK_DONT_ADD_PRINCIPAL_ACE.0,
            &VARIANT::from(spec.sid.as_str()),
            &VARIANT::default(),
            TASK_LOGON_INTERACTIVE_TOKEN,
            &VARIANT::from(security::login_sddl(&spec.sid).as_str()),
        )
    }
    .map_err(com_error)?;
    verify(&task, spec)
}
pub(super) fn add_trigger(task: &ITaskDefinition, sid: &str) -> Result<()> {
    // SAFETY: owned unregistered COM definition, current-apartment typed casts.
    unsafe {
        let trigger: ILogonTrigger = task
            .Triggers()
            .map_err(com_error)?
            .Create(TASK_TRIGGER_LOGON)
            .map_err(com_error)?
            .cast()
            .map_err(com_error)?;
        trigger
            .SetId(&BSTR::from("AppProxyLogon"))
            .map_err(com_error)?;
        trigger.SetUserId(&BSTR::from(sid)).map_err(com_error)?;
        trigger.SetEnabled(VARIANT_TRUE).map_err(com_error)?;
        trigger
            .SetExecutionTimeLimit(&BSTR::from("PT0S"))
            .map_err(com_error)?;
    }
    Ok(())
}
pub(super) fn verify_trigger(task: &ITaskDefinition, sid: &str) -> Result<()> {
    // SAFETY: retained same-apartment objects and initialized output values.
    unsafe {
        let triggers = task.Triggers().map_err(com_error)?;
        let mut count = 0;
        triggers.Count(&mut count).map_err(com_error)?;
        if count != 1 {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
        let trigger = triggers.get_Item(1).map_err(com_error)?;
        let mut kind = TASK_TRIGGER_TYPE2::default();
        trigger.Type(&mut kind).map_err(com_error)?;
        if kind != TASK_TRIGGER_LOGON {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
        let trigger: ILogonTrigger = trigger.cast().map_err(com_error)?;
        let (mut id, mut user, mut delay, mut start, mut end, mut limit) = (
            BSTR::new(),
            BSTR::new(),
            BSTR::new(),
            BSTR::new(),
            BSTR::new(),
            BSTR::new(),
        );
        let mut enabled = VARIANT_BOOL::default();
        trigger.Id(&mut id).map_err(com_error)?;
        trigger.UserId(&mut user).map_err(com_error)?;
        trigger.Delay(&mut delay).map_err(com_error)?;
        trigger.StartBoundary(&mut start).map_err(com_error)?;
        trigger.EndBoundary(&mut end).map_err(com_error)?;
        trigger.ExecutionTimeLimit(&mut limit).map_err(com_error)?;
        trigger.Enabled(&mut enabled).map_err(com_error)?;
        let repetition = trigger.Repetition().map_err(com_error)?;
        let (mut interval, mut duration) = (BSTR::new(), BSTR::new());
        let mut stop = VARIANT_BOOL::default();
        repetition.Interval(&mut interval).map_err(com_error)?;
        repetition.Duration(&mut duration).map_err(com_error)?;
        repetition.StopAtDurationEnd(&mut stop).map_err(com_error)?;
        if text(id)? != "AppProxyLogon"
            || !security::user_matches(&text(user)?, sid)?
            || !delay.is_empty()
            || !start.is_empty()
            || !end.is_empty()
            || text(limit)? != "PT0S"
            || enabled != VARIANT_TRUE
            || !interval.is_empty()
            || !duration.is_empty()
            || stop != VARIANT_FALSE
        {
            return Err(Error::Invalid("GUARD_TASK_CONFLICT"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
