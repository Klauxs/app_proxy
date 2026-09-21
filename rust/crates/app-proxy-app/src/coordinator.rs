//! Authenticated coordinator RPC; configuration writes use durable request records.
//!
//! `protocol` holds the wire types, `server` the store owner, `client` the
//! callers' side. Everything public is re-exported here.
use crate::configuration::{CatalogPage, Configuration, catalog_page};
use crate::core_control::CoreControl;
use app_proxy_core::core_control::{CoreAction, CoreRequestStatus};
use app_proxy_core::launch::{LaunchAttempt, LaunchOrigin, LaunchRequest};
use app_proxy_core::{
    model::Manifest,
    registry::{ConfigAction, ConfigRequest},
};
use app_proxy_windows::config_transaction::{ConfigOutcome, ConfigRequestStatus};
use app_proxy_windows::{Error, Result, identity, ipc, process, store};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::net::windows::named_pipe::NamedPipeServer;
use uuid::Uuid;

mod client;
mod protocol;
mod server;

pub use client::*;
pub use protocol::Status;
pub use server::{serve, serve_expected};
// The test modules reach every internal item through `super::*`.
#[cfg(test)]
use {protocol::*, server::*};

#[cfg(test)]
mod launch_tests;
#[cfg(test)]
mod tests;

pub fn default_home() -> Result<PathBuf> {
    Ok(app_proxy_windows::layout::ensure_root()?.join("data"))
}

fn binaries() -> Result<(PathBuf, PathBuf)> {
    let current = std::env::current_exe()?;
    let directory = current
        .parent()
        .ok_or(Error::Invalid("PROGRAM_DIRECTORY_UNAVAILABLE"))?;
    Ok((
        directory.join("app-proxy.exe"),
        directory.join("app-proxy-host.exe"),
    ))
}
