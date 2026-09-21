//! Identity evidence as data. Obtaining and verifying it is the platform's job.
//!
//! The fields follow what Windows can attest: a user SID, a logon session and a
//! volume-scoped file index. Another platform would supply its own equivalents.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Data only: a PID alone must never authorize a stop operation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub creation_time: u64,
    pub user_sid: String,
    pub session_id: u32,
    pub image_path: PathBuf,
    pub image_file: FileIdentity,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileIdentity {
    pub volume_serial: u32,
    pub file_index: u64,
}
