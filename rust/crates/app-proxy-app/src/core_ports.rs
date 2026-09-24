//! Reserve listeners before starting a core. Only a failed bind moves an entry;
//! an external listener is never adopted or stopped.
use app_proxy_core::model::Manifest;
use app_proxy_windows::{Error, Result};
use std::{collections::HashSet, net::TcpListener};
use uuid::Uuid;

pub(crate) fn reserve(
    manifest: &mut Manifest,
    profiles: &[Uuid],
) -> Result<(Vec<TcpListener>, bool)> {
    let mut reservations = Vec::new();
    let mut changed = false;
    let mut ports: HashSet<_> = manifest.profiles.iter().map(|p| p.endpoint.port).collect();
    let mut ids = profiles.to_vec();
    ids.sort();
    ids.dedup();
    for id in ids {
        let profile = manifest
            .profiles
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or(Error::Invalid("PROFILE_NOT_FOUND"))?;
        let endpoint = &mut profile.endpoint;
        match TcpListener::bind((endpoint.host, endpoint.port)) {
            Ok(listener) => reservations.push(listener),
            Err(_) => {
                let mut replacement = None;
                let mut rejected = Vec::new();
                for _ in 0..64 {
                    let listener = TcpListener::bind((endpoint.host, 0))
                        .map_err(|_| Error::Invalid("LOCAL_PROXY_PORT_UNAVAILABLE"))?;
                    let port = listener.local_addr()?.port();
                    // Retain rejected reservations too, so the OS cannot hand
                    // the same unused-but-configured port straight back to us.
                    if ports.insert(port) {
                        reservations.push(listener);
                        replacement = Some(port);
                        break;
                    }
                    rejected.push(listener);
                }
                endpoint.port =
                    replacement.ok_or(Error::Invalid("LOCAL_PROXY_PORT_UNAVAILABLE"))?;
                profile.revision = profile
                    .revision
                    .checked_add(1)
                    .ok_or(Error::Invalid("REVISION_EXHAUSTED"))?;
                changed = true;
            }
        }
    }
    Ok((reservations, changed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use app_proxy_core::model::*;
    use app_proxy_windows::store::Store;

    #[test]
    fn reserves_available_ports_and_moves_only_conflicts_without_touching_foreign_listeners() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::create(&temp.path().join("store")).unwrap();
        let mut manifest = store.load().unwrap();
        let held: Vec<_> = (0..3)
            .map(|_| TcpListener::bind("127.0.0.1:0").unwrap())
            .collect();
        for listener in &held {
            let node_id = Uuid::new_v4();
            manifest.profiles.push(ProxyProfile {
                id: Uuid::new_v4(),
                name: "route".into(),
                revision: 1,
                kind: ProxyKind::Managed,
                endpoint: Endpoint {
                    host: "127.0.0.1".parse().unwrap(),
                    port: listener.local_addr().unwrap().port(),
                },
                selected_node_id: node_id,
                source: ProxySource::Manual {
                    nodes: vec![ManualNode {
                        id: node_id,
                        name: "node".into(),
                        protocol: ManualProtocol::Http,
                        host: "proxy.invalid".into(),
                        port: 8080,
                        credentials: None,
                    }],
                },
            });
        }
        let before: Manifest =
            serde_json::from_slice(&serde_json::to_vec(&manifest).unwrap()).unwrap();
        let mut held = held;
        drop(held.remove(1));
        let ids: Vec<_> = manifest.profiles.iter().take(2).map(|p| p.id).collect();
        let (reservations, changed) = reserve(&mut manifest, &[ids[0], ids[1], ids[0]]).unwrap();
        assert!(changed);
        assert_ne!(
            manifest.profiles[0].endpoint.port,
            before.profiles[0].endpoint.port
        );
        assert_eq!(manifest.profiles[0].revision, 2);
        assert!(manifest.profiles[1].endpoint == before.profiles[1].endpoint);
        assert_eq!(manifest.profiles[1].revision, 1);
        assert!(manifest.profiles[2].endpoint == before.profiles[2].endpoint);
        assert_eq!(manifest.profiles[2].revision, 1);
        for listener in &held {
            assert!(std::net::TcpStream::connect(listener.local_addr().unwrap()).is_ok());
        }
        for profile in &manifest.profiles[..2] {
            assert!(TcpListener::bind((profile.endpoint.host, profile.endpoint.port)).is_err());
        }
        drop(reservations);
        let (_, changed) = reserve(&mut manifest, &ids).unwrap();
        assert!(!changed);
    }
}
