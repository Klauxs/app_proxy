use super::*;
use crate::instance_resource::ResourceOwner;
use app_proxy_core::launch::LaunchNetwork;
use uuid::Uuid;

pub(crate) fn publish_for_dispatch(store: &Store, permit: AuthorizedSpawn<'_>) -> PackageTicket {
    let fixture = Fixture::new(0);
    let mut request: Request = store::decode(
        &store::read_protected(
            &fixture.ticket.request_path(),
            &fixture.ticket.request.owner_sid,
            LIMIT,
        )
        .unwrap(),
    )
    .unwrap();
    request.context = permit.context();
    request.binding = permit.binding().clone();
    request.cwd = store.root().to_owned();
    assert_eq!(
        request.context.owner.store_id,
        store.load().unwrap().store_id
    );
    publish(permit.package_request().unwrap().to_owned(), request).unwrap()
}

pub(crate) fn complete_fixture(ticket: &PackageTicket, created: bool) {
    if created {
        consume(
            &ticket.request_path(),
            Some("Fixture_publisher"),
            Some("Fixture_1_x64__publisher"),
            |_, _| Ok(()),
        )
        .unwrap();
    } else {
        let _gate = ticket.gate().unwrap();
        ticket.write(Phase::Consuming {}).unwrap();
    }
}

struct Fixture {
    ticket: PackageTicket,
    _store: Store,
    _temp: tempfile::TempDir,
}
impl Fixture {
    fn new(age: u64) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::create(&temp.path().join("store")).unwrap();
        let manifest = store.load().unwrap();
        let current = identity::current().unwrap();
        let context = DispatchIdentity {
            owner: ResourceOwner {
                store_id: manifest.store_id,
                attempt_id: Uuid::new_v4(),
                epoch: Uuid::new_v4(),
            },
            dispatch_id: Uuid::new_v4(),
        };
        let root = store
            .root()
            .join("state")
            .join(format!("package-{}", context.owner.attempt_id));
        let issued_at = now().unwrap() - age;
        let request = Request {
            version: 1,
            context,
            binding: LaunchBinding {
                dependency_digest: [1; 32],
                resource_key: [2; 32],
                executable: current.image_path.clone(),
                image: current.image_file.clone(),
                session_id: current.session_id,
                network: LaunchNetwork::Direct {},
            },
            owner_sid: current.user_sid.clone(),
            directory: current.image_file.clone(),
            helper: current.image_file.clone(),
            issuer: current,
            issued_tick: tick().saturating_sub(age * 1000),
            family: "Fixture_publisher".into(),
            full_name: "Fixture_1_x64__publisher".into(),
            issued_at,
            expires_at: issued_at + TTL,
            args: vec![
                "--exact".into(),
                "launching::package_launch::tests::application_child".into(),
                "--ignored".into(),
            ],
            cwd: temp.path().to_owned(),
            environment: EnvPatch::default(),
        };
        let ticket = publish(root, request).unwrap();
        Self {
            ticket,
            _store: store,
            _temp: temp,
        }
    }
    fn consume(&self) -> Result<()> {
        consume(
            &self.ticket.request_path(),
            Some("Fixture_publisher"),
            Some("Fixture_1_x64__publisher"),
            |child, _| {
                assert_eq!(child.package_full_name()?, None);
                Ok(())
            },
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Ok(PackageOutcome::Created(child)) = self.ticket.outcome() {
            let _ = process::terminate_exact(&child);
        }
    }
}

#[test]
fn repeated_activation_helpers_race_on_one_ticket_without_duplicate_creation() {
    let fixture = Fixture::new(0);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            let path = fixture.ticket.request_path();
            let family = fixture.ticket.request.family.clone();
            let full = fixture.ticket.request.full_name.clone();
            std::thread::spawn(move || {
                barrier.wait();
                consume(&path, Some(&family), Some(&full), |_, _| Ok(()))
            })
        })
        .collect();
    let mut successes = 0;
    for thread in threads {
        match thread.join().unwrap() {
            Ok(()) => successes += 1,
            Err(Error::Invalid(
                app_proxy_core::error_code::PACKAGE_REQUEST_BUSY
                | "PACKAGE_REQUEST_ALREADY_CONSUMED",
            )) => {}
            other => panic!("unexpected helper result: {other:?}"),
        }
    }
    assert_eq!(successes, 1);
    let PackageOutcome::Created(first) = fixture.ticket.outcome().unwrap() else {
        panic!("not created")
    };
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"))
    ));
    let PackageOutcome::Created(again) = fixture.ticket.outcome().unwrap() else {
        panic!("not created")
    };
    assert_eq!(first, again);
}

#[test]
fn revoked_request_cannot_be_consumed_even_after_reopen() {
    let fixture = Fixture::new(0);
    let PackageOutcome::NotCreated(proof) = fixture.ticket.revoke().unwrap() else {
        panic!()
    };
    assert_eq!(proof.context(), fixture.ticket.request.context);
    let reopened = PackageTicket::open(&fixture.ticket.root).unwrap();
    assert!(matches!(
        reopened.outcome().unwrap(),
        PackageOutcome::NotCreated(_)
    ));
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"))
    ));
    assert!(matches!(
        reopened.revoke().unwrap(),
        PackageOutcome::NotCreated(_)
    ));
}

#[test]
fn expired_or_wrong_package_helper_never_creates_an_application() {
    let fixture = Fixture::new(TTL + 1);
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid("PACKAGE_REQUEST_EXPIRED"))
    ));
    assert!(matches!(
        fixture.ticket.outcome().unwrap(),
        PackageOutcome::NotCreated(_)
    ));
    let fixture = Fixture::new(0);
    // A foreign/version-stale caller cannot even consume or revoke the pending request.
    for (family, full) in [
        (None, None),
        (Some("Fixture_publisher"), Some("Fixture_2_x64__publisher")),
    ] {
        assert!(matches!(
            consume(&fixture.ticket.request_path(), family, full, |_, _| Ok(())),
            Err(Error::Invalid("PACKAGE_HELPER_IDENTITY_MISMATCH"))
        ));
        assert!(matches!(
            fixture.ticket.outcome().unwrap(),
            PackageOutcome::Pending
        ));
    }
    let mut past = fixture.ticket.request.issued_at;
    past += 100;
    let mut request: Request = store::decode(
        &store::read_protected(
            &fixture.ticket.request_path(),
            &fixture.ticket.request.owner_sid,
            LIMIT,
        )
        .unwrap(),
    )
    .unwrap();
    request.issued_at = past;
    request.expires_at = past + TTL;
    assert!(matches!(
        check_deadline(&request, Instant::now()),
        Err(Error::Invalid("PACKAGE_REQUEST_EXPIRED"))
    ));
}

#[test]
fn gate_serializes_revocation_and_consumption_and_unknown_never_expires() {
    let fixture = Fixture::new(0);
    let gate = fixture.ticket.gate().unwrap();
    let other = PackageTicket::open(&fixture.ticket.root).unwrap();
    assert!(matches!(
        other.revoke(),
        Err(Error::Invalid(
            app_proxy_core::error_code::PACKAGE_REQUEST_BUSY
        ))
    ));
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid(
            app_proxy_core::error_code::PACKAGE_REQUEST_BUSY
        ))
    ));
    // Fault injection: the helper died after publishing its creation intent.
    fixture.ticket.write(Phase::Consuming {}).unwrap();
    drop(gate);
    assert!(matches!(
        other.revoke().unwrap(),
        PackageOutcome::Indeterminate
    ));
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"))
    ));
    assert!(matches!(
        PackageTicket::open(&fixture.ticket.root)
            .unwrap()
            .outcome()
            .unwrap(),
        PackageOutcome::Indeterminate
    ));
}

#[test]
fn receipt_is_bound_to_nonce_request_and_physical_directory() {
    let fixture = Fixture::new(0);
    let mut state: State = store::decode(
        &store::read_protected(
            &fixture.ticket.root.join("state.json"),
            &fixture.ticket.request.owner_sid,
            LIMIT,
        )
        .unwrap(),
    )
    .unwrap();
    state.context.dispatch_id = Uuid::new_v4();
    store::replace_protected(
        &fixture.ticket.root,
        &fixture.ticket.request.owner_sid,
        "state.json",
        &store::encode(&state, LIMIT).unwrap(),
        LIMIT,
    )
    .unwrap();
    assert!(matches!(
        fixture.ticket.outcome(),
        Err(Error::Invalid("PACKAGE_RECEIPT_BINDING_MISMATCH"))
    ));
    let copy = fixture._temp.path().join("copy");
    fs::create_dir(&copy).unwrap();
    let handle = security::directory(&copy, true).unwrap();
    security::protect(&handle, &fixture.ticket.request.owner_sid).unwrap();
    fs::copy(fixture.ticket.request_path(), copy.join("request.json")).unwrap();
    assert!(matches!(
        PackageTicket::open(&copy),
        Err(Error::Invalid("PACKAGE_REQUEST_DIRECTORY_MISMATCH"))
    ));
}

/// Children of a packaged desktop app inherit its package identity, so a run
/// started from such a host cannot observe an unpackaged fixture child.
fn packaged_host() -> bool {
    let packaged = matches!(identity::package_full_name(), Ok(Some(_)));
    if packaged {
        eprintln!("skipped: the test host has an MSIX package identity");
    }
    packaged
}

#[test]
fn successful_creation_is_once_only_and_late_cancel_preserves_exact_child() {
    if packaged_host() {
        return;
    }
    let fixture = Fixture::new(0);
    fixture.consume().unwrap();
    let PackageOutcome::Created(child) = fixture.ticket.outcome().unwrap() else {
        panic!()
    };
    assert!(process::is_running_exact(&child).unwrap());
    assert!(matches!(
        fixture.consume(),
        Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"))
    ));
    let PackageOutcome::Created(again) = fixture.ticket.revoke().unwrap() else {
        panic!()
    };
    assert_eq!(child, again);
    assert!(process::is_running_exact(&child).unwrap());
    process::terminate_exact(&child).unwrap();
    // The durable receipt remains historical after the exact process exits.
    let PackageOutcome::Created(recovered) = PackageTicket::open(&fixture.ticket.root)
        .unwrap()
        .outcome()
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(recovered, child);
}

#[test]
fn wall_clock_rollback_cannot_extend_capability_or_revive_old_issuer() {
    let fixture = Fixture::new(0);
    let request = &fixture.ticket.request;
    // UTC was rolled backwards into the original interval, but 25 real seconds elapsed.
    assert!(matches!(
        check_clock(
            request,
            request.issued_at + 10,
            request.issued_tick + 25000,
            Duration::ZERO
        ),
        Err(Error::Invalid("PACKAGE_REQUEST_EXPIRED"))
    ));
    assert!(matches!(
        check_clock(
            request,
            request.issued_at + 1,
            request.issued_tick.saturating_sub(1),
            Duration::ZERO
        ),
        Err(Error::Invalid("PACKAGE_REQUEST_EXPIRED"))
    ));
    let mut request: Request = store::decode(
        &store::read_protected(&fixture.ticket.request_path(), &request.owner_sid, LIMIT).unwrap(),
    )
    .unwrap();
    request.issuer.creation_time += 1;
    assert!(check_deadline(&request, Instant::now()).is_err());
}

#[test]
fn request_and_state_write_failures_never_publish_a_partial_capability() {
    for fail_state in [false, true] {
        let fixture = Fixture::new(0);
        let mut request: Request = store::decode(
            &store::read_protected(
                &fixture.ticket.request_path(),
                &fixture.ticket.request.owner_sid,
                LIMIT,
            )
            .unwrap(),
        )
        .unwrap();
        request.context.owner.attempt_id = Uuid::new_v4();
        request.context.dispatch_id = Uuid::new_v4();
        let context = request.context;
        let root = fixture
            ._store
            .root()
            .join("state")
            .join(format!("package-{}", context.owner.attempt_id));
        let mut held = None;
        let result = publish_with(root.clone(), request, |stage| {
            let name = if fail_state {
                "state.json"
            } else {
                "request.json"
            };
            fs::write(stage.join(name), b"{}").unwrap();
            held = Some(
                OpenOptions::new()
                    .read(true)
                    .share_mode(FILE_SHARE_READ)
                    .open(stage.join(name))
                    .unwrap(),
            );
            Ok(())
        });
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("fault did not block publication"),
        };
        assert!(!root.exists());
        let SpawnFailure::NotCreated { evidence, .. } = publication_failure(&root, context, error)
        else {
            panic!()
        };
        assert_eq!(evidence.context(), context);
        drop(held);
    }
    // A colliding final directory is never replaced, nor reported absent.
    let fixture = Fixture::new(0);
    let request: Request = store::decode(
        &store::read_protected(
            &fixture.ticket.request_path(),
            &fixture.ticket.request.owner_sid,
            LIMIT,
        )
        .unwrap(),
    )
    .unwrap();
    assert!(publish(fixture.ticket.root.clone(), request).is_err());
    assert!(matches!(
        fixture.ticket.outcome().unwrap(),
        PackageOutcome::Pending
    ));
}

#[test]
fn child_identity_or_receipt_failure_after_creation_preserves_unknown_without_retry() {
    for block_receipt in [false, true] {
        let fixture = Fixture::new(0);
        let mut child = None;
        let mut held = None;
        let result = consume(
            &fixture.ticket.request_path(),
            Some("Fixture_publisher"),
            Some("Fixture_1_x64__publisher"),
            |process, _| {
                child = Some(process.identity.clone());
                if block_receipt {
                    held = Some(
                        OpenOptions::new()
                            .read(true)
                            .share_mode(FILE_SHARE_READ)
                            .open(fixture.ticket.root.join("state.json"))
                            .unwrap(),
                    );
                    Ok(())
                } else {
                    Err(Error::Invalid("PACKAGE_CHILD_IDENTITY_UNCONFIRMED"))
                }
            },
        );
        assert!(result.is_err());
        drop(held);
        let child = child.unwrap();
        assert!(process::is_running_exact(&child).unwrap());
        assert!(matches!(
            fixture.ticket.revoke().unwrap(),
            PackageOutcome::Indeterminate
        ));
        assert!(matches!(
            fixture.consume(),
            Err(Error::Invalid("PACKAGE_REQUEST_ALREADY_CONSUMED"))
        ));
        process::terminate_exact(&child).unwrap();
    }
}

#[test]
#[ignore = "application fixture invoked only by package request tests"]
fn application_child() {
    std::thread::sleep(Duration::from_secs(30));
}
