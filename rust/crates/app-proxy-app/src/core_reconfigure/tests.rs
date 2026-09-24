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
#[ignore = "requires APP_PROXY_TEST_SING_BOX; owned core and loopback peers only"]
async fn real_remove_retains_other_routes_rolls_back_failure_and_stops_last() {
    let binary = PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("store");
    let mut store = Store::create(&root).unwrap();
    let installed = root.join("bin/sing-box/1.14.1");
    fs::create_dir_all(&installed).unwrap();
    for name in ["sing-box.exe", "libcronet.dll"] {
        fs::copy(binary.parent().unwrap().join(name), installed.join(name)).unwrap();
    }
    let peer = upstream("retained").await;
    let mut profiles = Vec::new();
    let mut endpoints = Vec::new();
    let mut reserved = Vec::new();
    for n in 0..2 {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: socket.local_addr().unwrap().port(),
        };
        reserved.push(socket);
        let id = Uuid::new_v4();
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: store.load().unwrap().revision,
                action: ConfigAction::CreateManualProfile {
                    profile_id: id,
                    name: format!("route {n}"),
                    endpoint: endpoint.clone(),
                    node: input(peer.port),
                },
            })
            .unwrap();
        profiles.push(id);
        endpoints.push(endpoint);
    }
    drop(reserved);
    let configuration = Arc::new(Configuration::new(store));
    let _cleanup = Cleanup(configuration.clone());
    let manager = CoreManager::new(root.clone(), configuration.clone());
    // Real native creation succeeds, but Running and the start receipt have
    // not been written. Reconciliation must adopt only this job's exact core.
    let core_binary = singbox_binary::discover(&root).await.unwrap().unwrap();
    let start_request = Uuid::new_v4();
    let action = app_proxy_core::core_control::CoreAction::Start {
        profiles: profiles.clone(),
        required: profiles[0],
    };
    let (generation, prepared) = {
        let mut store = configuration.lock().unwrap();
        let generation = store.prepare_core_generation(&profiles).unwrap();
        store
            .begin_core_request(start_request, Uuid::new_v4(), &action)
            .unwrap();
        let prepared = CoreProcess::prepare(&mut store, &core_binary, &generation)
            .unwrap()
            .bind_request(&mut store, start_request, action)
            .unwrap();
        store
            .transition_core_state(
                &CoreState::Stopped {},
                CoreState::Starting {
                    generation: generation.id(),
                },
            )
            .unwrap();
        (generation, prepared)
    };
    let orphan = prepared.spawn(&core_binary, &generation).unwrap();
    wait_listeners(&orphan, &endpoints).await.unwrap();
    let original_identity = orphan.identity().clone();
    drop(orphan);
    assert!(
        matches!(manager.recover_start(generation.id()).await.unwrap(), CoreOutcome::Reconciled { process: Some(p), .. } if p == original_identity)
    );
    assert!(matches!(
        configuration
            .lock()
            .unwrap()
            .core_request_status(start_request)
            .unwrap(),
        Some(
            app_proxy_windows::core_requests::CoreRequestPhase::Complete {
                outcome: CoreOutcome::Reconciled { .. },
                ..
            }
        )
    ));
    let ready = manager
        .ensure_with(&profiles, profiles[0], |e| async {
            via(e).await.map(|_| ())
        })
        .await
        .unwrap();
    let revision = configuration.snapshot().unwrap().revision;
    let request = |profile_id| ConfigRequest {
        request_id: Uuid::new_v4(),
        expected_revision: configuration.snapshot().unwrap().revision,
        action: ConfigAction::RemoveProfile { profile_id },
    };
    let plan = manager.prepare_update(&request(profiles[0])).await.unwrap();
    assert_eq!(plan.removed_profiles, [profiles[0]]);
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    for e in &endpoints {
        assert_eq!(via(e.clone()).await.unwrap(), "retained");
    }
    // Fail only the candidate probe. Old generation has both listeners, so
    // restoring its removed route can prove the original generation healthy.
    let removed_port = endpoints[0].port;
    let outcome = manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |e| async move {
                if e.port == removed_port {
                    via(e).await.map(|_| ())
                } else {
                    Err(Error::Invalid("TEST_CANDIDATE_FAILURE"))
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CoreOutcome::Restored { core_down: false }
    ));
    assert_eq!(configuration.snapshot().unwrap().revision, revision);
    for e in &endpoints {
        assert_eq!(via(e.clone()).await.unwrap(), "retained");
    }
    // A switch can hit the same gap. Recovery identifies the candidate before
    // stopping it and restoring both old listeners; it must not duplicate it.
    let interrupted = manager.prepare_update(&request(profiles[0])).await.unwrap();
    let execution = admit(&configuration, interrupted.plan_id);
    let plan = configuration
        .lock()
        .unwrap()
        .start_core_update(interrupted.plan_id, execution)
        .unwrap();
    let CoreState::Running { ref process, .. } = plan.previous else {
        panic!("old core")
    };
    CoreProcess::attach(process).unwrap().stop().unwrap();
    let (candidate, prepared) = {
        let mut store = configuration.lock().unwrap();
        let down = CoreState::Down {
            generation: plan.old_generation().unwrap(),
        };
        store
            .transition_core_update(plan.plan_id, &plan.previous, down.clone())
            .unwrap();
        let candidate = store.open_core_generation(plan.candidate).unwrap();
        let prepared = CoreProcess::prepare(&mut store, &core_binary, &candidate).unwrap();
        store
            .transition_core_update(
                plan.plan_id,
                &down,
                CoreState::Starting {
                    generation: candidate.id(),
                },
            )
            .unwrap();
        (candidate, prepared)
    };
    let orphan = prepared.spawn(&core_binary, &candidate).unwrap();
    let candidate_identity = orphan.identity().clone();
    drop(orphan);
    assert!(matches!(
        manager
            .recover_update(plan.plan_id, |e| async { via(e).await.map(|_| ()) })
            .await
            .unwrap(),
        CoreOutcome::Restored { core_down: false }
    ));
    assert!(CoreProcess::recover(&candidate_identity).unwrap().is_none());
    for e in &endpoints {
        assert_eq!(via(e.clone()).await.unwrap(), "retained");
    }
    let plan = manager.prepare_update(&request(profiles[0])).await.unwrap();
    let outcome = manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |e| async { via(e).await.map(|_| ()) },
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, CoreOutcome::ProfileRemoved { profile_id, revision: r } if profile_id == profiles[0] && r == revision + 1)
    );
    assert!(via(endpoints[0].clone()).await.is_err());
    assert_eq!(via(endpoints[1].clone()).await.unwrap(), "retained");
    assert_eq!(
        manager
            .snapshot()
            .unwrap()
            .profiles
            .iter()
            .map(|p| p.id)
            .collect::<Vec<_>>(),
        [profiles[1]]
    );
    let plan = manager.prepare_update(&request(profiles[1])).await.unwrap();
    let outcome = manager
        .apply_update(
            plan.plan_id,
            admit(&configuration, plan.plan_id),
            |_| async { panic!("last removal must not start or probe a core") },
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, CoreOutcome::ProfileRemoved { profile_id, revision: r } if profile_id == profiles[1] && r == revision + 2)
    );
    assert_eq!(manager.state().unwrap(), CoreState::Stopped {});
    assert!(
        configuration
            .lock()
            .unwrap()
            .load()
            .unwrap()
            .profiles
            .is_empty()
    );
    assert!(via(endpoints[1].clone()).await.is_err());
}

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; owned cores and loopback synthetic Shadowsocks peers only"]
async fn real_subscription_selection_and_refresh_use_shared_core_confirmation_and_rollback() {
    use app_proxy_core::subscription;
    use app_proxy_windows::subscription_stage::ImportRequest;
    use std::{process::Stdio, time::Duration};
    let binary = PathBuf::from(std::env::var_os("APP_PROXY_TEST_SING_BOX").unwrap());
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
    let reserves: Vec<_> = (0..4)
        .map(|_| std::net::TcpListener::bind("127.0.0.1:0").unwrap())
        .collect();
    let ports: Vec<_> = reserves
        .iter()
        .map(|s| s.local_addr().unwrap().port())
        .collect();
    let config = serde_json::json!({ "log": {"disabled": true},
        "inbounds": [
            {"type":"shadowsocks", "tag":"first", "listen":"127.0.0.1", "listen_port":ports[0], "method":"aes-128-gcm", "password":"fixture"},
            {"type":"shadowsocks", "tag":"second", "listen":"127.0.0.1", "listen_port":ports[1], "method":"aes-128-gcm", "password":"fixture"}
        ],
        "outbounds": [
            {"type":"http", "tag":"first", "server":"127.0.0.1", "server_port":first.port},
            {"type":"http", "tag":"second", "server":"127.0.0.1", "server_port":second.port}
        ],
        "route": {"rules":[
            {"inbound":["first"], "action":"route", "outbound":"first"},
            {"inbound":["second"], "action":"route", "outbound":"second"},
            {"action":"reject"}
        ]}
    });
    let peer_config = temp.path().join("synthetic-peer.json");
    fs::write(&peer_config, serde_json::to_vec(&config).unwrap()).unwrap();
    drop(reserves);
    let mut peer = tokio::process::Command::new(&binary)
        .args(["run", "-c"])
        .arg(peer_config)
        .creation_flags(0x08000000)
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        for port in &ports[..2] {
            loop {
                assert!(peer.try_wait().unwrap().is_none());
                if tokio::net::TcpStream::connect(("127.0.0.1", *port))
                    .await
                    .is_ok()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    })
    .await
    .unwrap();
    let parsed = |second_port| {
        subscription::parse(&format!(
        "ss://aes-128-gcm:fixture@127.0.0.1:{}#A\nss://aes-128-gcm:fixture@127.0.0.1:{second_port}#B", ports[0]
    )).unwrap()
    };
    let import = ImportRequest {
        request_id: Uuid::new_v4(),
        expected_revision: 1,
        profile_id: Uuid::new_v4(),
        name: "subscription".into(),
        endpoint: Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: ports[2],
        },
        url: "https://synthetic.invalid/never-downloaded".into(),
        selected_names: vec!["A".into()],
    };
    let staged = store
        .stage_subscription_import(&import, &parsed(ports[1]))
        .unwrap();
    assert!(matches!(
        store.apply_config(&staged.request).unwrap(),
        app_proxy_windows::config_transaction::ConfigOutcome::Applied { .. }
    ));
    let manual = Uuid::new_v4();
    assert!(matches!(
        store
            .apply_config(&ConfigRequest {
                request_id: Uuid::new_v4(),
                expected_revision: 2,
                action: ConfigAction::CreateManualProfile {
                    profile_id: manual,
                    name: "manual".into(),
                    endpoint: Endpoint {
                        host: "127.0.0.1".parse().unwrap(),
                        port: ports[3]
                    },
                    node: input(first.port)
                }
            })
            .unwrap(),
        app_proxy_windows::config_transaction::ConfigOutcome::Applied { .. }
    ));
    let configuration = Arc::new(Configuration::new(store));
    let _cleanup = Cleanup(configuration.clone());
    let manager = CoreManager::new(root, configuration.clone());
    let ready = manager
        .ensure_with(
            &[import.profile_id, manual],
            import.profile_id,
            |endpoint| async {
                assert_eq!(via(endpoint).await?, "first");
                Ok(())
            },
        )
        .await
        .unwrap();
    let before = configuration.snapshot().unwrap();
    let ProxySource::Subscription { nodes, .. } = &before.profiles[0].source else {
        panic!()
    };
    let second_node = nodes[1].id;
    let impact = manager
        .prepare_update(&ConfigRequest {
            request_id: Uuid::new_v4(),
            expected_revision: before.revision,
            action: ConfigAction::EditSubscriptionProfile {
                profile_id: import.profile_id,
                edit: SubscriptionEdit::Select {
                    expected_source_revision: 1,
                    node_ids: vec![second_node],
                },
            },
        })
        .await
        .unwrap();
    assert_eq!(impact.affected_profiles.len(), 2);
    assert_eq!(configuration.snapshot().unwrap().revision, before.revision);
    assert_eq!(via(import.endpoint.clone()).await.unwrap(), "first");
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    let changed_port = ports[2];
    let result = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |endpoint| async move {
                let expected = if endpoint.port == changed_port {
                    "second"
                } else {
                    "first"
                };
                assert_eq!(via(endpoint).await?, expected);
                Ok(())
            },
        )
        .await
        .unwrap();
    assert!(matches!(result, CoreOutcome::Reconfigured { .. }));
    assert!(CoreProcess::recover(&ready.process).unwrap().is_none());
    assert_eq!(
        configuration.snapshot().unwrap().profiles[0].selected_node_id,
        second_node
    );
    let revision = configuration.snapshot().unwrap().revision;

    // Valid syntax, unreachable selected upstream: keep the previous selection,
    // source revision and both working routes after the failed candidate.
    let unused = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let staged = configuration
        .lock()
        .unwrap()
        .stage_subscription_refresh(
            Uuid::new_v4(),
            import.profile_id,
            1,
            import.request_id,
            &parsed(unused.local_addr().unwrap().port()),
        )
        .unwrap();
    let impact = manager.prepare_update(&staged.request).await.unwrap();
    let result = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |endpoint| async { via(endpoint).await.map(|_| ()) },
        )
        .await
        .unwrap();
    assert!(matches!(result, CoreOutcome::Restored { core_down: false }));
    assert_eq!(configuration.snapshot().unwrap().revision, revision);
    assert_eq!(via(import.endpoint.clone()).await.unwrap(), "second");
    assert_eq!(
        via(Endpoint {
            host: "127.0.0.1".parse().unwrap(),
            port: ports[3]
        })
        .await
        .unwrap(),
        "first"
    );
    drop(unused);

    let staged = configuration
        .lock()
        .unwrap()
        .stage_subscription_refresh(
            Uuid::new_v4(),
            import.profile_id,
            1,
            import.request_id,
            &parsed(ports[0]),
        )
        .unwrap();
    let impact = manager.prepare_update(&staged.request).await.unwrap();
    assert!(matches!(
        manager
            .apply_update(
                impact.plan_id,
                admit(&configuration, impact.plan_id),
                |endpoint| async {
                    assert_eq!(via(endpoint).await?, "first");
                    Ok(())
                }
            )
            .await
            .unwrap(),
        CoreOutcome::Reconfigured { .. }
    ));
    let updated = configuration.snapshot().unwrap();
    assert_eq!(updated.profiles[0].selected_node_id, second_node);
    assert!(matches!(
        updated.profiles[0].source,
        ProxySource::Subscription { revision: 2, .. }
    ));
    manager.stop().await.unwrap();
    peer.start_kill().unwrap();
    peer.wait().await.unwrap();
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

#[tokio::test]
#[ignore = "requires APP_PROXY_TEST_SING_BOX; real owned core in an isolated store"]
async fn real_shared_core_expansion_preserves_routes_reuses_subsets_and_rolls_back() {
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
    let instance = Uuid::new_v4();
    let application = Uuid::new_v4();
    let mut manifest = store.load().unwrap();
    manifest.applications.push(Application {
        id: application,
        name: "launch fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: std::env::current_exe().unwrap(),
        },
        template_ref: Template::Environment,
    });
    manifest.instances.push(Instance {
        id: instance,
        application_id: application,
        name: "launch fixture".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec![],
        env: SavedEnvironment::default(),
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Profile {
            profile_id: profiles[0],
        },
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    store.commit(manifest.revision, manifest).unwrap();
    drop(reservations);
    let configuration = Arc::new(Configuration::new(store));
    let _cleanup = Cleanup(configuration.clone());
    let manager = CoreManager::new(root.clone(), configuration.clone());

    let ready = manager
        .ensure_with(&profiles[..1], profiles[0], |e| async {
            via(e).await.map(|_| ())
        })
        .await
        .unwrap();
    // A queued launch permission and a queued stop share the lifecycle gate.
    // Permission publishes first; stop must fail before terminating the core.
    use app_proxy_core::launch::*;
    let epoch = Uuid::new_v4();
    let launch = LaunchRequest {
        request_id: Uuid::new_v4(),
        instance_id: instance,
        origin: LaunchOrigin::Interactive,
    };
    {
        let mut store = configuration.lock().unwrap();
        store.begin_launch(&launch, epoch).unwrap();
        let mut phase = LaunchPhase::Accepted {};
        for next in [
            LaunchPhase::Resolving {},
            LaunchPhase::CheckingInstance {},
            LaunchPhase::PreparingProxy {},
            LaunchPhase::PreparingData {},
        ] {
            store
                .advance_launch(launch.request_id, epoch, &phase, next.clone())
                .unwrap();
            phase = next;
        }
    }
    let identity = app_proxy_windows::identity::current().unwrap();
    let binding = LaunchBinding {
        dependency_digest: [1; 32],
        resource_key: [2; 32],
        executable: identity.image_path,
        image: identity.image_file,
        session_id: identity.session_id,
        network: LaunchNetwork::Profile {
            profile_id: profiles[0],
            generation: ready.generation,
            endpoint: endpoints[0].clone(),
        },
    };
    let held = manager.gate.lock().await;
    let permission = manager.ready_launch(launch.request_id, epoch, binding);
    tokio::pin!(permission);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut permission)
            .await
            .is_err()
    );
    let stopping = manager.stop();
    tokio::pin!(stopping);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut stopping)
            .await
            .is_err()
    );
    drop(held);
    let (permission, stopping) = tokio::join!(permission, stopping);
    permission.unwrap();
    assert!(matches!(
        stopping,
        Err(Error::Invalid("CORE_LAUNCH_IN_PROGRESS"))
    ));
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "first");
    let reused = manager
        .ensure_with(&profiles[..1], profiles[0], |e| async {
            via(e).await.map(|_| ())
        })
        .await
        .unwrap();
    assert_eq!(reused.process, ready.process);
    let blocked_plan = manager
        .prepare_expand(
            Uuid::new_v4(),
            configuration.snapshot().unwrap().revision,
            &profiles[1..],
            profiles[1],
        )
        .await
        .unwrap();
    let apply = Uuid::new_v4();
    configuration
        .lock()
        .unwrap()
        .begin_core_request(
            apply,
            epoch,
            &app_proxy_core::core_control::CoreAction::ApplyUpdate {
                plan_id: blocked_plan.plan_id,
            },
        )
        .unwrap();
    assert!(matches!(
        manager
            .apply_update(blocked_plan.plan_id, apply, |_| async { Ok(()) })
            .await,
        Err(Error::Invalid("CORE_LAUNCH_IN_PROGRESS"))
    ));
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    {
        let mut store = configuration.lock().unwrap();
        store
            .finish_core_request(
                apply,
                epoch,
                CoreOutcome::Failed {
                    code: "CORE_LAUNCH_IN_PROGRESS".into(),
                },
            )
            .unwrap();
        store.request_launch_cancel(launch.request_id).unwrap();
        store
            .advance_launch(
                launch.request_id,
                epoch,
                &LaunchPhase::ReadyToSpawn {},
                LaunchPhase::Cancelled {},
            )
            .unwrap();
    }
    let revision = configuration.snapshot().unwrap().revision;
    assert!(matches!(
        manager
            .ensure_with(&profiles[1..], profiles[1], |_| async { Ok(()) })
            .await,
        Err(Error::Invalid("CORE_RECONFIGURE_REQUIRES_CONFIRMATION"))
    ));
    let impact = manager
        .prepare_expand(Uuid::new_v4(), revision, &profiles[1..], profiles[1])
        .await
        .unwrap();
    assert_eq!(impact.affected_profiles, [profiles[0]]);
    assert_eq!(impact.added_profiles, [profiles[1]]);
    assert!(
        matches!(manager.state().unwrap(), CoreState::Running { process, .. } if process == ready.process)
    );
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "first");
    assert!(via(endpoints[1].clone()).await.is_err());
    // Only the newly requested route fails its probe. The old route must recover
    // even though the requested profile was never part of the original process.
    let new_port = endpoints[1].port;
    let restored = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |e| async move {
                if e.port == new_port {
                    Err(Error::Invalid("TEST_NEW_ROUTE_FAILURE"))
                } else {
                    via(e).await.map(|_| ())
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        restored,
        CoreOutcome::Restored { core_down: false }
    ));
    assert_eq!(via(endpoints[0].clone()).await.unwrap(), "first");
    assert!(via(endpoints[1].clone()).await.is_err());
    assert_eq!(configuration.snapshot().unwrap().revision, revision);
    let impact = manager
        .prepare_expand(Uuid::new_v4(), revision, &profiles[1..], profiles[1])
        .await
        .unwrap();
    // The new entry can become occupied after confirmation. Keep the blocker
    // alive: expansion must commit a replacement without moving the old route.
    let blocker = std::net::TcpListener::bind((endpoints[1].host, endpoints[1].port)).unwrap();
    let expanded = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |e| async { via(e).await.map(|_| ()) },
        )
        .await
        .unwrap();
    let CoreOutcome::Reconfigured {
        process,
        revision: committed_revision,
        ..
    } = expanded
    else {
        panic!("not expanded")
    };
    assert_eq!(committed_revision, revision + 1);
    let saved = configuration.snapshot().unwrap();
    let new_endpoint = saved
        .profiles
        .iter()
        .find(|p| p.id == profiles[1])
        .unwrap()
        .endpoint
        .clone();
    assert!(new_endpoint != endpoints[1]);
    assert!(std::net::TcpStream::connect(blocker.local_addr().unwrap()).is_ok());
    endpoints[1] = new_endpoint;
    for endpoint in &endpoints {
        assert_eq!(via(endpoint.clone()).await.unwrap(), "first");
    }
    let reused = manager
        .ensure_with(&profiles[..1], profiles[0], |e| async {
            via(e).await.map(|_| ())
        })
        .await
        .unwrap();
    assert_eq!(reused.process, process);
    let reused = manager
        .ensure_with(&profiles[1..], profiles[1], |e| async {
            via(e).await.map(|_| ())
        })
        .await
        .unwrap();
    assert_eq!(reused.process, process);
    assert_eq!(configuration.snapshot().unwrap().revision, revision + 1);

    // A failed candidate can lose an old port before rollback. Recovery still
    // restores the old route on a fresh port, even with a bound instance.
    let impact = manager
        .prepare_update(&edit(&configuration, profiles[0], first.port))
        .await
        .unwrap();
    let first_probe = std::sync::atomic::AtomicBool::new(true);
    let rollback_blocker = std::sync::Mutex::new(None);
    let outcome = manager
        .apply_update(
            impact.plan_id,
            admit(&configuration, impact.plan_id),
            |e| async {
                if first_probe.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    let CoreState::Running { process, .. } = manager.state().unwrap() else {
                        panic!("running candidate required")
                    };
                    CoreProcess::attach(&process).unwrap().stop().unwrap();
                    *rollback_blocker.lock().unwrap() = Some(
                        std::net::TcpListener::bind((endpoints[0].host, endpoints[0].port))
                            .unwrap(),
                    );
                    Err(Error::Invalid("TEST_CANDIDATE_FAILED"))
                } else {
                    via(e).await.map(|_| ())
                }
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        CoreOutcome::Restored { core_down: false }
    ));
    let saved = configuration.snapshot().unwrap();
    let recovered = saved
        .profiles
        .iter()
        .find(|p| p.id == profiles[0])
        .unwrap()
        .endpoint
        .clone();
    assert!(recovered != endpoints[0]);
    assert_eq!(via(recovered).await.unwrap(), "first");
    assert!(
        std::net::TcpStream::connect(
            rollback_blocker
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .local_addr()
                .unwrap()
        )
        .is_ok()
    );
    assert!(matches!(
        configuration
            .lock()
            .unwrap()
            .require_core_update(impact.plan_id)
            .unwrap()
            .result,
        Some(CoreOutcome::Restored { core_down: false })
    ));
    manager.stop().await.unwrap();
}
