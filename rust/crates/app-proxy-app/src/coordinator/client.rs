//! Client side: find or start the coordinator of a store and perform one
//! authenticated request per connection.
use super::protocol::*;
use super::*;

pub(super) async fn query(
    store_id: Uuid,
    policy: &ipc::PeerPolicy,
    wait: Duration,
) -> Result<Status> {
    let request = Request {
        protocol_major: PROTOCOL_MAJOR,
        request_id: Uuid::new_v4(),
        operation: Operation::Status {},
    };
    match rpc(store_id, policy, wait, request).await? {
        Reply::Status { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub(super) async fn rpc(
    store_id: Uuid,
    policy: &ipc::PeerPolicy,
    wait: Duration,
    request: Request,
) -> Result<Reply> {
    match deliver(store_id, policy, wait, &request).await {
        None => Err(Error::Invalid(
            app_proxy_core::error_code::IPC_CONNECT_TIMEOUT,
        )),
        Some(result) => result,
    }
}

/// `None` means the pipe could not be opened, which is the only outcome that
/// proves the request never left this process. `Some` means delivery was
/// attempted; on error the outcome is queried by request ID. The distinction is
/// made locally and not from an error code, because a code received from the
/// peer can spell any well-formed name.
type Delivery = Option<Result<Reply>>;

async fn deliver(
    store_id: Uuid,
    policy: &ipc::PeerPolicy,
    wait: Duration,
    request: &Request,
) -> Delivery {
    match ipc::connect(store_id, policy, wait).await {
        Ok(connection) => Some(exchange(connection, store_id, policy, request).await),
        Err(Error::Invalid(app_proxy_core::error_code::IPC_CONNECT_TIMEOUT)) => None,
        Err(error) => Some(Err(error)),
    }
}

async fn exchange(
    mut connection: ipc::Connection<tokio::net::windows::named_pipe::NamedPipeClient>,
    store_id: Uuid,
    policy: &ipc::PeerPolicy,
    request: &Request,
) -> Result<Reply> {
    connection
        .send(&hello(store_id, policy.session_id, None))
        .await?;
    let welcome: Welcome = connection.receive().await?;
    let server = match welcome {
        Welcome::Ready { hello } => hello,
        Welcome::Rejected { code } => {
            return Err(Error::Invalid(match code.as_str() {
                "PROTOCOL_VERSION_MISMATCH" => "PROTOCOL_VERSION_MISMATCH",
                "STORE_SESSION_CONFLICT" => "STORE_SESSION_CONFLICT",
                "STORE_ID_MISMATCH" => "STORE_ID_MISMATCH",
                _ => "IPC_HANDSHAKE_REJECTED",
            }));
        }
    };
    if server.protocol_major != PROTOCOL_MAJOR {
        return Err(Error::Invalid("PROTOCOL_VERSION_MISMATCH"));
    }
    if server.store_id != store_id
        || server.session_id != policy.session_id
        || server.epoch.is_none()
    {
        return Err(Error::Invalid("IPC_SERVER_HELLO_MISMATCH"));
    }
    let request_id = request.request_id;
    connection.send(request).await?;
    let response: Response = connection.receive().await?;
    if response.request_id != request_id || Some(response.epoch) != server.epoch {
        return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
    }
    if let Reply::Status { status } = &response.result
        && (status.store_id != store_id
            || Some(status.epoch) != server.epoch
            || status.coordinator_pid != connection.peer.pid
            || status.session_id != connection.peer.session_id)
    {
        return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
    }
    if let Reply::Error { code } = &response.result {
        return Err(remote_error(code));
    }
    Ok(response.result)
}

/// On failure retain the same request ID and query its status; never manufacture
/// a fresh ID to retry an operation whose response may have been lost.
pub async fn configure(root: PathBuf, request: ConfigRequest) -> Result<ConfigOutcome> {
    let operation = Operation::Configure {
        expected_revision: request.expected_revision,
        action: request.action,
    };
    match client_operation(root, request.request_id, operation).await? {
        Reply::Configured { outcome } => Ok(outcome),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn instance_settings(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::instance_settings::Summary> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::InstanceSettings { instance_id },
    )
    .await?
    {
        Reply::InstanceSettings { settings } if settings.instance_id == instance_id => Ok(settings),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn login_apply(
    root: PathBuf,
    request: app_proxy_windows::guard_task::login::journal::Request,
) -> Result<app_proxy_windows::guard_task::login::journal::Status> {
    store::describe(&root)?;
    match client_operation(root, request.id, Operation::LoginApply { request }).await? {
        Reply::LoginRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn login_resume(
    root: PathBuf,
    request_id: Uuid,
) -> Result<app_proxy_windows::guard_task::login::journal::Status> {
    store::describe(&root)?;
    match client_operation(root, Uuid::new_v4(), Operation::LoginResume { request_id }).await? {
        Reply::LoginRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn login_request(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<app_proxy_windows::guard_task::login::journal::Status>> {
    store::describe(&root)?;
    match client_operation(root, Uuid::new_v4(), Operation::LoginRequest { request_id }).await? {
        Reply::LoginRequest { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn login_status(root: PathBuf) -> Result<crate::login_tasks::View> {
    store::describe(&root)?;
    match client_operation(root, Uuid::new_v4(), Operation::LoginStatus {}).await? {
        Reply::LoginStatus { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn shortcut_apply(
    root: PathBuf,
    request: app_proxy_windows::shortcuts::journal::Request,
) -> Result<app_proxy_windows::shortcuts::journal::Status> {
    store::describe(&root)?;
    match client_operation(root, request.id, Operation::ShortcutApply { request }).await? {
        Reply::ShortcutRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_resume(
    root: PathBuf,
    request_id: Uuid,
) -> Result<app_proxy_windows::shortcuts::journal::Status> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutResume { request_id },
    )
    .await?
    {
        Reply::ShortcutRequest {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_request(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<app_proxy_windows::shortcuts::journal::Status>> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutRequest { request_id },
    )
    .await?
    {
        Reply::ShortcutRequest { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn shortcut_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::shortcuts::InstanceStatus> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutStatus { instance_id },
    )
    .await?
    {
        Reply::ShortcutStatus { status } if status.instance_id == instance_id => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn shortcut_check(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<app_proxy_windows::shortcuts::journal::Check> {
    store::describe(&root)?;
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::ShortcutCheck { instance_id },
    )
    .await?
    {
        Reply::ShortcutCheck { check } if check.instance_id == instance_id => Ok(check),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn subscription_preview(
    root: PathBuf,
    id: Uuid,
    request: crate::subscription_preview::PreviewRequest,
) -> Result<crate::subscription_preview::PreviewPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreview { id, request },
    )
    .await?
    {
        Reply::SubscriptionPreview { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn subscription_nodes(
    root: PathBuf,
    profile_id: Uuid,
    offset: usize,
    expected_revision: Option<u64>,
) -> Result<crate::subscription_preview::SavedPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionNodes {
            profile_id,
            offset,
            expected_revision,
        },
    )
    .await?
    {
        Reply::SubscriptionNodes { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_preview_page(
    root: PathBuf,
    id: Uuid,
    offset: usize,
) -> Result<crate::subscription_preview::PreviewPage> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreviewPage { id, offset },
    )
    .await?
    {
        Reply::SubscriptionPreview { page } => Ok(page),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_preview_close(root: PathBuf, id: Uuid) -> Result<()> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionPreviewClose { id },
    )
    .await?
    {
        Reply::SubscriptionPreviewClosed {} => Ok(()),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}
pub async fn subscription_stage(
    root: PathBuf,
    preview_id: Uuid,
    stage_id: Uuid,
    request: crate::subscription_preview::StageRequest,
) -> Result<app_proxy_windows::subscription_stage::StagedSubscription> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::SubscriptionStage {
            preview_id,
            stage_id,
            request,
        },
    )
    .await?
    {
        Reply::SubscriptionStaged { staged } => Ok(*staged),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn request_status(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<ConfigRequestStatus>> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::RequestStatus { request_id },
    )
    .await?
    {
        Reply::RequestStatus { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

/// Keep this ID if the response is lost; query it rather than submitting anew.
pub async fn control_core(
    root: PathBuf,
    request_id: Uuid,
    action: CoreAction,
) -> Result<CoreRequestStatus> {
    match client_operation(root, request_id, Operation::ControlCore { action }).await? {
        Reply::CoreRequestStatus {
            status: Some(status),
        } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn core_request_status(
    root: PathBuf,
    request_id: Uuid,
) -> Result<Option<CoreRequestStatus>> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::CoreRequestStatus { request_id },
    )
    .await?
    {
        Reply::CoreRequestStatus { status } => Ok(status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn core_status(root: PathBuf) -> Result<crate::core_manager::CoreSnapshot> {
    match client_operation(root, Uuid::new_v4(), Operation::CoreStatus {}).await? {
        Reply::CoreStatus { snapshot } => Ok(snapshot),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

/// Keep the request ID across transport failures. Submission acknowledges the
/// durable attempt; only its queried phase describes the launch result.
pub async fn launch(root: PathBuf, request: LaunchRequest) -> Result<LaunchAttempt> {
    launch_at_revision(root, request, None).await
}

pub(crate) async fn launch_at_revision(
    root: PathBuf,
    request: LaunchRequest,
    expected_revision: Option<u64>,
) -> Result<LaunchAttempt> {
    match client_operation(
        root,
        request.request_id,
        Operation::Launch {
            instance_id: request.instance_id,
            origin: request.origin,
            expected_revision,
        },
    )
    .await?
    {
        Reply::LaunchStatus {
            attempt: Some(attempt),
        } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn launch_status(root: PathBuf, request_id: Uuid) -> Result<Option<LaunchAttempt>> {
    match client_operation(root, Uuid::new_v4(), Operation::LaunchStatus { request_id }).await? {
        Reply::LaunchStatus { attempt } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn cancel_launch(root: PathBuf, request_id: Uuid) -> Result<LaunchAttempt> {
    match client_operation(root, Uuid::new_v4(), Operation::CancelLaunch { request_id }).await? {
        Reply::LaunchStatus {
            attempt: Some(attempt),
        } => Ok(attempt),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn runtime_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::launch_engine::RuntimeStatus> {
    match client_operation(
        root,
        Uuid::new_v4(),
        Operation::RuntimeStatus { instance_id },
    )
    .await?
    {
        Reply::RuntimeStatus { status } if status.instance_id == instance_id => Ok(*status),
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn guard_status(
    root: PathBuf,
    instance_id: Uuid,
) -> Result<crate::guard_control::GuardStatus> {
    match client_operation(root, Uuid::new_v4(), Operation::GuardStatus { instance_id }).await? {
        Reply::GuardStatus { status }
            if status.instance_id == instance_id
                && status.scan.as_ref().is_none_or(|s| {
                    s.instance_id == instance_id && s.revision == status.revision
                }) =>
        {
            Ok(*status)
        }
        _ => Err(Error::Invalid("IPC_RESPONSE_MISMATCH")),
    }
}

pub async fn catalog(root: PathBuf) -> Result<CatalogPage> {
    let mut offset = 0;
    let mut combined: Option<CatalogPage> = None;
    loop {
        let expected_revision = combined.as_ref().map(|c| c.revision);
        let Reply::Catalog { mut page } = client_operation(
            root.clone(),
            Uuid::new_v4(),
            Operation::Catalog {
                offset,
                expected_revision,
            },
        )
        .await?
        else {
            return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
        };
        let next = page.next_offset;
        if let Some(all) = &mut combined {
            if page.revision != all.revision {
                return Err(Error::Invalid("CATALOG_CHANGED"));
            }
            all.applications.append(&mut page.applications);
            all.instances.append(&mut page.instances);
            all.profiles.append(&mut page.profiles);
        } else {
            page.next_offset = None;
            combined = Some(page);
        }
        let Some(next) = next else {
            return Ok(combined.expect("first page inserted"));
        };
        if next <= offset {
            return Err(Error::Invalid("IPC_RESPONSE_MISMATCH"));
        }
        offset = next;
    }
}

pub(super) async fn client_operation(
    root: PathBuf,
    request_id: Uuid,
    operation: Operation,
) -> Result<Reply> {
    identity::assert_ordinary_user()?;
    ensure_store(&root).await?;
    let descriptor = store::describe(&root)?;
    let (_, host) = binaries()?;
    let policy = ipc::PeerPolicy::current(vec![identity::file_identity(&host)?])?;
    let request = Request {
        protocol_major: PROTOCOL_MAJOR,
        request_id,
        operation,
    };
    // A running coordinator answers on the first connection. Only when nobody
    // is listening does the client go through discovery and startup, after
    // which the same, still unsent request is delivered.
    if let Some(result) = deliver(
        descriptor.store_id,
        &policy,
        Duration::from_millis(40),
        &request,
    )
    .await
    {
        return result;
    }
    let owner = status(root).await?;
    match deliver(owner.store_id, &policy, Duration::from_secs(1), &request).await {
        None => Err(Error::Invalid(
            app_proxy_core::error_code::IPC_CONNECT_TIMEOUT,
        )),
        Some(result) => result,
    }
}

/// Launch-on-demand status flow; no arbitrary commands or target processes are accepted.
pub async fn status(root: PathBuf) -> Result<Status> {
    identity::assert_ordinary_user()?;
    ensure_store(&root).await?;
    let descriptor = store::describe(&root)?;
    let (_, host) = binaries()?;
    let policy = ipc::PeerPolicy::current(vec![identity::file_identity(&host)?])?;
    match query(descriptor.store_id, &policy, Duration::from_millis(40)).await {
        Ok(status) => return Ok(status),
        Err(Error::Invalid(app_proxy_core::error_code::IPC_CONNECT_TIMEOUT)) => {}
        Err(error) => return Err(error),
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let _startup = loop {
        if let Some(lock) = store::try_startup_lock(&root)? {
            break lock;
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("COORDINATOR_START_TIMEOUT"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    match query(descriptor.store_id, &policy, Duration::from_millis(40)).await {
        Ok(status) => return Ok(status),
        Err(Error::Invalid(app_proxy_core::error_code::IPC_CONNECT_TIMEOUT)) => {}
        Err(error) => return Err(error),
    }
    // Validate configuration before creating a background process. A held owner lock
    // is never interpreted as permission to kill or replace the existing owner.
    drop(store::Store::open(&root)?);
    let child = process::start_host(&host, &root)?;
    let result = query(descriptor.store_id, &policy, Duration::from_secs(5)).await;
    if result.is_err() && child.has_exited()? {
        return Err(Error::Invalid("COORDINATOR_START_FAILED"));
    }
    // Dropping our process handle does not terminate the coordinator or applications.
    result
}

pub(super) async fn ensure_store(root: &Path) -> Result<()> {
    let attempted_create = !root.try_exists()? || std::fs::read_dir(root)?.next().is_none();
    if attempted_create {
        match store::Store::create(root) {
            Ok(store) => {
                drop(store);
                return Ok(());
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(Error::Invalid(
                app_proxy_core::error_code::STORE_NOT_EMPTY
                | app_proxy_core::error_code::STORE_ALREADY_OWNED,
            )) => {}
            Err(error) => return Err(error),
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while root.join(".initializing").try_exists()?
        || (attempted_create && !root.join(".app-proxy-rust-owned.json").try_exists()?)
    {
        if tokio::time::Instant::now() >= deadline {
            return Err(Error::Invalid("STORE_INITIALIZATION_INCOMPLETE"));
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    store::describe(root)?;
    Ok(())
}
