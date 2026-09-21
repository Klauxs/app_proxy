//! Installer finalization runs from the newly installed frontend, so the normal
//! host capture and UAC ticket verification apply without an installer backdoor.
use crate::{configuration::Configuration, coordinator};
use app_proxy_core::model::Desired;
use app_proxy_windows::{
    Error, Result,
    guard_deployment::Deployment,
    guard_task::login::{
        Prepared,
        journal::{Action, Request},
    },
    store::Store,
};
use std::path::PathBuf;
use uuid::Uuid;

pub fn prepare(home: PathBuf) -> Result<()> {
    if !app_proxy_windows::setup::requested_at(&app_proxy_windows::setup::current_directory()?)? {
        return Err(Error::Invalid("SETUP_MAINTENANCE_REQUIRED"));
    }
    if !home.try_exists()? {
        return Ok(());
    }
    let configuration = Configuration::new(Store::open_expected(&home, None)?);
    let manifest = configuration.snapshot()?;
    let guarded = manifest
        .instances
        .iter()
        .any(|i| i.guard.desired == Desired::Enabled);
    let installed = match Deployment::listener(manifest.store_id) {
        Ok(_) => true,
        Err(Error::Invalid(app_proxy_core::error_code::GUARD_LISTENER_MISSING)) => false,
        Err(e) => return Err(e),
    };
    if guarded || installed {
        app_proxy_windows::guard_install::authorize_listener_update(manifest.store_id)?;
    }
    if guarded {
        let deployment = Deployment::listener(manifest.store_id)?;
        let prepared = Prepared::authorized(&deployment, &home)?;
        // Same-directory upgrades keep the ordinary task's exact action.
        // A deleted/modified task is not silently replaced by a different action.
        let existing = configuration.lock()?.login_registration()?;
        if existing.is_none() {
            crate::login_tasks::apply(
                &configuration,
                &home,
                &Request {
                    id: Uuid::new_v4(),
                    expected_revision: manifest.revision,
                    action: Action::Create,
                    expected_creation: None,
                },
            )?;
        } else {
            prepared.register()?;
            if !crate::login_tasks::status(&configuration, &home)?.ready {
                return Err(Error::Invalid("SETUP_LOGIN_NOT_READY"));
            }
        }
    }
    Ok(())
}

pub async fn verify(home: PathBuf) -> Result<()> {
    if !home.try_exists()? {
        return Ok(());
    }
    coordinator::status(home.clone()).await?;
    let instances = coordinator::catalog(home.clone())
        .await?
        .instances
        .into_iter()
        .filter(|i| i.guard == Desired::Enabled)
        .map(|i| i.id)
        .collect::<Vec<_>>();
    for instance in instances {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(45);
        loop {
            let status = coordinator::guard_status(home.clone(), instance).await?;
            if status.listener == crate::guard_control::ComponentState::ActiveEtw {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Invalid("SETUP_LISTENER_NOT_READY"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
    Ok(())
}
