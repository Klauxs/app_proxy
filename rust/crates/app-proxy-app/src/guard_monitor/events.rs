use super::*;
use crate::launch_engine::GuardObservation;
use app_proxy_core::launch::{LaunchOrigin, LaunchRequest};
use app_proxy_windows::etw::ProcessStartHint;

const QUEUE_LIMIT: usize = 256;

pub(super) fn enqueue(queue: &mut VecDeque<ProcessStartHint>, hints: &[ProcessStartHint]) -> bool {
    let mut overflow = false;
    for hint in hints {
        if queue
            .iter()
            .any(|queued| queued.pid == hint.pid && queued.creation_time == hint.creation_time)
        {
            continue;
        }
        if queue.len() == QUEUE_LIMIT {
            queue.pop_front();
            overflow = true;
        }
        queue.push_back(hint.clone());
    }
    overflow
}

impl Monitor {
    pub(super) async fn process_events(self: Arc<Self>, owner: Uuid) {
        loop {
            self.events_requested.notified().await;
            loop {
                let next = {
                    let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    if state.owner != Some(owner) {
                        return;
                    }
                    state.authorization.clone().and_then(|authorization| {
                        state.events.pop_front().map(|hint| (authorization, hint))
                    })
                };
                let Some((authorization, hint)) = next else {
                    break;
                };
                let result = self.launch.observe_guard_event(&hint).await;
                {
                    let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                    if state.owner != Some(owner)
                        || !state
                            .authorization
                            .as_ref()
                            .is_some_and(|active| Arc::ptr_eq(active, &authorization))
                    {
                        continue;
                    }
                }
                match result {
                    Ok(Some((scan, pinned))) => {
                        let GuardObservation::Correction { target } = scan.observation else {
                            continue;
                        };
                        let result = self
                            .launch
                            .submit_guard_event(
                                LaunchRequest {
                                    request_id: Uuid::new_v4(),
                                    instance_id: scan.instance_id,
                                    origin: LaunchOrigin::Guard,
                                },
                                scan.revision,
                                target,
                                pinned,
                            )
                            .await;
                        match result {
                            Ok(attempt) => {
                                let mut state =
                                    self.state.lock().unwrap_or_else(|p| p.into_inner());
                                if state.owner != Some(owner) {
                                    return;
                                }
                                state.scans.insert(
                                    scan.instance_id,
                                    scan::Record::new(
                                        scan.revision,
                                        scan::ScanPhase::Checking,
                                        Some(format!("GUARD_CORRECTION_PENDING: {}", attempt.id)),
                                    ),
                                );
                            }
                            Err(error) => {
                                if matches!(
                                    error,
                                    Error::Invalid(
                                        app_proxy_core::error_code::GUARD_STOPPED_RECORD_FAILED
                                    )
                                ) {
                                    let mut state =
                                        self.state.lock().unwrap_or_else(|p| p.into_inner());
                                    if state.owner == Some(owner) {
                                        state.scans.insert(
                                            scan.instance_id,
                                            scan::Record::new(
                                                scan.revision,
                                                scan::ScanPhase::Blocked,
                                                Some(error.to_string()),
                                            ),
                                        );
                                    }
                                }
                                app_proxy_windows::diagnostic_timing::mark(
                                    "event.admission_failed",
                                    || format!("{}:{error}", hint.pid),
                                );
                                self.scan_requested.notify_one();
                            }
                        }
                    }
                    Err(error) => {
                        app_proxy_windows::diagnostic_timing::mark(
                            "event.inspection_failed",
                            || format!("{}:{error}", hint.pid),
                        );
                        self.scan_requested.notify_one();
                    }
                    Ok(None) => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(pid: u32, creation_time: u64) -> ProcessStartHint {
        ProcessStartHint {
            pid,
            creation_time,
            image_name: "fixture.exe".into(),
            event_time: 1,
        }
    }

    #[test]
    fn deduplicates_exact_objects_but_retains_reused_pids() {
        let mut queue = VecDeque::new();
        assert!(!enqueue(&mut queue, &[hint(5, 1), hint(5, 1), hint(5, 2)]));
        assert_eq!(queue.len(), 2);
        assert_eq!(queue[0].creation_time, 1);
        assert_eq!(queue[1].creation_time, 2);
    }

    #[test]
    fn bounds_queue_and_requests_recovery_on_overflow() {
        let mut queue = VecDeque::new();
        let hints: Vec<_> = (1..=257).map(|pid| hint(pid, 1)).collect();
        assert!(enqueue(&mut queue, &hints));
        assert_eq!(queue.len(), QUEUE_LIMIT);
        assert_eq!(queue.front().unwrap().pid, 2);
        assert_eq!(queue.back().unwrap().pid, 257);
        assert!(!enqueue(&mut queue, &[hint(257, 1)]));
    }
}
