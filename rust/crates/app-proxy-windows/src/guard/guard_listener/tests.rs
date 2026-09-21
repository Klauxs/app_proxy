use super::*;
use crate::etw::ProcessStartHint;

fn batch(sequence: u64, count: usize, ended: Option<u32>) -> EventBatch {
    EventBatch {
        epoch: Uuid::new_v4(),
        sequence,
        hints: (1..=count as u32)
            .map(|pid| ProcessStartHint {
                pid,
                image_name: "fixture.exe".into(),
                creation_time: 1,
                event_time: 1,
            })
            .collect(),
        full_scan_required: true,
        dropped: 0,
        decode_failures: 0,
        etw_events_lost: 0,
        etw_buffers_lost: 0,
        ended,
    }
}

#[tokio::test]
async fn unauthenticated_attempts_share_one_deadline_and_listener_errors_are_fatal() {
    let mut attempts = 0;
    let before = tokio::time::Instant::now();
    let result = accept_until::<()>(Duration::from_millis(100), async || {
        attempts += 1;
        tokio::time::sleep(Duration::from_millis(35)).await;
        Err(AcceptError::Peer(Error::IdentityMismatch))
    })
    .await;
    assert!(matches!(
        result,
        Err(Error::Invalid("EVENT_COORDINATOR_TIMEOUT"))
    ));
    assert!(attempts >= 2);
    assert!(before.elapsed() < Duration::from_secs(1));
    let mut attempts = 0;
    assert_eq!(
        accept_until(Duration::from_secs(1), async || {
            attempts += 1;
            if attempts == 1 {
                Err(AcceptError::Peer(Error::IdentityMismatch))
            } else {
                Ok(12)
            }
        })
        .await
        .unwrap(),
        12
    );
    assert!(matches!(
        accept_until::<()>(Duration::from_secs(1), async || {
            Err(AcceptError::Listener(Error::Invalid("fixture bind error")))
        })
        .await,
        Err(Error::Invalid("fixture bind error"))
    ));
}

#[tokio::test]
async fn pump_delivers_backlog_and_final_failure_once_then_stops() {
    let mut incoming = std::collections::VecDeque::from([
        batch(1, crate::etw::BATCH_LIMIT, None),
        batch(2, crate::etw::BATCH_LIMIT, None),
        batch(3, 1, Some(5)),
    ]);
    let mut sent = Vec::new();
    let result = pump(
        |_| Ok(incoming.pop_front().expect("no drain after end")),
        &tokio::sync::Notify::new(),
        async |batch| {
            sent.push((batch.sequence, batch.hints.len(), batch.ended));
            Ok(())
        },
    )
    .await;
    assert!(matches!(
        result,
        Err(Error::Windows {
            operation: "ProcessTraceEnded",
            code: 5
        })
    ));
    assert_eq!(sent, vec![(1, 128, None), (2, 128, None), (3, 1, Some(5))]);
    assert!(incoming.is_empty());
}

#[tokio::test]
async fn pump_stops_on_disconnect_or_query_failure_and_emits_idle_heartbeats() {
    let mut drains = 0;
    assert!(
        pump(
            |_| {
                drains += 1;
                Ok(batch(1, 0, None))
            },
            &tokio::sync::Notify::new(),
            async |_| { Err(Error::Invalid("fixture disconnected")) }
        )
        .await
        .is_err()
    );
    assert_eq!(drains, 1);
    assert!(
        pump(
            |_| Err(Error::Invalid("fixture query failure")),
            &tokio::sync::Notify::new(),
            async |_| { panic!("failed query must not send a healthy heartbeat") }
        )
        .await
        .is_err()
    );
    let mut sequence = 0;
    let mut sent = Vec::new();
    let started = tokio::time::Instant::now();
    pump(
        |_| {
            sequence += 1;
            Ok(batch(sequence, 0, (sequence == 2).then_some(0)))
        },
        &tokio::sync::Notify::new(),
        async |batch| {
            sent.push((batch.sequence, tokio::time::Instant::now()));
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[1].1.duration_since(started) >= HEARTBEAT);
    assert!(sent[1].1.duration_since(sent[0].1) < Duration::from_secs(2));
}

#[tokio::test]
async fn ordinary_host_cannot_enter_privileged_listener_or_create_deployment() {
    identity::assert_ordinary_user().unwrap();
    assert!(run(Uuid::new_v4(), Uuid::new_v4()).await.is_err());
}

#[tokio::test]
async fn callback_wakes_delivery_without_flush_and_timer_cannot_be_starved() {
    let ready = tokio::sync::Notify::new();
    let mut flushes = Vec::new();
    let mut sequence = 0;
    pump(
        |flush| {
            flushes.push(flush);
            sequence += 1;
            Ok(batch(sequence, 1, (sequence == 3).then_some(0)))
        },
        &ready,
        async |batch| {
            if batch.sequence == 2 {
                // Model a busy sender while callbacks continue to arrive.
                tokio::time::sleep(HEARTBEAT * 2).await;
            }
            ready.notify_one(); // before the next wait: the permit must survive
            Ok(())
        },
    )
    .await
    .unwrap();
    assert_eq!(flushes, [true, false, true]);
}
