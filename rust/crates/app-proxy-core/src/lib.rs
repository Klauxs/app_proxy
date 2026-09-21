#![forbid(unsafe_code)]

pub mod core_control;
pub mod environment;
pub mod error_code;
pub mod identity;
pub mod launch;
pub mod model;
pub mod registry;
pub mod singbox;
pub mod subscription;
pub mod template;

pub use environment::{EnvPatch, EnvironmentError};
pub use identity::{FileIdentity, ProcessIdentity};
