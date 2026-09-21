use crate::{
    coordinator,
    exit::{self, Failure, fail},
    guard_control::{ComponentState, GuardPhase, GuardStatus},
    instance_cli::submit,
};
use app_proxy_core::{model::Desired, registry::ConfigAction};
use clap::Subcommand;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::PathBuf;
use uuid::Uuid;

fn normalize_listener_wait(status: &mut GuardStatus) {
    if status.desired == Desired::Enabled
        && matches!(
            status.diagnostic.as_deref(),
            Some("GUARD_RESOLUTION_BUSY" | "GUARD_SCAN_BUSY" | "GUARD_LISTENER_CHECK_BUSY")
        )
    {
        status.phase = GuardPhase::Starting;
    }
    // Unverified + a cached missing/start marker means the RPC's independent
    // deployment check passed, but the monitor has not connected yet. A truly
    // missing deployment is NeedsAuthorization and must never be hidden.
    if status.listener == ComponentState::Unverified
        && matches!(
            status.diagnostic.as_deref(),
            Some(
                "GUARD_LISTENER_MISSING"
                    | "GUARD_TASK_MISSING"
                    | "GUARD_LISTENER_START_PENDING"
                    | "GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED"
            )
        )
    {
        status.phase = GuardPhase::Starting;
        status.diagnostic = Some("GUARD_LISTENER_START_PENDING".into());
    }
}

fn listener_registered(status: &GuardStatus) -> bool {
    matches!(
        status.listener,
        ComponentState::ActiveEtw | ComponentState::ActivePolling
    ) || matches!(
        status.diagnostic.as_deref(),
        Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED" | "GUARD_LISTENER_START_PENDING")
    )
}

fn settling(status: &GuardStatus) -> bool {
    status.phase == GuardPhase::Starting
        || matches!(
            status.diagnostic.as_deref(),
            Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED" | "GUARD_LISTENER_START_PENDING")
        )
}

async fn settle_status<F, Fut>(
    mut status: GuardStatus,
    foreground: &mut crate::foreground::Foreground,
    mut poll: F,
) -> Result<GuardStatus, Failure>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = app_proxy_windows::Result<GuardStatus>>,
{
    normalize_listener_wait(&mut status);
    let revision = status.revision;
    // Covers the resident monitor's 30-second retry and a bounded connection.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(40);
    while settling(&status) {
        tokio::select! {
            biased;
            _ = foreground.cancelled() => return Err(fail(exit::ACTION_REQUIRED, "已停止等待；实例和已登记的登录自启动均保留。")),
            _ = tokio::time::sleep_until(deadline) => break,
            _ = tokio::time::sleep(std::time::Duration::from_millis(500)) => {}
        }
        let next = tokio::select! {
            biased;
            _ = foreground.cancelled() => return Err(fail(exit::ACTION_REQUIRED, "已停止等待；实例和已登记的登录自启动均保留。")),
            result = tokio::time::timeout_at(deadline, poll()) => result,
        };
        match next {
            Ok(Ok(current)) => {
                status = current;
                normalize_listener_wait(&mut status);
            }
            _ => break,
        }
        if status.revision != revision || status.desired != Desired::Enabled {
            break;
        }
    }
    Ok(status)
}

#[derive(Subcommand)]
pub enum Command {
    /// 查看或恢复本数据目录的登录启动入口
    Login {
        #[command(subcommand)]
        command: crate::login_cli::Command,
    },
    /// 查看保护状态和进程检查；已启用的自动保护继续运行
    Status { id: Uuid },
    /// 保存启用意图；所需组件未授权时报告保护未完成
    Enable { id: Uuid },
    /// 关闭实例保护
    Disable { id: Uuid },
}

#[derive(Serialize)]
struct Report<'a> {
    request_id: Option<Uuid>,
    receipt: Option<app_proxy_core::registry::ConfigReceipt>,
    status: Option<&'a GuardStatus>,
    login: Option<crate::login_tasks::View>,
    login_operation: Option<crate::login_cli::Outcome>,
    requires_action: Option<&'static str>,
}

pub(crate) fn component(value: ComponentState) -> &'static str {
    match value {
        ComponentState::NotApplicable => "不适用",
        ComponentState::NeedsAuthorization => "待授权安装",
        ComponentState::Unverified => "组件待核验或等待监听",
        ComponentState::ActiveEtw => "事件监听运行中",
        ComponentState::ActivePolling => "轮询检查运行中",
    }
}

async fn login_view(root: PathBuf) -> crate::login_tasks::View {
    coordinator::login_status(root)
        .await
        .unwrap_or_else(|error| {
            let code = match error {
                app_proxy_windows::Error::Invalid(code) => code,
                _ => "GUARD_LOGIN_CHECK_UNCONFIRMED",
            };
            crate::login_tasks::View {
                revision: 0,
                integration: None,
                ready: false,
                diagnostic: Some(code.into()),
            }
        })
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    run_with_foreground(
        root,
        command,
        json,
        &mut crate::foreground::Foreground::new(),
        None,
    )
    .await
    .map(|_| ())
}

pub(crate) async fn run_with_foreground(
    root: PathBuf,
    command: Command,
    json: bool,
    foreground: &mut crate::foreground::Foreground,
    expected_revision: Option<u64>,
) -> Result<Option<GuardStatus>, Failure> {
    foreground.check()?;
    if let Command::Login { command } = command {
        return crate::login_cli::run(root, command, json)
            .await
            .map(|_| None);
    }
    let (id, desired) = match command {
        Command::Status { id } => (id, None),
        Command::Enable { id } => (id, Some(Desired::Enabled)),
        Command::Disable { id } => (id, Some(Desired::Disabled)),
        Command::Login { .. } => unreachable!(),
    };
    // A lightweight protocol check precedes enabling. Login COM diagnostics
    // cannot prevent a current protection query or disabling protection.
    if desired == Some(Desired::Enabled) {
        coordinator::login_request(root.clone(), Uuid::new_v4())
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    }
    // Check protocol support before changing configuration with an older host.
    let mut status = coordinator::guard_status(root.clone(), id)
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    if expected_revision.is_some_and(|revision| revision != status.revision) {
        return Err(fail(exit::CONFLICT, "配置已变化，请重新确认保护设置。"));
    }
    let mut confirmed_revision = status.revision;
    let mut request_id = None;
    let mut receipt = None;
    let mut installed_now = false;
    if let Some(desired) = desired
        && status.desired != desired
    {
        foreground.check()?;
        let catalog = coordinator::catalog(root.clone())
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
        if confirmed_revision != catalog.revision {
            return Err(fail(exit::CONFLICT, "配置已变化，请重新确认保护设置。"));
        }
        foreground.check()?;
        let instance = catalog
            .instances
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| fail(exit::INVALID, "INSTANCE_NOT_FOUND"))?;
        let (request, applied) = submit(
            &root,
            catalog.revision,
            ConfigAction::BindInstance {
                instance_id: id,
                network: instance.network,
                guard: Some(desired),
            },
            json,
        )
        .await?;
        request_id = Some(request);
        confirmed_revision = applied.revision;
        receipt = Some(applied);
        match coordinator::guard_status(root.clone(), id).await {
            Ok(current) => status = current,
            Err(_) => {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&Report {
                            request_id,
                            receipt,
                            status: None,
                            login: None,
                            login_operation: None,
                            requires_action: Some("query_guard_status"),
                        })
                        .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?
                    );
                }
                return Err(fail(
                    exit::UNCONFIRMED,
                    "配置回执已保存；保护状态暂未确认，请查询 guard status，不要重复配置请求。",
                ));
            }
        }
    }
    if confirmed_revision != status.revision {
        return Err(fail(
            exit::CONFLICT,
            "保护设置已保存，但配置随后发生变化；请重新确认。",
        ));
    }
    if desired == Some(Desired::Enabled)
        && status.listener == ComponentState::NeedsAuthorization
        && !json
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        foreground.check()?;
        println!(
            "缺少进程监听组件，需要 Windows 管理员授权，安装位置自动选择。\n1. 安装监听组件\n2. 暂不安装（保留实例配置）\n组件就绪后会自动检查已启用保护的实例，关闭并纠正未按要求使用代理的误启动主进程。"
        );
        let answer = foreground.read_line().await?;
        if answer.trim() == "1" {
            foreground.check()?;
            let catalog = coordinator::catalog(root.clone())
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            if catalog.revision != status.revision
                || !catalog
                    .instances
                    .iter()
                    .any(|i| i.id == id && i.guard == Desired::Enabled)
            {
                return Err(fail(
                    exit::ACTION_REQUIRED,
                    "配置已变化，未发起授权；请重新查看 guard status。",
                ));
            }
            let store = coordinator::status(root.clone())
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?
                .store_id;
            foreground.check()?;
            println!("等待 Windows 授权及组件核验；取消 UAC 会保留当前实例配置。");
            let (sender, result) = tokio::sync::oneshot::channel();
            // A native consent/COM call cannot be cancelled by dropping an async
            // future. A separate thread lets this foreground process exit and
            // invalidate its issuer identity without waiting for Tokio shutdown.
            std::thread::Builder::new()
                .name("guard-authorize".into())
                .spawn(move || {
                    let _ =
                        sender.send(app_proxy_windows::guard_install::authorize_listener(store));
                })
                .map_err(|_| fail(exit::UNCONFIRMED, "未能启动授权流程。"))?;
            let installed = tokio::select! {
                biased;
                _ = foreground.cancelled() => return Err(fail(exit::UNCONFIRMED, "已停止等待授权。若 Windows 授权窗口仍在，请选择取消；已提交的安装结果未确认，请先查询 guard status，不要重复安装。")),
                result = result => result.map_err(|_| fail(exit::UNCONFIRMED, "组件安装结果不明，请先查询 guard status，不要重复安装。"))?,
            };
            match installed {
                Ok(_) => {
                    installed_now = true;
                    println!("监听组件已安装，正在连接…")
                }
                Err(app_proxy_windows::Error::Invalid("GUARD_INSTALL_CANCELLED")) => {
                    return Err(fail(
                        exit::ACTION_REQUIRED,
                        "已取消 Windows 授权，实例配置保留。",
                    ));
                }
                Err(error) => {
                    return Err(fail(
                        exit::UNCONFIRMED,
                        format!(
                            "监听组件安装未确认：{error}。请先查询 guard status；不会自动重试。"
                        ),
                    ));
                }
            }
            foreground.check()?;
            status = coordinator::guard_status(root.clone(), id)
                .await
                .map_err(|e| fail(exit::UNCONFIRMED, e.to_string()))?;
        }
    }
    if confirmed_revision != status.revision {
        return Err(fail(
            exit::CONFLICT,
            "配置已变化；未继续登记登录入口，请重新确认保护设置。",
        ));
    }
    normalize_listener_wait(&mut status);
    if desired == Some(Desired::Enabled)
        && !installed_now
        && !listener_registered(&status)
        && settling(&status)
    {
        if !json {
            println!("正在确认监听组件…");
        }
        status = settle_status(status, foreground, || {
            coordinator::guard_status(root.clone(), id)
        })
        .await?;
        if status.revision != confirmed_revision {
            return Err(fail(exit::CONFLICT, "配置已变化，请重新确认保护设置。"));
        }
    }
    let mut login = login_view(root.clone()).await;
    let mut login_operation = None;
    if desired == Some(Desired::Enabled) {
        foreground.check()?;
        if login.revision != 0 && login.revision != confirmed_revision {
            return Err(fail(exit::CONFLICT, "配置已变化，请重新确认保护设置。"));
        }
        // Installation authority is independent of the first event heartbeat.
        // Native login admission re-verifies the deployment before registering.
        let listener_verified = installed_now || listener_registered(&status);
        if listener_verified && login.integration.is_none() && login.diagnostic.is_none() {
            if !json {
                println!("正在登记登录自启动…");
            }
            use app_proxy_windows::guard_task::login::journal::{Action, Request, Status};
            let request = Request {
                id: Uuid::new_v4(),
                expected_revision: status.revision,
                action: Action::Create,
                expected_creation: None,
            };
            let outcome =
                crate::login_cli::perform(&root, request.id, Some(request), foreground).await?;
            let revision = match &outcome.status {
                Some(Status::Created { revision }) => Some(*revision),
                _ => None,
            };
            if let Some(revision) = revision {
                confirmed_revision = revision;
            }
            login_operation = Some(outcome);
            // Preserve the request even when verification or the foreground wait
            // ends. A later explicit query/resume reconciles the native operation.
            if foreground.check().is_ok() {
                login = login_view(root.clone()).await;
                if let Some(revision) = revision
                    && let Ok(current) = coordinator::guard_status(root.clone(), id).await
                    && current.revision == revision
                {
                    status = current;
                }
            }
        }
    }
    if desired == Some(Desired::Enabled) && status.revision == confirmed_revision {
        normalize_listener_wait(&mut status);
        if settling(&status) && !json {
            println!("正在确认监听和保护状态…");
        }
        status = settle_status(status, foreground, || {
            coordinator::guard_status(root.clone(), id)
        })
        .await?;
    }
    let changed = status.revision != confirmed_revision
        || (login.revision != 0 && login.revision != confirmed_revision);
    if changed {
        login.ready = false;
        login.diagnostic = Some("GUARD_LOGIN_STATE_CHANGED".into());
    }
    let requires_action = if changed {
        Some("refresh_guard_status")
    } else {
        status.phase.required_action().or_else(|| {
            (status.desired == Desired::Enabled && !login.ready).then_some("verify_guard_login")
        })
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                request_id,
                receipt,
                status: Some(&status),
                login: Some(login),
                login_operation,
                requires_action
            })
            .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        if let Some(outcome) = &login_operation {
            if let Some(code) = &outcome.error {
                println!(
                    "登录入口诊断：{code}；请用 guard login request {} 查询原请求。",
                    outcome.request_id
                );
            } else if !matches!(
                outcome.status,
                Some(app_proxy_windows::guard_task::login::journal::Status::Created { .. })
            ) {
                println!(
                    "登录自启动登记结果待确认；请用 guard login request {} 查询。",
                    outcome.request_id
                );
            }
        }
        let name = coordinator::catalog(root.clone())
            .await
            .ok()
            .and_then(|catalog| {
                catalog
                    .instances
                    .into_iter()
                    .find(|instance| instance.id == id)
            })
            .map(|instance| {
                instance
                    .name
                    .chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
            })
            .unwrap_or_else(|| "当前实例".into());
        println!(
            "{name}：{}。\n进程监听：{}。",
            match status.phase {
                GuardPhase::Disabled => "保护已关闭",
                GuardPhase::NeedsAuthorization => "保护未生效，等待组件授权",
                GuardPhase::Blocked => "保护受阻，请查看诊断",
                GuardPhase::Starting => "保护正在检查或纠正",
                GuardPhase::Active => "保护运行中",
                GuardPhase::Degraded => "保护降级，请查看诊断",
            },
            component(status.listener)
        );
        crate::login_cli::print_view(&login);
        if let Some(scan) = &status.scan {
            use crate::launch_engine::GuardObservation;
            match &scan.observation {
                GuardObservation::Disabled {} => {}
                GuardObservation::Pending { .. } => {
                    println!("应用正在启动，稍后继续检查。")
                }
                GuardObservation::Session { process, .. } => {
                    println!("应用已在运行（PID {}）。", process.pid)
                }
                GuardObservation::Absent {} => println!("应用尚未启动。"),
                GuardObservation::Compliant { process } => println!(
                    "主进程 PID {} 的代理参数匹配；网络可用性需单独检查。",
                    process.pid
                ),
                GuardObservation::Correction { target } => println!(
                    "主进程 PID {} 的代理参数不匹配；自动保护就绪后会重新核验并纠正。",
                    target.process.pid
                ),
                GuardObservation::Blocked { code } => println!("无法确认进程状态：{code}。"),
            }
        }
        if let Some(code) = &status.diagnostic {
            if matches!(
                code.as_str(),
                "GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED" | "GUARD_LISTENER_START_PENDING"
            ) {
                println!("组件已安装，正在等待后台连接，无需重复安装。");
            } else if matches!(
                code.as_str(),
                "GUARD_INITIAL_SCAN_PENDING"
                    | "GUARD_SCAN_REFRESH_PENDING"
                    | "GUARD_RESOLUTION_BUSY"
                    | "GUARD_SCAN_BUSY"
                    | "GUARD_LISTENER_CHECK_BUSY"
            ) || code.starts_with("GUARD_CORRECTION_PENDING:")
                || code.starts_with("GUARD_LAUNCH_PENDING:")
            {
                println!("正在完成首次检查或自动纠正，请稍候。");
            } else {
                println!("保护诊断：{code}。");
            }
        }
        if let Some(action) = requires_action {
            println!(
                "{}",
                match action {
                    "verify_guard_login" =>
                        "本次保护状态如上；登录自启动尚未就绪，可在管理实例中选择启用或修复保护。",
                    "authorize_guard_components" =>
                        "实例已保留；请在管理实例中启用保护并授权安装监听组件。",
                    "wait_for_guard_check" =>
                        "后台仍在准备，稍后在管理实例中查看保护状态，无需重复创建实例。",
                    _ => "实例已保留；请按上述诊断处理，可在管理实例中重新查看保护状态。",
                }
            );
        }
    }
    if desired.is_some() && requires_action.is_some() {
        return Err(fail(
            exit::ACTION_REQUIRED,
            "保护尚未完全就绪，请按状态提示处理。",
        ));
    }
    Ok(Some(status))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending() -> GuardStatus {
        GuardStatus {
            instance_id: Uuid::new_v4(),
            revision: 7,
            desired: Desired::Enabled,
            phase: GuardPhase::Starting,
            listener: ComponentState::Unverified,
            scan: None,
            diagnostic: Some("GUARD_LISTENER_START_PENDING".into()),
        }
    }

    #[tokio::test]
    async fn registration_does_not_wait_for_heartbeat_but_success_display_does() {
        let status = pending();
        assert!(listener_registered(&status));
        let mut foreground = crate::foreground::Foreground::detached(false);
        let mut polls = 0;
        let ready = settle_status(status, &mut foreground, || {
            polls += 1;
            let mut next = pending();
            next.phase = GuardPhase::Active;
            next.listener = ComponentState::ActiveEtw;
            next.diagnostic = None;
            std::future::ready(Ok(next))
        })
        .await
        .unwrap();
        assert_eq!(polls, 1);
        assert!(ready.phase == GuardPhase::Active);
        assert!(ready.diagnostic.is_none());
    }

    #[tokio::test]
    async fn settling_stops_on_configuration_change_and_does_not_retry_real_failures() {
        let mut foreground = crate::foreground::Foreground::detached(false);
        let changed = settle_status(pending(), &mut foreground, || {
            let mut next = pending();
            next.revision = 8;
            std::future::ready(Ok(next))
        })
        .await
        .unwrap();
        assert_eq!(changed.revision, 8);
        let mut failed = pending();
        failed.phase = GuardPhase::Blocked;
        failed.diagnostic = Some("GUARD_COORDINATOR_CHANGED".into());
        assert!(!listener_registered(&failed));
        let failed = settle_status(failed, &mut foreground, || async {
            panic!("must not retry permanent failure")
        })
        .await
        .unwrap();
        assert!(failed.phase == GuardPhase::Blocked);
    }

    #[tokio::test]
    async fn cached_missing_after_verified_install_waits_without_reinstalling() {
        let mut status = pending();
        status.phase = GuardPhase::Blocked;
        status.diagnostic = Some("GUARD_LISTENER_MISSING".into());
        normalize_listener_wait(&mut status);
        assert!(listener_registered(&status));
        assert!(settling(&status));
        let mut polls = 0;
        let mut foreground = crate::foreground::Foreground::detached(false);
        let ready = settle_status(status, &mut foreground, || {
            polls += 1;
            let mut next = pending();
            if polls == 1 {
                next.phase = GuardPhase::Blocked;
                next.diagnostic = Some("GUARD_LISTENER_MISSING".into());
            } else {
                next.phase = GuardPhase::Active;
                next.listener = ComponentState::ActiveEtw;
                next.diagnostic = None;
            }
            std::future::ready(Ok(next))
        })
        .await
        .unwrap();
        assert_eq!(polls, 2);
        assert!(ready.phase == GuardPhase::Active);

        let mut missing = pending();
        missing.listener = ComponentState::NeedsAuthorization;
        missing.phase = GuardPhase::NeedsAuthorization;
        missing.diagnostic = Some("GUARD_LISTENER_MISSING".into());
        normalize_listener_wait(&mut missing);
        assert!(!listener_registered(&missing));
        assert!(!settling(&missing));
    }

    #[tokio::test]
    async fn transient_resolution_contention_is_waited_out_before_reporting() {
        let mut busy = pending();
        busy.phase = GuardPhase::Blocked;
        busy.listener = ComponentState::ActiveEtw;
        busy.diagnostic = Some("GUARD_RESOLUTION_BUSY".into());
        let mut foreground = crate::foreground::Foreground::detached(false);
        let ready = settle_status(busy, &mut foreground, || {
            let mut next = pending();
            next.phase = GuardPhase::Active;
            next.listener = ComponentState::ActiveEtw;
            next.diagnostic = None;
            std::future::ready(Ok(next))
        })
        .await
        .unwrap();
        assert!(ready.phase == GuardPhase::Active);
    }
}
