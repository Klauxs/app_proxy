#![cfg(windows)]
use app_proxy_windows::{identity, ipc};
use std::time::Duration;
use uuid::Uuid;

fn policy() -> ipc::PeerPolicy {
    ipc::PeerPolicy::current(vec![identity::current().unwrap().image_file]).unwrap()
}

#[tokio::test]
async fn real_named_pipe_authenticates_both_peers_and_roundtrips() {
    let id = Uuid::new_v4();
    let mut listener = ipc::Listener::bind(id, policy()).unwrap();
    assert!(ipc::Listener::bind(id, policy()).is_err());
    let server = tokio::spawn(async move {
        let mut connection = listener.accept().await.unwrap();
        assert_eq!(connection.peer.pid, std::process::id());
        let value: Vec<String> = connection.receive().await.unwrap();
        assert_eq!(value, ["中文", "spaces and quotes \""]);
        connection
            .send(&serde_json::json!({"ok":true}))
            .await
            .unwrap();
    });
    let mut client = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(client.peer.pid, std::process::id());
    client
        .send(&["中文", "spaces and quotes \""])
        .await
        .unwrap();
    assert_eq!(
        client.receive::<serde_json::Value>().await.unwrap()["ok"],
        true
    );
    server.await.unwrap();
}

#[tokio::test]
async fn wrong_client_image_is_rejected_and_listener_keeps_accepting() {
    let id = Uuid::new_v4();
    let mut wrong = policy();
    wrong.allowed_images[0].file_index ^= 1;
    let mut listener = ipc::Listener::bind(id, wrong).unwrap();
    let client = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    let error = listener.accept().await.err().unwrap();
    assert_eq!(error.to_string(), "IPC_PEER_IMAGE_MISMATCH");
    drop(client);
    let _client = ipc::connect(id, &policy(), Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        listener.accept().await.err().unwrap().to_string(),
        "IPC_PEER_IMAGE_MISMATCH"
    );
}

#[tokio::test]
async fn client_rejects_wrong_server_before_sending_payload() {
    let id = Uuid::new_v4();
    let _listener = ipc::Listener::bind(id, policy()).unwrap();
    let mut wrong = policy();
    wrong.allowed_images[0].file_index ^= 1;
    assert_eq!(
        ipc::connect(id, &wrong, Duration::from_secs(1))
            .await
            .err()
            .unwrap()
            .to_string(),
        "IPC_PEER_IMAGE_MISMATCH"
    );
}

#[tokio::test]
async fn session_conflict_and_missing_server_have_explicit_errors() {
    let id = Uuid::new_v4();
    let _listener = ipc::Listener::bind(id, policy()).unwrap();
    let mut other_session = policy();
    other_session.session_id = other_session.session_id.wrapping_add(1);
    assert_eq!(
        ipc::connect(id, &other_session, Duration::from_secs(1))
            .await
            .err()
            .unwrap()
            .to_string(),
        "STORE_SESSION_CONFLICT"
    );
    assert_eq!(
        ipc::connect(Uuid::new_v4(), &policy(), Duration::from_millis(30))
            .await
            .err()
            .unwrap()
            .to_string(),
        "IPC_CONNECT_TIMEOUT"
    );
}
