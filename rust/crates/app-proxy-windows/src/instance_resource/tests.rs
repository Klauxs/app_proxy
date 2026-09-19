use super::*;
use crate::{installation, store::Store};
use app_proxy_core::{launch::*, model::*};
use std::time::{Duration, Instant};

fn resource_for(path: &Path) -> InstanceResource {
    let application = installation::resolve(&ApplicationLocator::Exe {
        path: path.to_owned(),
    })
    .unwrap();
    InstanceResource::resolve(&application, None).unwrap()
}
fn current_resource() -> InstanceResource {
    resource_for(&std::env::current_exe().unwrap())
}
fn owner() -> ResourceOwner {
    ResourceOwner {
        store_id: Uuid::new_v4(),
        attempt_id: Uuid::new_v4(),
        epoch: Uuid::new_v4(),
    }
}

fn ready_store(
    root: &Path,
    resource: &InstanceResource,
    attempt_id: Uuid,
) -> (Store, ResourceOwner) {
    let mut store = Store::create(root).unwrap();
    let mut manifest = store.load().unwrap();
    let app = Uuid::new_v4();
    let instance = Uuid::new_v4();
    manifest.applications.push(Application {
        id: app,
        name: "fixture".into(),
        revision: 1,
        locator: ApplicationLocator::Exe {
            path: resource.executable.clone(),
        },
        template_ref: Template::Codex,
    });
    manifest.instances.push(Instance {
        id: instance,
        application_id: app,
        name: "fixture".into(),
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
    let owner = ResourceOwner {
        store_id: manifest.store_id,
        attempt_id,
        epoch: Uuid::new_v4(),
    };
    store.commit(manifest.revision, manifest).unwrap();
    store
        .begin_launch(
            &LaunchRequest {
                request_id: attempt_id,
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
            .advance_launch(attempt_id, owner.epoch, &phase, next.clone())
            .unwrap();
        phase = next;
    }
    store
        .ready_launch(
            attempt_id,
            owner.epoch,
            LaunchBinding {
                dependency_digest: [1; 32],
                resource_key: resource.key,
                executable: resource.executable.clone(),
                image: resource.image.clone(),
                session_id: resource.session_id,
                network: LaunchNetwork::Direct {},
            },
        )
        .unwrap();
    (store, owner)
}
fn spec(exe: PathBuf, cwd: PathBuf) -> process::SpawnSpec {
    process::SpawnSpec {
        exe,
        cwd,
        args: vec![],
        environment: app_proxy_core::EnvPatch::default(),
        mode: process::CreationMode::Normal,
    }
}
struct Child(process::StartedProcess);
impl Drop for Child {
    fn drop(&mut self) {
        let _ = self.0.terminate();
    }
}
fn child_spec(root: &Path, mode: &str) -> process::SpawnSpec {
    let mut spec = spec(
        std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap(),
        root.parent().unwrap().to_owned(),
    );
    spec.args = [
        "--exact",
        "instance_resource::tests::resource_child",
        "--ignored",
    ]
    .map(Into::into)
    .to_vec();
    spec.environment.set.insert(
        "APP_PROXY_RESOURCE_TEST_ROOT".into(),
        root.to_str().unwrap().into(),
    );
    spec.environment
        .set
        .insert("APP_PROXY_RESOURCE_TEST_MODE".into(), mode.into());
    spec
}
fn wait_ready(root: &Path, child: &mut Child) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !root.parent().unwrap().join("ready").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "fixture exited before ready"
        );
        assert!(Instant::now() < deadline, "fixture not ready");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn physical_keys_match_hard_links_and_separate_original_and_isolated_data() {
    let temp = tempfile::tempdir().unwrap();
    let exe = temp.path().join("fixture.exe");
    let alias = temp.path().join("alias.exe");
    fs::write(&exe, b"key fixture, never executed").unwrap();
    fs::hard_link(&exe, &alias).unwrap();
    assert_eq!(resource_for(&exe).digest(), resource_for(&alias).digest());
    let resource = resource_for(&exe);
    let (mut store, _) = ready_store(&temp.path().join("store"), &resource, Uuid::new_v4());
    let mut manifest = store.load().unwrap();
    let instance = manifest.instances[0].id;
    manifest.instances[0].data = InstanceData::Isolated {
        location: StorageLocation::Store {
            relative_path: PathBuf::from("instances").join(instance.to_string()),
        },
    };
    store.commit(manifest.revision, manifest).unwrap();
    let application = installation::resolve(&ApplicationLocator::Exe { path: exe }).unwrap();
    let data = store
        .prepare_instance_data(instance, None)
        .unwrap()
        .unwrap();
    let isolated = InstanceResource::resolve(&application, Some(&data)).unwrap();
    assert_ne!(resource.digest(), isolated.digest());
    let same_data = store
        .prepare_instance_data(instance, None)
        .unwrap()
        .unwrap();
    assert_eq!(
        isolated.digest(),
        InstanceResource::resolve(&application, Some(&same_data))
            .unwrap()
            .digest()
    );
    assert!(fs::rename(&data.paths.root, temp.path().join("moved-data")).is_err());
}

#[test]
fn concurrent_registry_initialization_and_moving_lock_between_threads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("resources");
    let barrier = Arc::new(std::sync::Barrier::new(4));
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let root = root.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                ResourceRegistry::open_at(&root).unwrap()
            })
        })
        .collect();
    let registries: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    let mut first = registries[0].acquire(current_resource()).unwrap();
    let owner = owner();
    first.reserve(owner).unwrap();
    assert!(matches!(
        registries[1].acquire(current_resource()),
        Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"))
    ));
    let first = std::thread::spawn(move || {
        first.release_before_spawn(owner).unwrap();
        first
    })
    .join()
    .unwrap();
    drop(first);
    let mut second = registries[1].acquire(current_resource()).unwrap();
    second.reserve(super::tests::owner()).unwrap();
}

#[test]
fn foreign_registry_bad_claim_and_changed_claim_are_preserved() {
    let temp = tempfile::tempdir().unwrap();
    let foreign = temp.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    fs::write(foreign.join("keep"), b"user file").unwrap();
    assert!(ResourceRegistry::open_at(&foreign).is_err());
    assert_eq!(fs::read(foreign.join("keep")).unwrap(), b"user file");
    let registry = ResourceRegistry::open_at(&temp.path().join("resources")).unwrap();
    let mut reservation = registry.acquire(current_resource()).unwrap();
    let owner = owner();
    reservation.reserve(owner).unwrap();
    let path = registry.root.join(&reservation.name);
    let original = fs::read(&path).unwrap();
    let mut changed: ResourceClaim = store::decode(&original).unwrap();
    changed.owner.epoch = Uuid::new_v4();
    store::replace_protected(
        &registry.root,
        &registry.sid,
        &reservation.name,
        &store::encode(&changed, LIMIT).unwrap(),
        LIMIT,
    )
    .unwrap();
    let changed_bytes = fs::read(&path).unwrap();
    assert!(matches!(
        reservation.release_before_spawn(owner),
        Err(Error::Invalid("INSTANCE_RESOURCE_CLAIM_CHANGED"))
    ));
    assert_eq!(fs::read(&path).unwrap(), changed_bytes);
    store::replace_protected(
        &registry.root,
        &registry.sid,
        &reservation.name,
        b"broken",
        LIMIT,
    )
    .unwrap();
    assert!(reservation.release_before_spawn(owner).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"broken");
    drop(reservation);
    assert!(registry.acquire(current_resource()).is_err());
    assert_eq!(fs::read(path).unwrap(), b"broken");
}

#[test]
fn one_use_dispatch_blocks_unknown_retry_and_no_creation_evidence_is_scoped() {
    let temp = tempfile::tempdir().unwrap();
    let registry = ResourceRegistry::open_at(&temp.path().join("resources")).unwrap();
    let shared_id = Uuid::new_v4();
    let resource = current_resource();
    let (mut first_store, first_owner) = ready_store(&temp.path().join("a"), &resource, shared_id);
    let mut first = registry.acquire(resource).unwrap();
    first.reserve(first_owner).unwrap();
    let dispatch = first_store
        .dispatch_launch(shared_id, first_owner.epoch)
        .unwrap();
    drop(first.authorize_spawn(dispatch).unwrap()); // Simulate lost dispatch/result.
    first_store.recover_launches(Uuid::new_v4()).unwrap();
    assert!(
        first_store
            .dispatch_launch(shared_id, first_owner.epoch)
            .is_err()
    );
    assert!(first.release_before_spawn(first_owner).is_err());

    // Same external UUID in another store must not yield evidence for the first.
    let invalid_exe = temp.path().join("invalid.exe");
    fs::write(&invalid_exe, b"not a PE executable").unwrap();
    let resource = resource_for(&invalid_exe);
    let (mut second_store, second_owner) =
        ready_store(&temp.path().join("b"), &resource, shared_id);
    let mut second = registry.acquire(resource).unwrap();
    second.reserve(second_owner).unwrap();
    let dispatch = second_store
        .dispatch_launch(shared_id, second_owner.epoch)
        .unwrap();
    let permit = second.authorize_spawn(dispatch).unwrap();
    let failure = process::spawn_for_attempt(
        permit,
        spec(
            fs::canonicalize(invalid_exe).unwrap(),
            temp.path().to_owned(),
        ),
    )
    .err()
    .unwrap();
    let process::SpawnFailure::NotCreated { evidence, .. } = failure else {
        panic!("creation failure must be definite")
    };
    assert!(
        first_store
            .fail_launch_not_created(shared_id, first_owner.epoch, &evidence)
            .is_err()
    );
    assert!(first.release_not_created(first_owner, &evidence).is_err());
    assert_eq!(
        first_store
            .launch_request(shared_id)
            .unwrap()
            .unwrap()
            .phase,
        LaunchPhase::Indeterminate {}
    );
    assert!(
        second_store
            .fail_launch_not_created(shared_id, Uuid::new_v4(), &evidence)
            .is_err()
    );
    let local_bytes = fs::read(second_store.root().join("state/launch.json")).unwrap();
    let mut changed: serde_json::Value = serde_json::from_slice(&local_bytes).unwrap();
    changed["attempts"][0]["dispatch_id"] = serde_json::to_value(Uuid::new_v4()).unwrap();
    second_store
        .replace_bounded(
            "state/launch.json",
            &serde_json::to_vec(&changed).unwrap(),
            8 * 1024 * 1024,
        )
        .unwrap();
    assert!(
        second_store
            .fail_launch_not_created(shared_id, second_owner.epoch, &evidence)
            .is_err()
    );
    second_store
        .replace_bounded("state/launch.json", &local_bytes, 8 * 1024 * 1024)
        .unwrap();
    let original = second.claim().unwrap().clone();
    let mut changed = original.clone();
    changed.dispatch_id = Some(Uuid::new_v4());
    second.write(changed).unwrap();
    assert!(second.release_not_created(second_owner, &evidence).is_err());
    second.write(original).unwrap();
    second_store
        .fail_launch_not_created(shared_id, second_owner.epoch, &evidence)
        .unwrap();
    second.release_not_created(second_owner, &evidence).unwrap();
    second.reserve(owner()).unwrap();
    assert!(
        second_store
            .dispatch_launch(shared_id, second_owner.epoch)
            .is_err()
    );
}

#[test]
fn permission_retains_store_ownership_and_authorization_failure_proves_no_creation() {
    let temp = tempfile::tempdir().unwrap();
    let registry = ResourceRegistry::open_at(&temp.path().join("resources")).unwrap();
    let resource = current_resource();
    let root = temp.path().join("store");
    let (mut store, owner) = ready_store(&root, &resource, Uuid::new_v4());
    let mut reservation = registry.acquire(resource).unwrap();
    reservation.reserve(owner).unwrap();
    let dispatch = store
        .dispatch_launch(owner.attempt_id, owner.epoch)
        .unwrap();
    drop(store);
    assert!(matches!(
        Store::open(&root),
        Err(Error::Invalid("STORE_ALREADY_OWNED"))
    ));
    // Change the claim's owner behind the held lock; publication refuses to
    // overwrite it, and the consumed dispatch can only return no-creation proof.
    let mut changed = reservation.claim().unwrap().clone();
    changed.owner.epoch = Uuid::new_v4();
    store::replace_protected(
        &registry.root,
        &registry.sid,
        &reservation.name,
        &store::encode(&changed, LIMIT).unwrap(),
        LIMIT,
    )
    .unwrap();
    let failure = reservation.authorize_spawn(dispatch).err().unwrap();
    let process::SpawnFailure::NotCreated { evidence, .. } = failure else {
        panic!("no dispatch happened")
    };
    let mut store = Store::open(&root).unwrap();
    store
        .fail_launch_not_created(owner.attempt_id, owner.epoch, &evidence)
        .unwrap();
}

#[test]
fn recovered_reserved_claim_rejects_old_dispatch_after_owner_changes() {
    let temp = tempfile::tempdir().unwrap();
    let registry = ResourceRegistry::open_at(&temp.path().join("resources")).unwrap();
    let resource = current_resource();
    let (mut store, previous) = ready_store(&temp.path().join("store"), &resource, Uuid::new_v4());
    let mut reservation = registry.acquire(resource).unwrap();
    reservation.reserve(previous).unwrap();
    let dispatch = store
        .dispatch_launch(previous.attempt_id, previous.epoch)
        .unwrap();
    drop(reservation); // No global intent was ever published.
    let mut reservation = registry.acquire(current_resource()).unwrap();
    reservation.release_before_spawn(previous).unwrap();
    let next = owner();
    reservation.reserve(next).unwrap();
    assert!(matches!(
        reservation.authorize_spawn(dispatch),
        Err(process::SpawnFailure::NotCreated { .. })
    ));
    assert_eq!(reservation.claim().unwrap().owner, next);
    assert_eq!(
        reservation.claim().unwrap().phase,
        ResourcePhase::Reserved {}
    );
}

#[test]
#[ignore = "native subprocess fixture used by resource reservation tests"]
fn resource_child() {
    let root = PathBuf::from(std::env::var_os("APP_PROXY_RESOURCE_TEST_ROOT").unwrap());
    let mode = std::env::var("APP_PROXY_RESOURCE_TEST_MODE").unwrap();
    if mode == "unknown" {
        let registry = ResourceRegistry::open_at(&root).unwrap();
        let resource = current_resource();
        let (mut store, owner) = ready_store(
            &root.parent().unwrap().join("child-store"),
            &resource,
            Uuid::new_v4(),
        );
        let mut reservation = registry.acquire(resource).unwrap();
        reservation.reserve(owner).unwrap();
        let dispatch = store
            .dispatch_launch(owner.attempt_id, owner.epoch)
            .unwrap();
        let _permit = reservation.authorize_spawn(dispatch).unwrap();
        fs::write(root.parent().unwrap().join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    } else {
        assert_eq!(mode, "plain");
        fs::write(root.parent().unwrap().join("ready"), b"ready").unwrap();
        std::thread::sleep(Duration::from_secs(30));
    }
}

#[test]
fn native_owner_death_releases_kernel_lock_but_keeps_unknown_claim() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("resources");
    let registry = ResourceRegistry::open_at(&root).unwrap();
    let mut child = Child(process::spawn(child_spec(&root, "unknown")).unwrap());
    wait_ready(&root, &mut child);
    assert!(matches!(
        registry.acquire(current_resource()),
        Err(Error::Invalid("INSTANCE_RESOURCE_BUSY"))
    ));
    child.0.terminate().unwrap();
    let mut reservation = registry.acquire(current_resource()).unwrap();
    assert_eq!(
        reservation.claim().unwrap().phase,
        ResourcePhase::SpawnRequested {}
    );
    assert!(matches!(
        reservation.reserve(owner()),
        Err(Error::Invalid("INSTANCE_RESOURCE_RECOVERY_REQUIRED"))
    ));
    let existing = reservation.claim().unwrap().owner;
    assert!(reservation.release_before_spawn(existing).is_err());
    drop(reservation);
    assert_eq!(
        registry
            .acquire(current_resource())
            .unwrap()
            .claim()
            .unwrap()
            .phase,
        ResourcePhase::SpawnRequested {}
    );
}

#[test]
fn confirmed_process_keeps_claim_after_unlock_until_exact_exit() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("resources");
    let registry = ResourceRegistry::open_at(&root).unwrap();
    let resource = current_resource();
    let (mut store, owner) = ready_store(&temp.path().join("store"), &resource, Uuid::new_v4());
    let mut reservation = registry.acquire(resource).unwrap();
    reservation.reserve(owner).unwrap();
    let dispatch = store
        .dispatch_launch(owner.attempt_id, owner.epoch)
        .unwrap();
    let permit = reservation.authorize_spawn(dispatch).unwrap();
    let mut child = Child(process::spawn_for_attempt(permit, child_spec(&root, "plain")).unwrap());
    wait_ready(&root, &mut child);
    reservation
        .confirm(owner, child.0.identity.clone())
        .unwrap();
    store
        .confirm_launch(owner.attempt_id, owner.epoch, child.0.identity.clone())
        .unwrap();
    // Local confirmation cannot authorize another store to erase the global
    // evidence until its ACK is durable as well.
    let held = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(reservation.registry.root.join(&reservation.name))
        .unwrap();
    assert!(reservation.reconcile(&mut store).is_err());
    assert!(
        store
            .launch_request(owner.attempt_id)
            .unwrap()
            .unwrap()
            .resource_pending
    );
    assert!(matches!(
        reservation.release_exited(owner),
        Err(Error::Invalid("INSTANCE_RESOURCE_RECOVERY_REQUIRED"))
    ));
    drop(held);
    reservation.reconcile(&mut store).unwrap();
    assert!(
        !store
            .launch_request(owner.attempt_id)
            .unwrap()
            .unwrap()
            .resource_pending
    );
    drop(reservation);
    let mut reservation = registry.acquire(current_resource()).unwrap();
    assert!(reservation.reserve(super::tests::owner()).is_err());
    assert!(matches!(
        reservation.release_exited(owner),
        Err(Error::Invalid("INSTANCE_STILL_RUNNING"))
    ));
    child.0.terminate().unwrap();
    reservation.release_exited(owner).unwrap();
    assert!(store.observe_launch_exit(owner.attempt_id).unwrap());
    reservation.reserve(super::tests::owner()).unwrap();
}
