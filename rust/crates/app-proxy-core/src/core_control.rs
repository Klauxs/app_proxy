use crate::{ProcessIdentity, model::ValidationError};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreAction {
    Start {
        profiles: Vec<Uuid>,
        required: Uuid,
    },
    Stop {},
    Install {},
    CancelInstall {
        request_id: Uuid,
    },
    PrepareRemove {
        expected_revision: u64,
        profile_id: Uuid,
    },
    RecoverStart {
        generation: Uuid,
    },
    PrepareUpdate {
        expected_revision: u64,
        profile_id: Uuid,
        node: crate::registry::ManualProxyInput,
    },
    PrepareSubscription {
        expected_revision: u64,
        profile_id: Uuid,
        edit: crate::registry::SubscriptionEdit,
    },
    PrepareExpand {
        expected_revision: u64,
        profiles: Vec<Uuid>,
        required: Uuid,
    },
    ApplyUpdate {
        plan_id: Uuid,
    },
    RecoverUpdate {
        plan_id: Uuid,
    },
}
impl CoreAction {
    pub fn normalize(&mut self) -> Result<(), ValidationError> {
        if matches!(self, Self::PrepareRemove { expected_revision, profile_id } | Self::PrepareUpdate { expected_revision, profile_id, .. } | Self::PrepareSubscription { expected_revision, profile_id, .. } if *expected_revision == 0 || profile_id.is_nil())
            || matches!(
                self,
                Self::PrepareExpand {
                    expected_revision: 0,
                    ..
                }
            )
            || matches!(self, Self::ApplyUpdate { plan_id } | Self::RecoverUpdate { plan_id } if plan_id.is_nil())
        {
            return Err(ValidationError("INVALID_CORE_REQUEST"));
        }
        if let Self::CancelInstall { request_id } = self
            && request_id.is_nil()
        {
            return Err(ValidationError("INVALID_REQUEST_ID"));
        }
        if matches!(self, Self::RecoverStart { generation } if generation.is_nil()) {
            return Err(ValidationError("INVALID_CORE_REQUEST"));
        }
        if let Self::Start { profiles, required }
        | Self::PrepareExpand {
            profiles, required, ..
        } = self
        {
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
    Prepared {
        impact: UpdateImpact,
    },
    ProfileRemoved {
        profile_id: Uuid,
        revision: u64,
    },
    Reconciled {
        generation: Uuid,
        process: Option<ProcessIdentity>,
    },
    Reconfigured {
        generation: Uuid,
        process: ProcessIdentity,
        revision: u64,
    },
    Restored {
        core_down: bool,
    },
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
#[serde(deny_unknown_fields)]
pub struct UpdateImpact {
    pub plan_id: Uuid,
    pub manifest_revision: u64,
    pub previous_generation: Uuid,
    /// The edited, added or removed profile. Removal probes retained routes.
    pub changed_profile: Uuid,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added_profiles: Vec<Uuid>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_profiles: Vec<Uuid>,
    pub affected_profiles: Vec<Uuid>,
    /// Configured bindings, not evidence that these applications are running.
    pub bound_instances: Vec<Uuid>,
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

const INVALID_OUTCOME: ValidationError = ValidationError("INVALID_CORE_REQUEST_OUTCOME");

impl CoreOutcome {
    /// Invariants of a stored or received outcome. `owner` is the SID every
    /// reported process must belong to.
    pub fn validate(&self, owner: &str) -> Result<(), ValidationError> {
        match self {
            CoreOutcome::Prepared { impact }
                if impact.plan_id.is_nil()
                    || impact.manifest_revision == 0
                    || impact.previous_generation.is_nil()
                    || impact.changed_profile.is_nil()
                    || impact.added_profiles.iter().any(Uuid::is_nil)
                    || impact
                        .added_profiles
                        .iter()
                        .any(|p| impact.affected_profiles.contains(p))
                    || impact.removed_profiles.iter().any(|p| {
                        p.is_nil()
                            || !impact.affected_profiles.contains(p)
                            || impact.added_profiles.contains(p)
                    })
                    || impact.affected_profiles.iter().any(Uuid::is_nil)
                    || impact.bound_instances.iter().any(Uuid::is_nil)
                    || !(impact.affected_profiles.contains(&impact.changed_profile)
                        || impact.added_profiles.contains(&impact.changed_profile)) =>
            {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::ProfileRemoved {
                profile_id,
                revision,
            } if profile_id.is_nil() || *revision == 0 => {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::Reconciled {
                generation,
                process,
            } if generation.is_nil()
                || process.as_ref().is_some_and(|p| {
                    p.pid == 0
                        || p.creation_time == 0
                        || p.user_sid != owner
                        || !p.image_path.is_absolute()
                }) =>
            {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::Reconfigured { revision: 0, .. } => {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::CancelRequested { request_id } if request_id.is_nil() => {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::Installed { version } if !crate::singbox::valid_version(version) => {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::Ready {
                generation,
                process,
            }
            | CoreOutcome::Reconfigured {
                generation,
                process,
                ..
            } if generation.is_nil()
                || process.pid == 0
                || process.creation_time == 0
                || process.user_sid != owner
                || !process.image_path.is_absolute() =>
            {
                return Err(INVALID_OUTCOME);
            }
            CoreOutcome::Failed { code } | CoreOutcome::Indeterminate { code }
                if !valid_failure_code(code) =>
            {
                return Err(INVALID_OUTCOME);
            }
            _ => {}
        }
        Ok(())
    }
}

/// A failure code, optionally followed by the fixed installer diagnostics.
/// Arbitrary text is rejected so that records cannot carry paths or URLs.
fn valid_failure_code(value: &str) -> bool {
    let mut parts = value.split("; ");
    let code = parts.next().unwrap_or_default();
    if code.is_empty()
        || code.len() > 96
        || !code.bytes().all(|c| c.is_ascii_uppercase() || c == b'_')
    {
        return false;
    }
    let details: Vec<_> = parts.collect();
    if details.is_empty() {
        return true;
    }
    if details.len() != 6 || value.len() > 512 {
        return false;
    }
    let Some(phase) = details[0].strip_prefix("phase=") else {
        return false;
    };
    if !matches!(
        phase,
        "checking_existing" | "downloading" | "verifying" | "checking_binary" | "publishing"
    ) {
        return false;
    }
    for (field, prefix) in [(details[1], "operation="), (details[3], "io_kind=")] {
        let Some(name) = field.strip_prefix(prefix) else {
            return false;
        };
        if name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'(' | b')'))
        {
            return false;
        }
    }
    let Some(win32) = details[2].strip_prefix("win32=") else {
        return false;
    };
    if win32 != "none" && win32.parse::<u32>().is_err() {
        return false;
    }
    [
        (details[4], "downloaded_bytes="),
        (details[5], "elapsed_ms="),
    ]
    .iter()
    .all(|(field, prefix)| {
        field.strip_prefix(prefix).is_some_and(|v| {
            !v.is_empty() && v.bytes().all(|c| c.is_ascii_digit()) && v.parse::<u128>().is_ok()
        })
    })
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

    #[test]
    fn installation_diagnostics_remain_bounded_and_reject_arbitrary_text() {
        let valid = "STORE_ACCESS_DENIED; phase=checking_existing; operation=GetTokenInformation(elevation); win32=5; io_kind=PermissionDenied; downloaded_bytes=0; elapsed_ms=10";
        assert!(valid_failure_code(valid));
        assert!(valid_failure_code("CORE_INSTALL_IO_FAILED"));
        for invalid in [
            valid.replace("checking_existing", "unknown"),
            valid.replace("win32=5", "win32=broken"),
            valid.replace("elapsed_ms=10", "elapsed_ms=-1"),
            valid.replace(
                "GetTokenInformation(elevation)",
                "https://private?token=secret",
            ),
            format!("{valid}\nsecret"),
            format!("{valid}; extra=secret"),
        ] {
            assert!(!valid_failure_code(&invalid));
        }
    }
}
