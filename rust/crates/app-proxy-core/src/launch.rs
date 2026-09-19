//! Durable launch evidence. Confirmed is historical; live process observation is
//! separate. No arguments, environment values or credentials belong in this DTO.
use crate::{FileIdentity, ProcessIdentity, model::Endpoint};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchOrigin {
    Interactive,
    Shortcut,
    Guard,
    Ifeo,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchRequest {
    pub request_id: Uuid,
    pub instance_id: Uuid,
    pub origin: LaunchOrigin,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchPhase {
    Accepted {},
    Resolving {},
    CheckingInstance {},
    PreparingProxy {},
    PreparingData {},
    ReadyToSpawn {},
    SpawnRequested {},
    AwaitingIdentity {},
    Indeterminate {},
    Confirmed { process: ProcessIdentity },
    Failed { code: String },
    Cancelled {},
}
impl LaunchPhase {
    pub fn before_spawn(&self) -> bool {
        matches!(
            self,
            Self::Accepted {}
                | Self::Resolving {}
                | Self::CheckingInstance {}
                | Self::PreparingProxy {}
                | Self::PreparingData {}
                | Self::ReadyToSpawn {}
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum LaunchNetwork {
    Direct {},
    Profile {
        profile_id: Uuid,
        generation: Uuid,
        endpoint: Endpoint,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchBinding {
    pub dependency_digest: [u8; 32],
    pub resource_key: [u8; 32],
    pub executable: PathBuf,
    pub image: FileIdentity,
    pub session_id: u32,
    pub network: LaunchNetwork,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LaunchAttempt {
    pub id: Uuid,
    pub instance_id: Uuid,
    pub origin: LaunchOrigin,
    pub epoch: Uuid,
    pub phase: LaunchPhase,
    pub accepted_at: u64,
    pub finished_at: Option<u64>,
    pub cancel_requested: bool,
    pub binding: Option<LaunchBinding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dispatch_id: Option<Uuid>,
    /// Only exact process observation can release a confirmed session.
    pub session_exited: bool,
    /// Preserve a terminal receipt until the cross-store claim is synchronized.
    #[serde(default)]
    pub resource_pending: bool,
    /// Optional precondition for an automatic foreground continuation. It is
    /// fixed at first admission and cannot be loosened by request replays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_revision: Option<u64>,
}
impl LaunchAttempt {
    pub fn reserves_instance(&self) -> bool {
        !matches!(
            self.phase,
            LaunchPhase::Failed { .. } | LaunchPhase::Cancelled {}
        ) && !self.session_exited
    }
}
