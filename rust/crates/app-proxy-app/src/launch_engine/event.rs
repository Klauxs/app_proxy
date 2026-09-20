//! Single-event observation; broad occupancy checks belong after exact stop.
use super::*;
use app_proxy_windows::{etw::ProcessStartHint, native_process::PinnedProcess};

/// Pins acquired for one event are carried through admission, never cached
/// across events or recreated from serialized data.
pub(crate) struct ObservedTarget {
    pub(super) pinned: PinnedProcess,
    pub(super) application: installation::ResolvedApplication,
    pub(super) data: Option<PreparedData>,
    pub(super) instance_id: Uuid,
    pub(super) revision: u64,
}
impl std::ops::Deref for ObservedTarget {
    type Target = PinnedProcess;
    fn deref(&self) -> &Self::Target {
        &self.pinned
    }
}

impl LaunchEngine {
    pub(crate) async fn observe_guard_event(
        &self,
        hint: &ProcessStartHint,
    ) -> Result<Option<(GuardScan, ObservedTarget)>> {
        let _timing = app_proxy_windows::diagnostic_timing::Span::new("event.inspect", || {
            hint.pid.to_string()
        });
        let snapshot = self.configuration.snapshot()?;
        let mut pinned = match PinnedProcess::open(hint.pid) {
            Ok(pinned) => pinned,
            // No absence/readiness claim: background scans still diagnose
            // unreadable registered applications. These events grant no action.
            Err(
                Error::IdentityMismatch
                | Error::Windows { code: 5 | 87, .. }
                | Error::Invalid("GUARD_TARGET_EXITED_BEFORE_STOP"),
            ) => return Ok(None),
            Err(error) => return Err(error),
        };
        if pinned.identity().creation_time != hint.creation_time {
            return Ok(None);
        }
        let mut selected = None;
        let mut prepared = None;
        for instance in &snapshot.instances {
            if instance.guard.desired != Desired::Enabled {
                continue;
            }
            let (application, _) = entries(&snapshot, instance.id)?;
            if !application.template_ref.supports_guard() {
                continue;
            }
            let NetworkBinding::Profile { profile_id } = instance.network else {
                continue;
            };
            let resolved = self
                .resolve_for_observation(application.locator.clone())
                .await?;
            if pinned.identity().image_file != *resolved.image() {
                continue;
            }
            if pinned.arguments().is_none() {
                pinned.read_arguments()?;
            }
            let data = self
                .configuration
                .lock()?
                .inspect_instance_data(instance.id, resolved.package())?;
            let endpoint = snapshot
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?
                .endpoint
                .clone();
            let target = InstanceTarget::new(&resolved, data.as_ref(), application.template_ref)?;
            let observed = target.inspect_pinned(
                &pinned,
                std::net::SocketAddr::new(endpoint.host, endpoint.port),
            )?;
            if observed.role != ProcessRole::Main || observed.relation != InstanceRelation::Target {
                continue;
            }
            if observed.proxy == ProxyArguments::Matching {
                continue;
            }
            if observed.proxy != ProxyArguments::Mismatched {
                return Err(Error::Invalid("GUARD_PROXY_ARGUMENTS_UNKNOWN"));
            }
            if selected.is_some() {
                return Err(Error::Invalid("GUARD_EVENT_INSTANCE_AMBIGUOUS"));
            }
            selected = Some(GuardScan {
                instance_id: instance.id,
                revision: snapshot.revision,
                observation: GuardObservation::Correction {
                    target: GuardTarget {
                        process: observed.identity,
                        endpoint,
                    },
                },
            });
            prepared = Some((resolved, data));
        }
        let Some(scan) = selected else {
            return Ok(None);
        };
        let store = self.configuration.lock()?;
        if store.load()?.revision != snapshot.revision {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        for attempt in store
            .launch_attempts()?
            .iter()
            .filter(|a| a.instance_id == scan.instance_id && a.reserves_instance())
        {
            match &attempt.phase {
                LaunchPhase::Confirmed { process } if !process::is_running_exact(process)? => {}
                _ => return Ok(None),
            }
        }
        pinned.verify()?;
        app_proxy_windows::diagnostic_timing::mark("event.correction", || {
            format!("{}:{}", scan.instance_id, hint.pid)
        });
        let (application, data) = prepared.expect("selected observation retains its pins");
        let observed = ObservedTarget {
            pinned,
            application,
            data,
            instance_id: scan.instance_id,
            revision: scan.revision,
        };
        Ok(Some((scan, observed)))
    }

    pub(crate) fn submit_guard_event(
        self: &Arc<Self>,
        request: LaunchRequest,
        revision: u64,
        target: GuardTarget,
        pinned: ObservedTarget,
    ) -> Result<LaunchAttempt> {
        if pinned.identity() != &target.process
            || pinned.instance_id != request.instance_id
            || pinned.revision != revision
        {
            return Err(Error::IdentityMismatch);
        }
        let _timing = app_proxy_windows::diagnostic_timing::Span::new("launch.admit", || {
            format!("{}:{}", request.request_id, target.process.pid)
        });
        self.submit_checked(request, Some(revision), Some(target), Some(pinned))
    }
}
