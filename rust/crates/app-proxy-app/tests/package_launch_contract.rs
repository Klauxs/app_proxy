#![cfg(windows)]
use app_proxy_core::{launch::*, model::*};
use app_proxy_windows::{
    identity, installation, instance_data,
    instance_resource::{InstanceResource, ResourceOwner, ResourceRegistry},
    package,
    package_launch::PackageOutcome,
    process::{CreationMode, SpawnSpec},
    store::Store,
};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use uuid::Uuid;

#[test]
#[ignore = "requires Claude MSIX; activates only an expired product helper, never Claude"]
fn expired_production_helper_receipt_is_shared_from_default_store_location() {
    let package = package::discover("claude").unwrap();
    let temp = tempfile::Builder::new()
        .prefix("AppProxyRust-PackageContract-")
        .tempdir_in(instance_data::local_app_data().unwrap())
        .unwrap();
    let mut store = Store::create(&temp.path().join("store")).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    let instance = Uuid::new_v4();
    let locator = ApplicationLocator::Msix {
        family_name: package.family_name.clone(),
        app_id: package.app_id.clone(),
    };
    manifest.applications.push(Application {
        id: app,
        name: "package contract".into(),
        revision: 1,
        locator: locator.clone(),
        template_ref: Template::Claude,
    });
    manifest.instances.push(Instance {
        id: instance,
        application_id: app,
        name: "package contract".into(),
        revision: 1,
        data: InstanceData::Original {},
        args: vec![],
        env: SavedEnvironment::default(),
        cwd: WorkingDirectory::Application {},
        network: NetworkBinding::Direct {},
        guard: GuardConfig {
            desired: Desired::Disabled,
            policy: GuardPolicy::StopUnproxied,
        },
    });
    store.commit(manifest.revision, manifest).unwrap();
    let resolved = installation::resolve(&locator).unwrap();
    let resource = InstanceResource::resolve(&resolved, None).unwrap();
    let key = resource.digest();
    let registry = ResourceRegistry::for_test_at(&temp.path().join("resources")).unwrap();
    let mut reservation = registry.acquire(resource).unwrap();
    let id = Uuid::new_v4();
    let owner = ResourceOwner {
        store_id: store.load().unwrap().store_id,
        attempt_id: id,
        epoch: Uuid::new_v4(),
    };
    reservation.reserve(owner).unwrap();
    store
        .begin_launch(
            &LaunchRequest {
                request_id: id,
                instance_id: instance,
                origin: LaunchOrigin::Interactive,
            },
            owner.epoch,
        )
        .unwrap();
    let mut phase = LaunchPhase::Accepted {};
    for next in [
        LaunchPhase::Resolving {},
        LaunchPhase::CheckingInstance {},
        LaunchPhase::PreparingProxy {},
        LaunchPhase::PreparingData {},
    ] {
        store
            .advance_launch(id, owner.epoch, &phase, next.clone())
            .unwrap();
        phase = next;
    }
    store
        .ready_launch(
            id,
            owner.epoch,
            LaunchBinding {
                dependency_digest: [1; 32],
                resource_key: key,
                executable: resolved.executable().into(),
                image: resolved.image().clone(),
                session_id: identity::current().unwrap().session_id,
                network: LaunchNetwork::Direct {},
            },
        )
        .unwrap();
    let dispatch = store
        .dispatch_package_launch(id, owner.epoch, &package)
        .unwrap();
    let permit = reservation.authorize_spawn(dispatch).unwrap();
    let helper = Path::new(env!("CARGO_BIN_EXE_app-proxy-host"));
    let ticket = store
        .prepare_package_launch(
            permit,
            &resolved,
            helper,
            SpawnSpec {
                exe: resolved.executable().into(),
                args: vec![],
                cwd: resolved.executable().parent().unwrap().into(),
                environment: Default::default(),
                mode: CreationMode::Normal,
            },
        )
        .unwrap();
    // Never activate an executable capability: wait longer than its fixed TTL.
    std::thread::sleep(Duration::from_secs(21));
    package::activate_launch(&package, helper, &ticket.request_path()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(6);
    let proof = loop {
        match ticket.outcome().unwrap() {
            PackageOutcome::NotCreated(proof) => break proof,
            PackageOutcome::Pending if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(30))
            }
            _ => panic!("expired helper did not publish a shared no-creation receipt"),
        }
    };
    store
        .fail_launch_not_created(id, owner.epoch, &proof)
        .unwrap();
    reservation.reconcile(&mut store).unwrap();
    assert!(!store.launch_request(id).unwrap().unwrap().resource_pending);
    assert!(matches!(
        store
            .package_launch_ticket(id)
            .unwrap()
            .unwrap()
            .outcome()
            .unwrap(),
        PackageOutcome::NotCreated(_)
    ));
    if package.isolated_storage {
        // This UUID belongs to the temporary store created above. Never remove
        // the package's LocalState or the shared AppProxyRust container.
        let namespace = instance_data::local_app_data()
            .unwrap()
            .join("Packages")
            .join(&package.family_name)
            .join("LocalState")
            .join("AppProxyRust")
            .join(owner.store_id.to_string());
        let request_namespace = ticket
            .request_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_owned();
        assert_eq!(
            std::fs::canonicalize(&namespace).unwrap(),
            std::fs::canonicalize(request_namespace).unwrap()
        );
        drop(ticket);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match std::fs::remove_dir_all(&namespace) {
                Ok(()) => break,
                Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(30))
                }
                Err(error) => panic!("cannot clean package fixture: {error}"),
            }
        }
    }
}
