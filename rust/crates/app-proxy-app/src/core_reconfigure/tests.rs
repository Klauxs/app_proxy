use super::*;
use crate::configuration::Configuration;
use app_proxy_core::{model::*, registry::*};
use app_proxy_windows::store::Store;
use std::{fs, path::PathBuf, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

struct Upstream {
    port: u16,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Upstream {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn upstream(body: &'static str) -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            tokio::spawn(async move {
                let mut request = vec![0; 4096];
                let count = stream.read(&mut request).await.unwrap_or(0);
                if request[..count].starts_with(b"CONNECT ") {
                    if stream
                        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                        .await
                        .is_err()
                    {
                        return;
                    }
                    if stream.read(&mut request).await.unwrap_or(0) == 0 {
                        return;
                    }
                }
                let reply = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(reply.as_bytes()).await;
            });
        }
    });
    Upstream { port, task }
}
async fn via(endpoint: Endpoint) -> Result<String> {
    reqwest::Client::builder()
        .no_proxy()
        .proxy(reqwest::Proxy::http(format!("http://{}:{}", endpoint.host, endpoint.port)).unwrap())
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .unwrap()
        .get("http://fixture.invalid/test")
        .send()
        .await
        .map_err(|_| Error::Invalid("TEST_PROXY_FAILED"))?
        .error_for_status()
        .map_err(|_| Error::Invalid("TEST_PROXY_FAILED"))?
        .text()
        .await
        .map_err(|_| Error::Invalid("TEST_PROXY_FAILED"))
}
fn input(port: u16) -> ManualProxyInput {
    ManualProxyInput {
        protocol: ManualProtocol::Http,
        host: "127.0.0.1".into(),
        port,
        credentials: None,
    }
}
fn edit(configuration: &Configuration, profile_id: Uuid, port: u16) -> ConfigRequest {
    ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: configuration.snapshot().unwrap().revision,
        action: ConfigAction::UpdateManualProfile {
            profile_id,
            node: input(port),
        },
    }
}

fn admit(configuration: &Configuration, plan_id: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    configuration
        .lock()
        .unwrap()
        .begin_core_request(
            id,
            Uuid::new_v4(),
            &app_proxy_core::core_control::CoreAction::ApplyUpdate { plan_id },
        )
        .unwrap();
    id
}
struct Cleanup(Arc<Configuration>);
impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok(store) = self.0.lock()
            && let Ok(CoreState::Running { process, .. }) = store.core_state()
            && let Ok(Some(core)) = CoreProcess::recover(&process)
        {
            let _ = core.stop();
        }
    }
}

#[tokio::test]
async fn rollback_checks_later_routes_after_slow_failures_with_bounded_concurrency() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let active = AtomicUsize::new(0);
    let peak = AtomicUsize::new(0);
    let checked = AtomicUsize::new(0);
    let endpoints: Vec<_> = (1..=5)
        .map(|port| Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port,
        })
        .collect();
    let healthy = any_old_route_healthy(&endpoints, &|endpoint| {
        let active = &active;
        let peak = &peak;
        let checked = &checked;
        async move {
            peak.fetch_max(active.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
            checked.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            active.fetch_sub(1, Ordering::SeqCst);
            if endpoint.port == 5 {
                Ok(())
            } else {
                Err(Error::Invalid("TEST_TIMEOUT"))
            }
        }
    })
    .await;
    assert!(healthy);
    assert_eq!(checked.load(Ordering::SeqCst), 5);
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    assert_eq!(active.load(Ordering::SeqCst), 0);
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; real owned core in an isolated store"]
async fn real_shared_core_update_checks_confirmation_switches_rolls_back_and_recovers() {
    use std::os::windows::fs::OpenOptionsExt;
    let binary =
        PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").expect("validation binary"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let first = upstream("first").await;
    let second = upstream("second").await;
    let mut profiles = Vec::new();
    let mut endpoints = Vec::new();
    let mut reservations = Vec::new();
    for n in 0..2 {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: socket.local_addr().unwrap().port(),
        };
        reservations.push(socket);
        let id = Uuid::new_v4();
        profiles.push(id);
        endpoints.push(endpoint.clone());
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: store.load().unwrap().revision,
                action: ConfigAction::CreateManualProfile {
                    profile_id: id,
                    name: format!("proxy {n}"),
                    endpoint,
                    node: input(first.port),
                },
            })
            .unwrap();
    }
    drop(reservations);
    let configuration = Arc::new(Configuration::new(store));
    let _cleanup = Cleanup(configuration.clone());
    let manager = CoreManager::new(root.clone(), configuration.clone());
    let ready = manager
        .ensure_with(&profiles, profiles[0], |e| async {
            assert_eq!(via(e).await?, "first");
            Ok(())
        })
        .await
        .unwrap();
    let revision = configuration.snapshot().unwrap().revision;
    let request = edit(&configuration, profiles[0], second.port);
    let impact = manager.prepare_update(&request).await.unwrap();
    assert_eq!(impact.affected_profiles.len(), 2);
    assert_eq!(configuration.snapshot().unwrap().revision, revision);
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "first");
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    let changed_port = endpoints[0].port;
    let result = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |e| async move {
                let expected = if e.port == changed_port {
                    "second"
                } else {
                    "first"
                };
                assert_eq!(via(e).await?, expected);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, CoreOutcome::Reconfigured { revision: r, .. } if r == revision + 1));
    assert!(CoreProcess::recover(&ready.process).unwrap().is_none());
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "second");
    assert_eq!(via(endpoints[1].clone()).await.unwrap(), "first");

    // A failed candidate must restore both old profile routes, not just the edited one.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let bad_port = unused.local_addr().unwrap().port();
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], bad_port))
        .await
        .unwrap();
    let result = manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |e| async { via(e).await.map(|_| ()) },
        )
        .await
        .unwrap();
    assert!(matches!(result, CoreOutcome::Restored { core_down: false }));
    assert_eq!(configuration.snapshot().unwrap().revision, revision + 1);
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "second");
    assert_eq!(via(endpoints[1].clone()).await.unwrap(), "first");
    drop(unused);

    // Crash after switch intent, before stop: ordinary stop/reuse cannot cross it.
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], first.port))
        .await
        .unwrap();
    let execution = admit(&configuration, plan.plan_id);
    configuration
        .lock()
        .unwrap()
        .start_core_update(plan.plan_id, execution)
        .unwrap();
    let state = manager.state().unwrap();
    assert!(matches!(
        manager.stop().await,
        Err(Error::Invalid("CORE_RECONFIGURATION_PENDING"))
    ));
    assert!(
        manager
            .ensure_with(&profiles, profiles[0], |_| async { Ok(()) })
            .await
            .is_err()
    );
    assert_eq!(manager.state().unwrap(), state);
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "second");
    let manager = CoreManager::new(root.clone(), configuration.clone());
    assert!(matches!(
        manager
            .recover_update(plan.plan_id, |e| async { via(e).await.map(|_| ()) })
            .await
            .unwrap(),
        CoreOutcome::Restored { core_down: false }
    ));

    // Crash/lock at commit: finish manifest writes without another process restart.
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], first.port))
        .await
        .unwrap();
    let held = std::sync::Mutex::new(None);
    let result = manager
        .apply_update(plan.plan_id, admit(&configuration, plan.plan_id), |e| {
            let root = &root;
            let held = &held;
            async move {
                via(e).await?;
                *held.lock().unwrap() = Some(
                    fs::OpenOptions::new()
                        .read(true)
                        .share_mode(1)
                        .open(root.join("manifest.json"))
                        .unwrap(),
                );
                Ok(())
            }
        })
        .await;
    assert!(result.is_err());
    assert_eq!(
        configuration
            .lock()
            .unwrap()
            .core_update()
            .unwrap()
            .unwrap()
            .phase,
        UpdatePhase::Committing {}
    );
    let running = manager.state().unwrap();
    *held.lock().unwrap() = None;
    assert!(matches!(
        manager
            .recover_update(plan.plan_id, |_| async {
                panic!("must not repeat health or spawn after commit intent")
            })
            .await
            .unwrap(),
        CoreOutcome::Reconfigured { .. }
    ));
    assert_eq!(manager.state().unwrap(), running);
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "first");
    // Unknown creation is not replayed, even when no process happens to exist.
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], second.port))
        .await
        .unwrap();
    let execution = admit(&configuration, plan.plan_id);
    let plan = configuration
        .lock()
        .unwrap()
        .start_core_update(plan.plan_id, execution)
        .unwrap();
    let CoreState::Running { ref process, .. } = plan.previous else {
        unreachable!()
    };
    CoreProcess::attach(process).unwrap().stop().unwrap();
    let down = CoreState::Down {
        generation: plan.old_generation().unwrap(),
    };
    let starting = CoreState::Starting {
        generation: plan.candidate,
    };
    {
        let mut store = configuration.lock().unwrap();
        store
            .transition_core_update(plan.plan_id, &plan.previous, down)
            .unwrap();
        let state = store.core_state().unwrap();
        store
            .transition_core_update(plan.plan_id, &state, starting.clone())
            .unwrap();
    }
    assert!(matches!(
        manager
            .recover_update(plan.plan_id, |_| async {
                panic!("unknown creation must not run")
            })
            .await,
        Err(Error::Invalid("CORE_UPDATE_START_UNKNOWN"))
    ));
    assert_eq!(manager.state().unwrap(), starting);
    // Test harness knows it never spawned; production must reconcile evidence.
    configuration
        .lock()
        .unwrap()
        .transition_core_update(
            plan.plan_id,
            &starting,
            CoreState::Down {
                generation: plan.candidate,
            },
        )
        .unwrap();
    manager
        .recover_update(plan.plan_id, |e| async { via(e).await.map(|_| ()) })
        .await
        .unwrap();

    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], second.port))
        .await
        .unwrap();
    manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |e| async { via(e).await.map(|_| ()) },
        )
        .await
        .unwrap();
    drop(first); // The unchanged second profile is now unhealthy.
    let third = upstream("third").await;
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], third.port))
        .await
        .unwrap();
    assert!(matches!(
        manager
            .apply_update(
                plan.plan_id,
                admit(&configuration, plan.plan_id),
                |e| async { via(e).await.map(|_| ()) }
            )
            .await
            .unwrap(),
        CoreOutcome::Reconfigured { .. }
    ));
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "third");
    assert!(via(endpoints[1].clone()).await.is_err());
    let bad = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let plan = manager
        .prepare_update(&edit(
            &configuration,
            profiles[0],
            bad.local_addr().unwrap().port(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        manager
            .apply_update(
                plan.plan_id,
                admit(&configuration, plan.plan_id),
                |e| async { via(e).await.map(|_| ()) }
            )
            .await
            .unwrap(),
        CoreOutcome::Restored { core_down: false }
    ));
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "third");
    drop(bad);

    // If restoring connectivity also fails, keep old configuration and report Down.
    let plan = manager
        .prepare_update(&edit(&configuration, profiles[0], second.port))
        .await
        .unwrap();
    let old_revision = configuration.snapshot().unwrap().revision;
    drop(third);
    drop(second);
    let outcome = manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |e| async { via(e).await.map(|_| ()) },
        )
        .await
        .unwrap();
    assert!(matches!(outcome, CoreOutcome::Restored { core_down: true }));
    assert_eq!(configuration.snapshot().unwrap().revision, old_revision);
    assert!(
        matches!(manager.state().unwrap(), CoreState::Down { generation } if generation == plan.previous_generation)
    );
    manager.stop().await.unwrap();
}
