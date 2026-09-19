#![forbid(unsafe_code)]

pub mod core_control;
pub mod launch;
pub mod model;
pub mod registry;
pub mod singbox;
pub mod template;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
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

/// Intentionally not Debug: environment values can contain credentials.
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvPatch {
    #[serde(default)]
    pub set: BTreeMap<String, String>,
    #[serde(default)]
    pub unset: Vec<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EnvironmentError {
    #[error("INVALID_ENV_NAME")]
    InvalidName,
    #[error("AMBIGUOUS_ENV_PATCH")]
    Ambiguous,
    #[error("INVALID_ENV_VALUE")]
    InvalidValue,
}

impl EnvPatch {
    /// Patches use ASCII variable names, compared without case on Windows.
    /// Inherited variables are retained by the platform without lossy conversion.
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        let mut names = std::collections::HashSet::new();
        for name in self.set.keys().chain(self.unset.iter()) {
            if name.is_empty() || !name.is_ascii() || name.bytes().any(|c| c == 0 || c == b'=') {
                return Err(EnvironmentError::InvalidName);
            }
            if !names.insert(name.to_ascii_uppercase()) {
                return Err(EnvironmentError::Ambiguous);
            }
        }
        if self.set.values().any(|v| v.contains('\0')) {
            return Err(EnvironmentError::InvalidValue);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_patch_rejects_case_conflicts_and_preserves_empty_set() {
        let mut patch = EnvPatch::default();
        patch.set.insert("HTTP_PROXY".into(), String::new());
        assert!(patch.validate().is_ok());
        patch.unset.push("http_proxy".into());
        assert_eq!(patch.validate(), Err(EnvironmentError::Ambiguous));
        patch.unset.clear();
        patch.set.insert("Http_Proxy".into(), "other".into());
        assert_eq!(patch.validate(), Err(EnvironmentError::Ambiguous));
    }

    #[test]
    fn environment_patch_rejects_embedded_nul_and_equals() {
        for key in ["", "X=Y", "X\0Y"] {
            let patch = EnvPatch {
                unset: vec![key.into()],
                ..Default::default()
            };
            assert_eq!(patch.validate(), Err(EnvironmentError::InvalidName));
        }
        let patch = EnvPatch {
            set: [("A".into(), "x\0y".into())].into(),
            ..Default::default()
        };
        assert_eq!(patch.validate(), Err(EnvironmentError::InvalidValue));
    }
}
