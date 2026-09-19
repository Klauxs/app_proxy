#![forbid(unsafe_code)]
#![cfg(windows)]

pub mod configuration;
pub mod coordinator;
pub mod core_cli;
pub mod core_control;
mod core_installer;
pub mod core_manager;
mod core_reconfigure;
mod foreground;
pub mod instance_cli;
pub mod launch_cli;
pub mod launch_engine;
pub mod probe;
pub mod proxy_cli;
pub mod proxy_health;
