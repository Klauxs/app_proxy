#![cfg(windows)]

pub mod config_transaction;
pub mod core_process;
pub mod core_requests;
pub mod core_state;
pub mod core_update;
pub mod etw;
pub mod event_pipe;
pub mod guard_deployment;
pub mod guard_install;
pub mod guard_listener;
pub mod guard_task;
pub mod identity;
pub mod ifeo;
pub mod ifeo_entry;
pub mod ifeo_rules;
pub mod installation;
pub mod instance_data;
pub mod instance_process;
pub mod instance_resource;
pub mod ipc;
pub mod launch_state;
pub mod package;
pub mod package_launch;
pub mod process;
pub mod process_query;
pub mod process_stop;
pub mod singbox_binary;
pub mod singbox_install;
mod storage_security;
pub mod store;

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
