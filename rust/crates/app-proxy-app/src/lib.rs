#![forbid(unsafe_code)]
#![cfg(windows)]

pub mod configuration;
pub mod coordinator;
pub mod core_cli;
pub mod core_control;
mod core_installer;
pub mod core_manager;
mod core_ports;
mod core_reconfigure;
pub mod exit;
mod foreground;
pub mod guard_cli;
pub mod guard_control;
mod guard_monitor;
pub mod instance_cli;
pub mod instance_settings;
pub mod instance_status;
pub mod launch_cli;
pub mod launch_engine;
pub mod login_cli;
pub mod login_tasks;
pub mod menu;
mod output;
pub mod probe;
pub mod proxy_cli;
pub mod proxy_health;
pub mod setup;
pub mod shortcut_cli;
pub mod shortcuts;
mod subscription_cli;
pub mod subscription_download;
pub mod subscription_preview;
