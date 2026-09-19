use crate::{ProcessIdentity, model::ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreAction {
    Start { profiles: Vec<Uuid>, required: Uuid },
    Stop {},
    Install {},
    CancelInstall { request_id: Uuid },
}
impl CoreAction {
    pub fn normalize(&mut self) -> Result<(), ValidationError> {
        if let Self::CancelInstall { request_id } = self
            && request_id.is_nil()
        {
            return Err(ValidationError("INVALID_REQUEST_ID"));
        }
        if let Self::Start { profiles, required } = self {
            if profiles.is_empty()
                || profiles.len() > 1024
                || required.is_nil()
                || profiles.iter().any(Uuid::is_nil)
                || !profiles.contains(required)
            {
                return Err(ValidationError("INVALID_CORE_REQUEST"));
            }
            profiles.sort();
            profiles.dedup();
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreOutcome {
    Ready {
        generation: Uuid,
        process: ProcessIdentity,
    },
    Stopped {},
    Installed {
        version: String,
    },
    CancelRequested {
        request_id: Uuid,
    },
    Cancelled {},
    Failed {
        code: String,
    },
    Indeterminate {
        code: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreRequestStatus {
    Pending {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        progress: Option<InstallProgress>,
    },
    Indeterminate {},
    Complete {
        outcome: CoreOutcome,
        completed_at: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallProgress {
    pub phase: InstallPhase,
    pub downloaded: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallPhase {
    CheckingExisting,
    Downloading,
    Verifying,
    CheckingBinary,
    Publishing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_is_idempotent_and_requires_the_probed_profile() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut action = CoreAction::Start {
            profiles: vec![second, first, second],
            required: first,
        };
        action.normalize().unwrap();
        let once = serde_json::to_vec(&action).unwrap();
        action.normalize().unwrap();
        assert_eq!(once, serde_json::to_vec(&action).unwrap());
        let mut missing = CoreAction::Start {
            profiles: vec![first],
            required: second,
        };
        assert!(missing.normalize().is_err());
    }
}
