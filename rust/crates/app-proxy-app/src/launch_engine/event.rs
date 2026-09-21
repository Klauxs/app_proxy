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
    pub(super) stopped: Option<app_proxy_windows::process_stop::ObservedGuardStop>,
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
            // Preserve a known managed session's historical proxy binding.
            // A different pending launch never excuses this unproxied main.
            if let LaunchPhase::Confirmed { process } = &attempt.phase
                && process == pinned.identity()
            {
                return Ok(None);
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
            stopped: None,
        };
        Ok(Some((scan, observed)))
    }

    pub(crate) async fn submit_guard_event(
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
        let engine = self.clone();
        let queued = app_proxy_windows::diagnostic_timing::Span::new("event.stop_queue", || {
            target.process.pid.to_string()
        });
        tokio::task::spawn_blocking(move || {
            drop(queued);
            engine.submit_checked(request, Some(revision), Some(target), Some(pinned))
        })
        .await
        .map_err(|_| Error::Invalid("GUARD_EVENT_WORKER_INTERRUPTED"))?
    }

    /// The caller holds the admission/configuration gates across this short
    /// boundary: a completed disable/edit cannot race a stale stop decision.
    /// No history reconciliation, journal writes, resource locks or child scans.
    pub(super) fn stop_event_before_admission(
        &self,
        store: &app_proxy_windows::store::Store,
        request: &LaunchRequest,
        target: &GuardTarget,
        observed: &mut ObservedTarget,
    ) -> Result<()> {
        let timing = app_proxy_windows::diagnostic_timing::Span::new("event.stop_policy", || {
            target.process.pid.to_string()
        });
        if request.origin != LaunchOrigin::Guard || request.request_id.is_nil() {
            return Err(Error::Invalid("INVALID_GUARD_REQUEST"));
        }
        let snapshot = store.load()?;
        if snapshot.revision != observed.revision {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        let (app, instance) = entries(&snapshot, request.instance_id)?;
        if instance.guard.desired != Desired::Enabled
            || !app.template_ref.supports_guard()
            || !snapshot.profiles.iter().any(|p| {
                instance.network == NetworkBinding::Profile { profile_id: p.id }
                    && p.endpoint == target.endpoint
            })
        {
            return Err(Error::Invalid("GUARD_CONFIG_CHANGED"));
        }
        // Observation already bound the native handle and parsed arguments.
        // The native boundary only verifies that exact object is still alive.
        drop(timing);
        observed.stopped = Some(app_proxy_windows::process_stop::stop_observed_guard(
            &observed.pinned,
            target,
        )?);
        Ok(())
    }
}
