use super::*;

fn batch(epoch: Uuid, sequence: u64) -> EventBatch {
    EventBatch {
        epoch,
        sequence,
        hints: vec![],
        full_scan_required: false,
        dropped: 0,
        decode_failures: 0,
        etw_events_lost: 0,
        etw_buffers_lost: 0,
        ended: None,
    }
}

fn ordinary_policy() -> Policy {
    let mut policy = Policy::current(vec![identity::current().unwrap().image_file], false).unwrap();
    // The transport fixtures are deliberately ordinary at both ends. Production
    // has no entry point for this policy and still requires an elevated sender.
    policy.elevated_peer = false;
    policy
}

async fn pair() -> (EventSender, EventReceiver) {
    let store_id = Uuid::new_v4();
    let policy = ordinary_policy();
    let name = address(store_id, &policy).unwrap();
    let stream = server(&name, &policy.logon_sid, true).unwrap();
    let receiver =
        EventReceiver::connect_policy(store_id, ordinary_policy(), Duration::from_secs(1))
            .await
            .unwrap();
    stream.connect().await.unwrap();
    let (peer, process) = authenticate(stream.as_raw_handle(), &policy, true).unwrap();
    (
        EventSender {
            stream,
            store_id,
            peer,
            _process: process,
            cursor: Cursor::default(),
            poisoned: false,
        },
        receiver,
    )
}

#[test]
fn stream_requires_rescan_on_gaps_reconnect_loss_and_end_and_rejects_replay() {
    let epoch = Uuid::new_v4();
    let mut cursor = Cursor::default();
    let mut first = batch(epoch, 12);
    cursor.observe(&mut first).unwrap();
    assert!(first.full_scan_required);
    let mut next = batch(epoch, 13);
    cursor.observe(&mut next).unwrap();
    assert!(!next.full_scan_required);
    assert!(cursor.observe(&mut batch(epoch, 13)).is_err());
    assert!(cursor.observe(&mut batch(Uuid::new_v4(), 14)).is_err());
    let mut gap = batch(epoch, 15);
    cursor.observe(&mut gap).unwrap();
    assert!(gap.full_scan_required);
    let mut lost = batch(epoch, 16);
    lost.decode_failures = 1;
    cursor.observe(&mut lost).unwrap();
    assert!(lost.full_scan_required);
    let mut rollback = batch(epoch, 17);
    cursor.observe(&mut rollback).unwrap();
    assert!(rollback.full_scan_required);
    let mut end = batch(epoch, 18);
    end.ended = Some(5);
    cursor.observe(&mut end).unwrap();
    assert!(end.full_scan_required);
    assert!(cursor.observe(&mut batch(epoch, 19)).is_err());
    let mut reconnect = batch(epoch, 19);
    Cursor::default().observe(&mut reconnect).unwrap();
    assert!(reconnect.full_scan_required);
}

#[test]
fn event_schema_rejects_commands_and_max_unicode_batch_fits_frame_budget() {
    let store_id = Uuid::new_v4();
    let mut valid = batch(Uuid::new_v4(), u64::MAX);
    valid.dropped = u64::MAX;
    valid.decode_failures = u64::MAX;
    valid.etw_events_lost = u32::MAX;
    valid.etw_buffers_lost = u32::MAX;
    valid.hints = (1..=crate::etw::BATCH_LIMIT)
        .map(|pid| crate::etw::ProcessStartHint {
            pid: pid as u32,
            image_name: "😀".repeat(260),
            creation_time: 1,
            event_time: i64::MAX,
        })
        .collect();
    Cursor::default().observe(&mut valid).unwrap();
    let mut json = serde_json::to_value(Frame {
        version: VERSION,
        store_id,
        batch: valid,
    })
    .unwrap();
    assert!(serde_json::to_vec(&json).unwrap().len() < ipc::FRAME_LIMIT);
    json["command"] = "stop".into();
    assert!(serde_json::from_value::<Frame>(json.clone()).is_err());
    json.as_object_mut().unwrap().remove("command");
    json["batch"]["hints"][0]["pid"] = 0.into();
    let mut bad: Frame = serde_json::from_value(json.clone()).unwrap();
    assert!(Cursor::default().observe(&mut bad.batch).is_err());
    json["batch"]["hints"][0]["pid"] = 1.into();
    json["batch"]["hints"][0]["image_name"] = "C:\\private\\app.exe".into();
    let mut bad: Frame = serde_json::from_value(json).unwrap();
    assert!(Cursor::default().observe(&mut bad.batch).is_err());
}

#[tokio::test]
async fn native_pipe_is_read_only_and_normal_tokens_cannot_create_privileged_listener() {
    let policy = ordinary_policy();
    assert!(matches!(
        EventListener::bind(Uuid::new_v4(), policy.allowed_images.clone()),
        Err(Error::Invalid("EVENT_ELEVATION_MISMATCH"))
    ));
    let store = Uuid::new_v4();
    let name = address(store, &policy).unwrap();
    let stream = server(&name, &policy.logon_sid, true).unwrap();
    assert!(server(&name, &policy.logon_sid, false).is_err());
    assert!(server(&name, &policy.logon_sid, true).is_err());
    assert!(ClientOptions::new().open(&name).is_err()); // Requests write access.
    // Public receiver refuses our non-elevated fixture before consuming any data.
    assert!(matches!(
        EventReceiver::connect(store, policy.allowed_images, Duration::from_secs(1)).await,
        Err(Error::Invalid("EVENT_ELEVATION_MISMATCH"))
    ));
    drop(stream);
    assert!(matches!(
        EventReceiver::connect(
            Uuid::new_v4(),
            ordinary_policy().allowed_images,
            Duration::from_millis(20)
        )
        .await,
        Err(Error::Invalid(
            app_proxy_core::error_code::IPC_CONNECT_TIMEOUT
        ))
    ));
}

#[tokio::test]
async fn native_pipe_authentication_checks_image_session_user_and_logon() {
    for which in 0..4 {
        let store = Uuid::new_v4();
        let mut policy = ordinary_policy();
        let name = address(store, &policy).unwrap();
        let server = server(&name, &policy.logon_sid, true).unwrap();
        let _client = ClientOptions::new().write(false).open(&name).unwrap();
        server.connect().await.unwrap();
        let expected = match which {
            0 => {
                policy.allowed_images[0].file_index ^= 1;
                "IPC_PEER_IMAGE_MISMATCH"
            }
            1 => {
                policy.session_id = policy.session_id.wrapping_add(1);
                "STORE_SESSION_CONFLICT"
            }
            2 => {
                policy.owner_sid.push_str("-1");
                "IPC_PEER_USER_MISMATCH"
            }
            _ => {
                policy.logon_sid.push_str("-1");
                "EVENT_LOGON_MISMATCH"
            }
        };
        assert_eq!(
            authenticate(server.as_raw_handle(), &policy, true)
                .err()
                .unwrap()
                .to_string(),
            expected
        );
    }
}

#[tokio::test]
async fn native_event_roundtrip_rejects_wrong_store_and_cancellation_seals_receiver() {
    let (mut sender, mut receiver) = pair().await;
    assert_eq!(sender.peer, receiver.peer);
    let epoch = Uuid::new_v4();
    sender.send(batch(epoch, 1)).await.unwrap();
    assert!(receiver.receive().await.unwrap().full_scan_required);
    sender.send(batch(epoch, 2)).await.unwrap();
    assert!(!receiver.receive().await.unwrap().full_scan_required);
    let mut invalid = Frame {
        version: VERSION,
        store_id: Uuid::new_v4(),
        batch: batch(epoch, 3),
    };
    ipc::send_frame(&mut sender.stream, &invalid, ipc::FRAME_TIMEOUT)
        .await
        .unwrap();
    assert!(matches!(
        receiver.receive().await,
        Err(Error::Invalid("EVENT_PROTOCOL_MISMATCH"))
    ));
    assert!(matches!(
        receiver.receive().await,
        Err(Error::Invalid("IPC_CONNECTION_CLOSED"))
    ));
    invalid.store_id = sender.store_id;
    invalid.version += 1;
    let (mut sender, mut receiver) = pair().await;
    invalid.store_id = sender.store_id;
    ipc::send_frame(&mut sender.stream, &invalid, ipc::FRAME_TIMEOUT)
        .await
        .unwrap();
    assert!(matches!(
        receiver.receive().await,
        Err(Error::Invalid("EVENT_PROTOCOL_MISMATCH"))
    ));
    let (_sender, mut receiver) = pair().await;
    assert!(
        tokio::time::timeout(Duration::from_millis(20), receiver.receive())
            .await
            .is_err()
    );
    assert!(matches!(
        receiver.receive().await,
        Err(Error::Invalid("IPC_CONNECTION_CLOSED"))
    ));
}
