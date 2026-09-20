//! Coalesced, fair scans of registered instances. Event payloads never reach
//! correction admission: the launch engine independently observes each target.
use super::*;
use crate::launch_engine::{GuardObservation, GuardScan};
use app_proxy_core::launch::{LaunchAttempt, LaunchOrigin, LaunchPhase, LaunchRequest};
use std::collections::{HashSet, VecDeque};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ScanPhase {
    Ready,
    Checking,
    Blocked,
}
#[derive(Clone)]
pub(super) struct Record {
    pub revision: u64,
    pub phase: ScanPhase,
    pub diagnostic: Option<String>,
    checked: Instant,
}
impl Record {
    pub(super) fn new(revision: u64, phase: ScanPhase, diagnostic: Option<String>) -> Self {
        Self {
            revision,
            phase,
            diagnostic,
            checked: Instant::now(),
        }
    }
}

#[derive(Default)]
struct Schedule {
    queue: VecDeque<Uuid>,
    queued: HashSet<Uuid>,
    delays: HashMap<Uuid, (u8, Instant)>,
}
impl Schedule {
    fn enqueue(&mut self, id: Uuid) {
        if self.queued.insert(id) {
            self.queue.push_back(id);
        }
    }
    fn take(&mut self, now: Instant) -> Option<Uuid> {
        for _ in 0..self.queue.len() {
            let id = self.queue.pop_front()?;
            if self.delays.get(&id).is_none_or(|(_, next)| *next <= now) {
                self.queued.remove(&id);
                return Some(id);
            }
            self.queue.push_back(id);
        }
        None
    }
    fn finished(&mut self, id: Uuid, phase: ScanPhase, now: Instant) {
        let failures = self.delays.get(&id).map_or(0, |(count, _)| *count);
        let failures = if phase == ScanPhase::Blocked {
            failures.saturating_add(1).min(3)
        } else {
            0
        };
        let delay = match (phase, failures) {
            (ScanPhase::Blocked, 1 | 2) => Duration::from_millis(500),
            (ScanPhase::Blocked, _) => FULL_SCAN,
            (ScanPhase::Checking, _) => POLL,
            _ => Duration::from_millis(50),
        };
        self.delays.insert(id, (failures, now + delay));
        if phase != ScanPhase::Ready {
            self.enqueue(id);
        }
    }
}

impl Monitor {
    pub(super) async fn scan_instances(self: Arc<Self>, owner: Uuid) {
        let mut schedule = Schedule::default();
        let mut revision = None;
        let mut authorization: Option<Arc<Authorization>> = None;
        let mut pending: Option<Task<Result<GuardScan>>> = None;
        let mut scanning = None;
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            let mut rescan = false;
            let mut completed = None;
            tokio::select! {
                _ = self.scan_requested.notified() => rescan = true,
                result = joined(&mut pending), if pending.is_some() => {
                    pending = None;
                    completed = scanning.take().map(|id| (id, result));
                }
                _ = tick.tick() => {}
            }
            let current = {
                let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if state.owner != Some(owner) {
                    return;
                }
                state.authorization.clone()
            };
            if current.is_none() {
                pending = None;
                scanning = None;
                if authorization.take().is_some() {
                    revision = None;
                    schedule = Schedule::default();
                    self.clear_scans(owner);
                }
                continue;
            }
            let manifest = self.configuration.snapshot();
            let Ok(manifest) = manifest else {
                pending = None;
                scanning = None;
                revision = None;
                schedule = Schedule::default();
                self.clear_scans(owner);
                continue;
            };
            let same =
                matches!((&current, &authorization), (Some(a), Some(b)) if Arc::ptr_eq(a, b));
            if revision != Some(manifest.revision) || !same {
                pending = None;
                scanning = None;
                completed = None;
                schedule = Schedule::default();
                self.clear_scans(owner);
                revision = Some(manifest.revision);
                authorization = current;
                rescan = true;
            }
            let Some(authorization) = &authorization else {
                continue;
            };
            if rescan {
                app_proxy_windows::diagnostic_timing::mark("scan.requested", || owner.to_string());
                for instance in &manifest.instances {
                    if instance.guard.desired == Desired::Enabled {
                        schedule.enqueue(instance.id);
                    }
                }
            }
            if let Some((id, result)) = completed {
                // The same lock serializes listener revocation and admission.
                // Once accepted, the durable launch checks configuration again.
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if state.owner != Some(owner)
                    || !state
                        .authorization
                        .as_ref()
                        .is_some_and(|a| Arc::ptr_eq(a, authorization))
                {
                    continue;
                }
                let record = match result {
                    Ok(Ok(scan))
                        if scan.revision == manifest.revision && scan.instance_id == id =>
                    {
                        self.apply_scan(scan).unwrap_or_else(|error| {
                            Record::new(
                                manifest.revision,
                                ScanPhase::Blocked,
                                Some(error.to_string()),
                            )
                        })
                    }
                    _ => Record::new(
                        manifest.revision,
                        ScanPhase::Blocked,
                        Some("GUARD_SCAN_UNCONFIRMED".into()),
                    ),
                };
                schedule.finished(id, record.phase, Instant::now());
                state.scans.insert(id, record);
            }
            if pending.is_none()
                && let Some(id) = schedule.take(Instant::now())
            {
                let launch = self.launch.clone();
                scanning = Some(id);
                app_proxy_windows::diagnostic_timing::mark("scan.scheduled", || id.to_string());
                pending = Some(Task(tokio::spawn(
                    async move { launch.observe_guard(id).await },
                )));
            }
        }
    }

    fn clear_scans(&self, owner: Uuid) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.owner == Some(owner) {
            state.scans.clear();
        }
    }

    // Caller holds the current listener authorization gate. Kept synchronous so
    // admission has no cancellation gap after that check.
    pub(super) fn apply_scan(&self, scan: GuardScan) -> Result<Record> {
        if self.configuration.snapshot()?.revision != scan.revision {
            return Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"));
        }
        let (phase, diagnostic) = match scan.observation {
            GuardObservation::Correction { target } => {
                let attempt = self.launch.submit_guard(
                    LaunchRequest {
                        request_id: Uuid::new_v4(),
                        instance_id: scan.instance_id,
                        origin: LaunchOrigin::Guard,
                    },
                    scan.revision,
                    target,
                )?;
                (
                    ScanPhase::Checking,
                    Some(format!("GUARD_CORRECTION_PENDING: {}", attempt.id)),
                )
            }
            GuardObservation::Pending { attempt_id } => {
                let attempt = self
                    .launch
                    .status(attempt_id)?
                    .ok_or(Error::Invalid("LAUNCH_ATTEMPT_MISSING"))?;
                match attempt.phase {
                    LaunchPhase::Failed { code } => (ScanPhase::Blocked, Some(code)),
                    LaunchPhase::Indeterminate {} => (
                        ScanPhase::Blocked,
                        Some("GUARD_LAUNCH_INDETERMINATE".into()),
                    ),
                    _ => (
                        ScanPhase::Checking,
                        Some(format!("GUARD_LAUNCH_PENDING: {attempt_id}")),
                    ),
                }
            }
            GuardObservation::Blocked { code } => (ScanPhase::Blocked, Some(code)),
            GuardObservation::Disabled {} => return Err(Error::Invalid("GUARD_CONFIG_DISABLED")),
            GuardObservation::Absent {} => {
                // Preserve dispatched and non-transient failures across scans
                // and restarts; only pre-stop process races can clear here.
                let attempts = self.configuration.lock()?.launch_attempts()?;
                match recent_failure(&attempts, scan.instance_id) {
                    Some(code) => (ScanPhase::Blocked, Some(code)),
                    None => (ScanPhase::Ready, None),
                }
            }
            GuardObservation::Session { .. } | GuardObservation::Compliant { .. } => {
                (ScanPhase::Ready, None)
            }
        };
        Ok(Record::new(scan.revision, phase, diagnostic))
    }

    pub fn update_status(&self, status: &mut crate::guard_control::GuardStatus) {
        if status.desired != Desired::Enabled
            || status.diagnostic.as_deref() != Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED")
        {
            return;
        }
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let listener = state.snapshot.fresh();
        if state.authorization.is_none() || state.owner.is_none() {
            status.diagnostic = listener
                .diagnostic
                .or_else(|| Some("GUARD_LISTENER_START_PENDING".into()));
            return;
        }
        overlay(status, listener, state.scans.get(&status.instance_id));
    }
}

fn recent_failure(attempts: &[LaunchAttempt], instance: Uuid) -> Option<String> {
    let latest = attempts
        .iter()
        .filter(|a| a.instance_id == instance)
        .map(|a| a.accepted_at)
        .max()?;
    // Journal timestamps have one-second precision. A tied successful request
    // cannot prove it followed (and resolved) a failed correction.
    attempts
        .iter()
        .filter(|a| {
            a.instance_id == instance && a.accepted_at == latest && a.guard_correction.is_some()
        })
        .find_map(|a| match &a.phase {
            LaunchPhase::Failed { code } => {
                let correction = a.guard_correction.as_ref().unwrap();
                // Called only after a fresh Absent observation. A pre-stop
                // process race no longer describes current readiness; retain
                // its journal entry, and keep all dispatched/unknown failures.
                let transient = matches!(
                    code.as_str(),
                    "PROCESS_EXITED_DURING_INSPECTION"
                        | "PROCESS_QUERY_NOT_FOUND"
                        | "GUARD_TARGET_EXITED_BEFORE_STOP"
                        | "GUARD_CANDIDATES_CHANGED"
                );
                if transient
                    && correction.stop_started_at.is_none()
                    && correction.stop_nonce.is_none()
                    && !correction.stop_confirmed
                {
                    None
                } else {
                    Some(code.clone())
                }
            }
            LaunchPhase::Cancelled {} => Some("GUARD_CORRECTION_CANCELLED".into()),
            _ => None,
        })
}

fn overlay(
    status: &mut crate::guard_control::GuardStatus,
    listener: Snapshot,
    record: Option<&Record>,
) {
    use crate::guard_control::{ComponentState, GuardPhase};
    status.listener = if listener.phase == Phase::Etw {
        ComponentState::ActiveEtw
    } else {
        ComponentState::ActivePolling
    };
    // A newer read-only observation can revoke cached readiness; it cannot
    // establish automatic coverage before the background scan has run.
    if let Some(scan) = &status.scan {
        let diagnostic = match &scan.observation {
            GuardObservation::Blocked { code } => Some((GuardPhase::Blocked, code.clone())),
            GuardObservation::Correction { .. } => Some((
                GuardPhase::Starting,
                "GUARD_CORRECTION_CHECK_PENDING".into(),
            )),
            GuardObservation::Pending { .. } => {
                Some((GuardPhase::Starting, "GUARD_LAUNCH_CHECK_PENDING".into()))
            }
            GuardObservation::Disabled {} => {
                Some((GuardPhase::Blocked, "GUARD_CONFIG_CHANGED".into()))
            }
            _ => None,
        };
        if let Some((phase, code)) = diagnostic {
            if phase == GuardPhase::Starting
                && let Some(failed) = record
                    .filter(|r| r.revision == status.revision && r.phase == ScanPhase::Blocked)
            {
                status.phase = GuardPhase::Blocked;
                status.diagnostic = failed.diagnostic.clone();
            } else {
                status.phase = phase;
                status.diagnostic = Some(code);
            }
            return;
        }
    }
    let record = record.filter(|r| r.revision == status.revision);
    let Some(record) = record else {
        status.phase = GuardPhase::Starting;
        status.diagnostic = Some("GUARD_INITIAL_SCAN_PENDING".into());
        return;
    };
    // A stalled scan loop must not indefinitely advertise coverage.
    if record.checked.elapsed() > FULL_SCAN + Duration::from_secs(10) {
        status.phase = GuardPhase::Degraded;
        status.diagnostic = Some("GUARD_SCAN_STALE".into());
        return;
    }
    status.phase = match record.phase {
        ScanPhase::Blocked => GuardPhase::Blocked,
        ScanPhase::Checking => GuardPhase::Starting,
        ScanPhase::Ready if listener.phase == Phase::Etw => GuardPhase::Active,
        ScanPhase::Ready => GuardPhase::Degraded,
    };
    status.diagnostic = record.diagnostic.clone().or(listener.diagnostic);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch_engine::tests::Fixture;
    use app_proxy_windows::process;

    #[test]
    fn cached_ready_cannot_override_new_observations_or_missing_coverage() {
        use crate::guard_control::{ComponentState, GuardPhase, GuardStatus};
        let id = Uuid::new_v4();
        let fresh = Record::new(7, ScanPhase::Ready, None);
        let mut listener = Snapshot::new(Phase::Etw, None);
        listener.received = Some(Instant::now());
        let status = || GuardStatus {
            instance_id: id,
            revision: 7,
            desired: Desired::Enabled,
            phase: GuardPhase::Blocked,
            listener: ComponentState::Unverified,
            scan: None,
            diagnostic: Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED".into()),
        };
        let target = app_proxy_core::launch::GuardTarget {
            process: identity::current().unwrap(),
            endpoint: app_proxy_core::model::Endpoint {
                host: "127.0.0.1".parse().unwrap(),
                port: 1,
            },
        };
        for observation in [
            GuardObservation::Blocked {
                code: "INSTANCE_PROCESS_UNKNOWN".into(),
            },
            GuardObservation::Correction { target },
            GuardObservation::Pending {
                attempt_id: Uuid::new_v4(),
            },
        ] {
            let mut value = status();
            let blocked = matches!(observation, GuardObservation::Blocked { .. });
            value.scan = Some(GuardScan {
                instance_id: id,
                revision: 7,
                observation,
            });
            overlay(&mut value, listener.clone(), Some(&fresh));
            assert!(
                value.phase
                    == if blocked {
                        GuardPhase::Blocked
                    } else {
                        GuardPhase::Starting
                    }
            );
        }
        let mut value = status();
        value.scan = Some(GuardScan {
            instance_id: id,
            revision: 7,
            observation: GuardObservation::Pending {
                attempt_id: Uuid::new_v4(),
            },
        });
        let unresolved = Record::new(
            7,
            ScanPhase::Blocked,
            Some("GUARD_LAUNCH_INDETERMINATE".into()),
        );
        overlay(&mut value, listener.clone(), Some(&unresolved));
        assert!(value.phase == GuardPhase::Blocked);
        assert_eq!(
            value.diagnostic.as_deref(),
            Some("GUARD_LAUNCH_INDETERMINATE")
        );
        let mut value = status();
        overlay(&mut value, listener.clone(), Some(&fresh));
        assert!(value.phase == GuardPhase::Active);
        let mut value = status();
        overlay(&mut value, listener.clone(), None);
        assert!(value.phase == GuardPhase::Starting);
        let mut stale = fresh.clone();
        stale.checked = Instant::now() - Duration::from_secs(41);
        overlay(&mut value, listener.clone(), Some(&stale));
        assert!(value.phase == GuardPhase::Degraded);
        assert_eq!(value.diagnostic.as_deref(), Some("GUARD_SCAN_STALE"));
        listener.phase = Phase::Polling;
        listener.diagnostic = Some("GUARD_EVENT_STREAM_CLOSED".into());
        overlay(&mut value, listener, Some(&fresh));
        assert!(value.listener == ComponentState::ActivePolling);
        assert!(value.phase == GuardPhase::Degraded);
    }

    #[tokio::test]
    async fn observation_adapter_preserves_original_and_compliant_clone() {
        for (isolated, matching) in [(false, false), (true, true)] {
            let fixture = Fixture::guarded();
            let monitor = fixture.monitor();
            let id = monitor.configuration.snapshot().unwrap().instances[0].id;
            monitor
                .configuration
                .lock()
                .unwrap()
                .prepare_instance_data(id, None)
                .unwrap();
            let child = fixture.external_guard_target(isolated, matching);
            fixture.events(1).await;
            let scan = monitor.launch.observe_guard(id).await.unwrap();
            assert!(monitor.apply_scan(scan).unwrap().phase == ScanPhase::Ready);
            assert!(process::is_running_exact(&child.0.identity).unwrap());
            assert!(
                monitor
                    .configuration
                    .lock()
                    .unwrap()
                    .launch_attempts()
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[tokio::test]
    async fn observation_adapter_corrects_only_target_and_preserves_failure_after_absence() {
        let fixture = Fixture::guarded();
        let monitor = fixture.monitor();
        let id = monitor.configuration.snapshot().unwrap().instances[0].id;
        let child = fixture.external_guard_target(true, false);
        fixture.events(1).await;
        let scan = monitor.launch.observe_guard(id).await.unwrap();
        assert!(matches!(
            scan.observation,
            GuardObservation::Correction { .. }
        ));
        assert!(monitor.apply_scan(scan).unwrap().phase == ScanPhase::Checking);
        let attempts = monitor
            .configuration
            .lock()
            .unwrap()
            .launch_attempts()
            .unwrap();
        assert_eq!(attempts.len(), 1);
        let request = attempts[0].id;
        let result = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let result = monitor.launch.status(request).unwrap().unwrap();
                if result.finished_at.is_some() {
                    break result;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        assert!(
            matches!(result.phase, LaunchPhase::Failed { ref code } if code == "GUARD_STOPPED_PROXY_UNAVAILABLE")
        );
        let mut ordinary = result.clone();
        // Fresh absence clears only transient failures with no stop intent.
        for code in [
            "PROCESS_EXITED_DURING_INSPECTION",
            "PROCESS_QUERY_NOT_FOUND",
            "GUARD_TARGET_EXITED_BEFORE_STOP",
            "GUARD_CANDIDATES_CHANGED",
        ] {
            let mut transient = result.clone();
            transient.phase = LaunchPhase::Failed { code: code.into() };
            assert_eq!(
                recent_failure(&[transient.clone()], id).as_deref(),
                Some(code)
            );
            let correction = transient.guard_correction.as_mut().unwrap();
            correction.stop_started_at = None;
            correction.stop_nonce = None;
            correction.stop_confirmed = false;
            assert!(recent_failure(&[transient.clone()], id).is_none());
            transient.phase = LaunchPhase::Failed {
                code: "PROCESS_QUERY_TIMEOUT".into(),
            };
            assert_eq!(
                recent_failure(&[transient], id).as_deref(),
                Some("PROCESS_QUERY_TIMEOUT")
            );
        }
        ordinary.id = Uuid::new_v4();
        ordinary.guard_correction = None;
        ordinary.phase = LaunchPhase::Cancelled {};
        for attempts in [
            vec![result.clone(), ordinary.clone()],
            vec![ordinary.clone(), result.clone()],
        ] {
            assert_eq!(
                recent_failure(&attempts, id).as_deref(),
                Some("GUARD_STOPPED_PROXY_UNAVAILABLE")
            );
        }
        // An origin label by itself cannot manufacture correction evidence.
        ordinary.phase = LaunchPhase::Failed {
            code: "not-a-correction".into(),
        };
        assert!(recent_failure(&[ordinary.clone()], id).is_none());
        ordinary.accepted_at += 1;
        assert!(recent_failure(&[result.clone(), ordinary], id).is_none());
        assert!(!process::is_running_exact(&child.0.identity).unwrap());
        let scan = monitor.launch.observe_guard(id).await.unwrap();
        assert!(matches!(scan.observation, GuardObservation::Absent {}));
        let record = monitor.apply_scan(scan).unwrap();
        assert!(record.phase == ScanPhase::Blocked);
        assert_eq!(
            record.diagnostic.as_deref(),
            Some("GUARD_STOPPED_PROXY_UNAVAILABLE")
        );
        assert_eq!(
            monitor
                .configuration
                .lock()
                .unwrap()
                .launch_attempts()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn unlicensed_loop_and_stale_scan_never_admit_corrections() {
        let fixture = Fixture::guarded();
        let monitor = fixture.monitor();
        let id = monitor.configuration.snapshot().unwrap().instances[0].id;
        let child = fixture.external_guard_target(true, false);
        fixture.events(1).await;
        let service = monitor.start().unwrap();
        for _ in 0..2000 {
            monitor.scan_requested.notify_one();
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert!(
            monitor
                .configuration
                .lock()
                .unwrap()
                .launch_attempts()
                .unwrap()
                .is_empty()
        );
        let scan = monitor.launch.observe_guard(id).await.unwrap();
        {
            let mut store = monitor.configuration.lock().unwrap();
            let mut manifest = store.load().unwrap();
            manifest.instances[0].guard.desired = Desired::Disabled;
            store.commit(manifest.revision, manifest).unwrap();
        }
        assert!(matches!(
            monitor.apply_scan(scan),
            Err(Error::Invalid("LAUNCH_CONFIG_CHANGED"))
        ));
        assert!(process::is_running_exact(&child.0.identity).unwrap());
        assert!(
            monitor
                .configuration
                .lock()
                .unwrap()
                .launch_attempts()
                .unwrap()
                .is_empty()
        );
        drop(service);
    }

    #[test]
    fn bursts_coalesce_and_delayed_instance_does_not_starve_others() {
        let mut schedule = Schedule::default();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let now = Instant::now();
        for _ in 0..2000 {
            schedule.enqueue(a);
            schedule.enqueue(b);
        }
        assert_eq!(schedule.queue.len(), 2);
        assert_eq!(schedule.take(now), Some(a));
        schedule.finished(a, ScanPhase::Blocked, now);
        assert_eq!(schedule.take(now), Some(b));
        assert_eq!(schedule.take(now), None);
        assert_eq!(schedule.take(now + Duration::from_secs(1)), Some(a));
    }
    #[test]
    fn new_event_after_ready_scan_waits_only_the_short_coalescing_interval() {
        let mut schedule = Schedule::default();
        let id = Uuid::new_v4();
        let now = Instant::now();
        schedule.finished(id, ScanPhase::Ready, now);
        assert!(schedule.queue.is_empty());
        schedule.enqueue(id);
        assert_eq!(schedule.take(now + Duration::from_millis(49)), None);
        assert_eq!(schedule.take(now + Duration::from_millis(50)), Some(id));
        schedule.finished(id, ScanPhase::Blocked, now);
        assert_eq!(schedule.take(now + Duration::from_millis(50)), None);
        assert_eq!(schedule.take(now + Duration::from_millis(500)), Some(id));
    }

    #[test]
    fn unknown_has_three_checks_then_backs_off_despite_event_storms() {
        let mut schedule = Schedule::default();
        let id = Uuid::new_v4();
        let mut now = Instant::now();
        for _ in 0..3 {
            schedule.enqueue(id);
            assert_eq!(schedule.take(now), Some(id));
            schedule.finished(id, ScanPhase::Blocked, now);
            now += Duration::from_secs(1);
        }
        for _ in 0..2000 {
            schedule.enqueue(id);
        }
        assert_eq!(schedule.take(now), None);
        now += FULL_SCAN;
        assert_eq!(schedule.take(now), Some(id));
        schedule.finished(id, ScanPhase::Ready, now);
        assert_eq!(schedule.delays[&id].0, 0);
        assert!(schedule.queue.is_empty());
    }
}
