use super::*;

#[tokio::test]
async fn callback_notification_survives_enqueue_before_wait_and_decode_failure() {
    let state = State::new();
    state.push(hint(123, 1, 1, &image("App.exe")).unwrap());
    tokio::time::timeout(Duration::from_secs(1), state.ready.notified())
        .await
        .unwrap();
    assert_eq!(state.drain(Uuid::new_v4()).unwrap().hints.len(), 1);
    state.failed_decode();
    tokio::time::timeout(Duration::from_secs(1), state.ready.notified())
        .await
        .unwrap();
    assert!(state.drain(Uuid::new_v4()).unwrap().full_scan_required);
}

fn image(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[test]
fn hints_are_bounded_names_not_payload_or_identity() {
    let h = hint(123, 1, 456, &image(r"C:\private-path\应用.exe")).unwrap();
    assert_eq!(h.image_name, "应用.exe");
    assert_eq!(h.pid, 123);
    assert_eq!(h.creation_time, 1);
    assert_eq!(h.event_time, 456);
    for bytes in [
        vec![],
        vec![0],
        image(""),
        image("bad\0name"),
        image("bad\nname"),
        image(&"a".repeat(261)),
        vec![0; 4098],
        vec![0, 0xd8, 0, 0],
    ] {
        assert!(hint(123, 1, 456, &bytes).is_err());
    }
    assert!(hint(0, 1, 456, &image("app.exe")).is_err());
    assert!(hint(123, 1, 0, &image("app.exe")).is_err());
    assert!(hint(123, 0, 456, &image("app.exe")).is_err());
}

#[test]
fn bounded_queue_deduplicates_and_reports_overflow_decode_loss_and_end() {
    let state = State::new();
    let epoch = Uuid::new_v4();
    assert!(state.drain(epoch).unwrap().full_scan_required);
    for pid in 1..=QUEUE_LIMIT as u32 {
        state.push(hint(pid, 1, 1, &image("App.exe")).unwrap());
    }
    state.push(hint(1, 1, 2, &image("app.exe")).unwrap());
    state.push(hint(5000, 1, 2, &image("app.exe")).unwrap());
    state.failed_decode();
    state.lost(2, 1);
    let batch = state.drain(epoch).unwrap();
    assert_eq!(batch.sequence, 2);
    assert_eq!(batch.epoch, epoch);
    assert_eq!(batch.hints.len(), BATCH_LIMIT);
    assert_eq!(batch.hints[0].event_time, 2);
    assert_eq!(batch.dropped, 1);
    assert_eq!(batch.decode_failures, 1);
    assert_eq!(batch.etw_events_lost, 2);
    assert_eq!(batch.etw_buffers_lost, 1);
    assert!(batch.full_scan_required && batch.ended.is_none());
    let mut total = batch.hints.len();
    while total < QUEUE_LIMIT {
        let batch = state.drain(epoch).unwrap();
        assert!(batch.hints.len() <= BATCH_LIMIT);
        total += batch.hints.len();
    }
    assert_eq!(total, QUEUE_LIMIT);
    assert!(!state.drain(epoch).unwrap().full_scan_required);
    state.lost(0, 0); // Rollback/wrap is also uncertain.
    assert!(state.drain(epoch).unwrap().full_scan_required);
    state.ended.store(ERROR_CANCELLED, Ordering::Release);
    let end = state.drain(epoch).unwrap();
    assert!(end.full_scan_required);
    assert_eq!(end.ended, Some(ERROR_CANCELLED));
}

#[test]
fn ended_listener_delivers_every_queued_hint_in_bounded_batches_before_end() {
    let state = State::new();
    let epoch = Uuid::new_v4();
    for pid in 1..=QUEUE_LIMIT as u32 {
        state.push(hint(pid, 1, 1, &image("😀.exe")).unwrap());
    }
    state.ended.store(ERROR_CANCELLED, Ordering::Release);
    let mut pids = Vec::new();
    for index in 0..QUEUE_LIMIT / BATCH_LIMIT {
        let batch = state.drain(epoch).unwrap();
        assert_eq!(batch.sequence, index as u64 + 1);
        assert_eq!(batch.hints.len(), BATCH_LIMIT);
        assert!(batch.full_scan_required);
        assert_eq!(
            batch.ended,
            (index == QUEUE_LIMIT / BATCH_LIMIT - 1).then_some(ERROR_CANCELLED)
        );
        pids.extend(batch.hints.into_iter().map(|hint| hint.pid));
    }
    assert_eq!(pids, (1..=QUEUE_LIMIT as u32).collect::<Vec<_>>());
}

#[test]
fn native_tdh_decodes_named_properties_instead_of_header_pid_or_fixed_offsets() {
    // Installed Microsoft-Windows-Kernel-Process version-0 manifest layout.
    // Header ProcessId deliberately differs from payload ProcessID.
    let mut bytes = Vec::new();
    bytes.extend(123_u32.to_le_bytes());
    bytes.extend(456_u64.to_le_bytes());
    bytes.extend(789_u32.to_le_bytes());
    bytes.extend(1_u32.to_le_bytes());
    bytes.extend(image(r"C:\fixture\child.exe"));
    let mut record = EVENT_RECORD::default();
    record.EventHeader.ProviderId = PROVIDER;
    record.EventHeader.EventDescriptor.Id = 1;
    record.EventHeader.EventDescriptor.Version = 0;
    record.EventHeader.EventDescriptor.Opcode = 1;
    record.EventHeader.ProcessId = 987;
    record.EventHeader.TimeStamp = 1000;
    record.UserData = bytes.as_mut_ptr().cast();
    record.UserDataLength = bytes.len() as u16;
    let decoded = decode(&record).unwrap();
    assert_eq!(decoded.pid, 123);
    assert_eq!(decoded.image_name, "child.exe");
    let state = Arc::new(State::new());
    record.UserContext = Arc::as_ptr(&state).cast_mut().cast();
    // SAFETY: owned synthetic buffers match the real callback contract.
    unsafe { event_callback(&mut record) };
    assert_eq!(state.drain(Uuid::new_v4()).unwrap().hints.len(), 1);
    record.UserDataLength = 2;
    // SAFETY: backing storage is still valid; only advertised data length shrinks.
    unsafe { event_callback(&mut record) };
    let bad = state.drain(Uuid::new_v4()).unwrap();
    assert_eq!(bad.decode_failures, 1);
    assert!(bad.full_scan_required);
}

#[test]
#[ignore = "requires an explicitly authorized ETW-capable token; creates only its own temporary trace"]
fn native_process_start_session_observes_fixture_and_stops_without_adopting_collision() {
    let store = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let mut listener = ProcessListener::start(store, epoch).unwrap();
    assert!(matches!(
        ProcessListener::start(store, Uuid::new_v4()),
        Err(Error::Invalid("ETW_SESSION_CONFLICT"))
    ));
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "guard::etw::tests::event_fixture_child",
        ])
        .spawn()
        .unwrap();
    let pid = child.id();
    let deadline = Instant::now() + Duration::from_secs(10);
    let observed = loop {
        let batch = listener.drain().unwrap();
        if batch.hints.iter().any(|h| h.pid == pid) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(30));
    };
    let _ = child.kill();
    let _ = child.wait();
    listener.stop().unwrap();
    assert!(observed, "fixture start event not observed");
    let mut next = ProcessListener::start(store, Uuid::new_v4()).unwrap();
    next.stop().unwrap();
}

#[test]
fn listener_drain_delivers_final_batch_on_missing_session_but_rejects_foreign_owner() {
    let state = Arc::new(State::new());
    state.ended.store(ERROR_CANCELLED, Ordering::Release);
    let listener = ProcessListener {
        session: Session {
            handle: CONTROLTRACE_HANDLE::default(),
            guid: GUID::default(),
            name: vec![0],
            stopped: true,
        },
        worker: None,
        state,
        epoch: Uuid::new_v4(),
    };
    let last = listener
        .drain_after_query(Err(Error::Windows {
            operation: "QueryOwnedTrace",
            code: ERROR_WMI_INSTANCE_NOT_FOUND,
        }))
        .unwrap();
    assert!(last.full_scan_required);
    assert_eq!(last.ended, Some(ERROR_CANCELLED));
    assert!(
        listener
            .drain_after_query(Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH")))
            .is_err()
    );
}

#[test]
#[ignore = "requires permission to control an empty temporary ETW session; no kernel provider enabled"]
fn native_session_conflict_owner_check_and_external_stop_final_batch() {
    let store = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let session = Session::start(store, epoch).unwrap();
    session.query().unwrap();
    assert!(matches!(
        Session::start(store, Uuid::new_v4()),
        Err(Error::Invalid("ETW_SESSION_CONFLICT"))
    ));
    let mut other = Session {
        handle: session.handle,
        guid: GUID::from_u128(Uuid::new_v4().as_u128()),
        name: session.name.clone(),
        stopped: false,
    };
    assert!(matches!(
        other.stop(),
        Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"))
    ));
    other.stopped = true;
    assert!(matches!(
        other.flush(),
        Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"))
    ));
    session.flush().unwrap();
    session.query().unwrap();
    let mut controller = Session {
        handle: session.handle,
        guid: session.guid,
        name: session.name.clone(),
        stopped: false,
    };
    let listener = ProcessListener {
        session,
        worker: None,
        state: Arc::new(State::new()),
        epoch,
    };
    controller.stop().unwrap();
    let last = listener.drain().unwrap();
    assert!(last.full_scan_required);
    assert_eq!(last.ended, Some(ERROR_WMI_INSTANCE_NOT_FOUND));
    drop(listener);
    let mut again = Session::start(store, Uuid::new_v4()).unwrap();
    again.stop().unwrap();
}

#[test]
#[ignore = "ETW test child only"]
fn event_fixture_child() {
    thread::sleep(Duration::from_secs(15));
}

#[test]
#[ignore = "creates only its own empty temporary ETW session to validate crash recovery"]
fn native_recovery_uses_persisted_epoch_and_query_returned_handle() {
    let store = Uuid::new_v4();
    let epoch = Uuid::new_v4();
    let mut original = Session::start(store, epoch).unwrap();
    assert!(matches!(
        recover_owned(store, Uuid::new_v4()),
        Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"))
    ));
    original.query().unwrap();
    // Models a controller crash after StartTrace but before saving its handle:
    // recovery knows only the previously persisted epoch, SID/store/session.
    recover_owned(store, epoch).unwrap();
    assert!(matches!(
        original.query(),
        Err(Error::Windows {
            code: ERROR_WMI_INSTANCE_NOT_FOUND,
            ..
        })
    ));
    original.stopped = true;
    recover_owned(store, epoch).unwrap();
    let mut successor = Session::start(store, Uuid::new_v4()).unwrap();
    assert!(matches!(
        recover_owned(store, epoch),
        Err(Error::Invalid("ETW_SESSION_OWNER_MISMATCH"))
    ));
    successor.query().unwrap();
    successor.stop().unwrap();
}
