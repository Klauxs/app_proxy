#![cfg(windows)]

// Modules are grouped by domain on disk and in the module tree. Each one is also
// re-exported here, so `app_proxy_windows::store` and `crate::store` keep working.
/// The protected configuration store and its durable request journals.
pub mod storage {
    pub mod config_transaction;
    pub mod instance_data;
    pub(crate) mod journal;
    pub mod layout;
    pub(crate) mod storage_security;
    pub mod store;
    pub mod subscription_stage;
}

/// The shared sing-box process: binary, installation, state and reconfiguration.
pub mod proxy_core {
    pub mod core_process;
    pub mod core_requests;
    pub mod core_state;
    pub mod core_update;
    pub mod singbox_binary;
    pub mod singbox_install;
}

/// Creating an application process for an instance, including packaged apps.
pub mod launching {
    pub mod creation_guard;
    pub mod installation;
    pub mod instance_resource;
    pub mod launch_state;
    pub mod package;
    pub mod package_launch;
    pub mod process;
}

/// Exact process identity, observation and stopping.
pub mod processes {
    pub mod identity;
    pub mod instance_process;
    pub mod native_process;
    pub mod process_query;
    pub mod process_stop;
}

/// The elevated event listener and everything that deploys and schedules it.
pub mod guard {
    pub mod etw;
    pub mod event_pipe;
    pub mod guard_deployment;
    pub mod guard_install;
    pub mod guard_listener;
    pub mod guard_task;
}

/// Desktop integration, the console and the installer's primitives.
pub mod shell {
    pub mod console;
    pub mod setup;
    pub mod shortcuts;
}

/// Small operating system wrappers used across the crate.
pub mod system {
    pub(crate) mod com;
    pub mod diagnostic_timing;
    pub mod ipc;
    pub mod local_time;
    pub(crate) mod security_ffi;
}

pub use guard::{etw, event_pipe, guard_deployment, guard_install, guard_listener, guard_task};
pub use launching::{
    creation_guard, installation, instance_resource, launch_state, package, package_launch, process,
};
pub use processes::{identity, instance_process, native_process, process_query, process_stop};
pub use proxy_core::{
    core_process, core_requests, core_state, core_update, singbox_binary, singbox_install,
};
pub use shell::{console, setup, shortcuts};
pub use storage::{config_transaction, instance_data, layout, store, subscription_stage};
pub(crate) use storage::{journal, storage_security};
pub(crate) use system::{com, security_ffi};
pub use system::{diagnostic_timing, ipc, local_time};

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{operation} (Win32 {code})")]
    Windows { operation: &'static str, code: u32 },
    #[error("{0}")]
    Invalid(&'static str),
    #[error("PROCESS_IDENTITY_MISMATCH")]
    IdentityMismatch,
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Environment(#[from] app_proxy_core::EnvironmentError),
    #[error("INVALID_BRIDGE_RESPONSE")]
    Json(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

fn last_error(operation: &'static str) -> Error {
    Error::Windows {
        operation,
        code: io::Error::last_os_error().raw_os_error().unwrap_or(0) as u32,
    }
}

fn wide(value: &std::ffi::OsStr) -> Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(Error::Invalid("NUL_IN_WINDOWS_STRING"));
    }
    result.push(0);
    Ok(result)
}
