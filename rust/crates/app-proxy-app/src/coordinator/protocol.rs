//! Wire types of the coordinator protocol and the error-code bridge.
//! Payload types come from the crates that own them; this module only frames them.
use super::*;

/// The console program, the host and the coordinator ship and are replaced as a
/// set, so there is no negotiation below this number: any change to the wire
/// format raises it, and a mismatch is refused during the handshake.
pub(super) const PROTOCOL_MAJOR: u32 = 4;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Hello {
    pub(super) protocol_major: u32,
    pub(super) version: String,
    pub(super) store_id: Uuid,
    pub(super) session_id: u32,
    pub(super) epoch: Option<Uuid>,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Welcome {
    Ready { hello: Hello },
    Rejected { code: String },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub(super) protocol_major: u32,
    pub(super) request_id: Uuid,
    pub(super) operation: Operation,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Operation {
    LoginApply {
        request: app_proxy_windows::guard_task::login::journal::Request,
    },
    LoginResume {
        request_id: Uuid,
    },
    LoginRequest {
        request_id: Uuid,
    },
    LoginStatus {},
    InstanceSettings {
        instance_id: Uuid,
    },
    ShortcutApply {
        request: app_proxy_windows::shortcuts::journal::Request,
    },
    ShortcutResume {
        request_id: Uuid,
    },
    ShortcutRequest {
        request_id: Uuid,
    },
    ShortcutStatus {
        instance_id: Uuid,
    },
    ShortcutCheck {
        instance_id: Uuid,
    },
    RuntimeStatus {
        instance_id: Uuid,
    },
    SubscriptionNodes {
        profile_id: Uuid,
        offset: usize,
        expected_revision: Option<u64>,
    },
    SubscriptionPreview {
        id: Uuid,
        request: crate::subscription_preview::PreviewRequest,
    },
    SubscriptionPreviewPage {
        id: Uuid,
        offset: usize,
    },
    SubscriptionPreviewClose {
        id: Uuid,
    },
    SubscriptionStage {
        preview_id: Uuid,
        stage_id: Uuid,
        request: crate::subscription_preview::StageRequest,
    },
    Status {},
    Catalog {
        offset: usize,
        expected_revision: Option<u64>,
    },
    Configure {
        expected_revision: u64,
        action: ConfigAction,
    },
    RequestStatus {
        request_id: Uuid,
    },
    ControlCore {
        action: CoreAction,
    },
    CoreRequestStatus {
        request_id: Uuid,
    },
    CoreStatus {},
    Launch {
        instance_id: Uuid,
        origin: LaunchOrigin,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_revision: Option<u64>,
    },
    LaunchStatus {
        request_id: Uuid,
    },
    CancelLaunch {
        request_id: Uuid,
    },
    GuardStatus {
        instance_id: Uuid,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Response {
    pub(super) request_id: Uuid,
    pub(super) epoch: Uuid,
    pub(super) result: Reply,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Reply {
    LoginRequest {
        status: Option<app_proxy_windows::guard_task::login::journal::Status>,
    },
    LoginStatus {
        status: crate::login_tasks::View,
    },
    InstanceSettings {
        settings: crate::instance_settings::Summary,
    },
    ShortcutRequest {
        status: Option<app_proxy_windows::shortcuts::journal::Status>,
    },
    ShortcutStatus {
        status: crate::shortcuts::InstanceStatus,
    },
    ShortcutCheck {
        check: app_proxy_windows::shortcuts::journal::Check,
    },
    RuntimeStatus {
        status: Box<crate::launch_engine::RuntimeStatus>,
    },
    SubscriptionNodes {
        page: crate::subscription_preview::SavedPage,
    },
    SubscriptionPreview {
        page: crate::subscription_preview::PreviewPage,
    },
    SubscriptionStaged {
        staged: Box<app_proxy_windows::subscription_stage::StagedSubscription>,
    },
    SubscriptionPreviewClosed {},
    Status {
        status: Status,
    },
    Catalog {
        page: CatalogPage,
    },
    Configured {
        outcome: ConfigOutcome,
    },
    RequestStatus {
        status: Option<ConfigRequestStatus>,
    },
    CoreRequestStatus {
        status: Option<CoreRequestStatus>,
    },
    CoreStatus {
        snapshot: crate::core_manager::CoreSnapshot,
    },
    LaunchStatus {
        attempt: Option<LaunchAttempt>,
    },
    GuardStatus {
        status: Box<crate::guard_control::GuardStatus>,
    },
    Error {
        code: String,
    },
}

pub(super) fn safe_error(error: Error) -> String {
    match error {
        Error::Invalid(code) => code.into(),
        Error::Windows {
            operation: "ShortcutCom",
            code,
        } => format!("SHORTCUT_COM_ERROR:{code}"),
        _ => "COORDINATOR_OPERATION_FAILED".into(),
    }
}

/// Inverse of [`safe_error`]: every stable code the server sends stays
/// matchable by callers. Interning replaces a second, hand-maintained list of
/// codes that silently collapsed any newer code into the generic failure.
pub(super) fn remote_error(code: &str) -> Error {
    if let Some(value) = code.strip_prefix("SHORTCUT_COM_ERROR:")
        && let Ok(code) = value.parse::<u32>()
    {
        return Error::Windows {
            operation: "ShortcutCom",
            code,
        };
    }
    Error::Invalid(
        app_proxy_core::error_code::intern(code).unwrap_or("COORDINATOR_OPERATION_FAILED"),
    )
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Status {
    pub store_id: Uuid,
    pub revision: u64,
    pub epoch: Uuid,
    pub coordinator_pid: u32,
    pub session_id: u32,
    pub applications: usize,
    pub instances: usize,
    pub profiles: usize,
    pub phase: String,
}

pub(super) fn hello(store_id: Uuid, session_id: u32, epoch: Option<Uuid>) -> Hello {
    Hello {
        protocol_major: PROTOCOL_MAJOR,
        version: env!("CARGO_PKG_VERSION").into(),
        store_id,
        session_id,
        epoch,
    }
}
