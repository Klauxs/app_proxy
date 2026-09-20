use crate::{
    coordinator,
    guard_control::{ComponentState, GuardPhase, GuardStatus},
    instance_cli::{Failure, fail, submit},
};
use app_proxy_core::{model::Desired, registry::ConfigAction};
use clap::Subcommand;
use serde::Serialize;
use std::io::IsTerminal;
use std::path::PathBuf;
use uuid::Uuid;

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
    /// 关闭实例保护；已有 IFEO 时必须先解除系统接管
    Disable { id: Uuid },
}

#[derive(Serialize)]
struct Report {
    request_id: Option<Uuid>,
    receipt: Option<app_proxy_core::registry::ConfigReceipt>,
    status: Option<GuardStatus>,
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
}

pub(crate) async fn run_with_foreground(
    root: PathBuf,
    command: Command,
    json: bool,
    foreground: &mut crate::foreground::Foreground,
    expected_revision: Option<u64>,
) -> Result<(), Failure> {
    foreground.check()?;
    if let Command::Login { command } = command {
        return crate::login_cli::run(root, command, json).await;
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
            .map_err(|e| fail(3, e.to_string()))?;
    }
    // Check protocol support before changing configuration with an older host.
    let mut status = coordinator::guard_status(root.clone(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    if expected_revision.is_some_and(|revision| revision != status.revision) {
        return Err(fail(4, "配置已变化，请重新确认保护设置。"));
    }
    let mut confirmed_revision = status.revision;
    let mut request_id = None;
    let mut receipt = None;
    if let Some(desired) = desired
        && status.desired != desired
    {
        foreground.check()?;
        let catalog = coordinator::catalog(root.clone())
            .await
            .map_err(|e| fail(3, e.to_string()))?;
        if confirmed_revision != catalog.revision {
            return Err(fail(4, "配置已变化，请重新确认保护设置。"));
        }
        foreground.check()?;
        let instance = catalog
            .instances
            .iter()
            .find(|i| i.id == id)
            .ok_or_else(|| fail(2, "INSTANCE_NOT_FOUND"))?;
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
                        .map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
                    );
                }
                return Err(fail(
                    6,
                    "配置回执已保存；保护状态暂未确认，请查询 guard status，不要重复配置请求。",
                ));
            }
        }
    }
    if confirmed_revision != status.revision {
        return Err(fail(4, "保护设置已保存，但配置随后发生变化；请重新确认。"));
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
                .map_err(|e| fail(3, e.to_string()))?;
            if catalog.revision != status.revision
                || !catalog
                    .instances
                    .iter()
                    .any(|i| i.id == id && i.guard == Desired::Enabled)
            {
                return Err(fail(5, "配置已变化，未发起授权；请重新查看 guard status。"));
            }
            let store = coordinator::status(root.clone())
                .await
                .map_err(|e| fail(3, e.to_string()))?
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
                .map_err(|_| fail(6, "未能启动授权流程。"))?;
            let installed = tokio::select! {
                biased;
                _ = foreground.cancelled() => return Err(fail(6, "已停止等待授权。若 Windows 授权窗口仍在，请选择取消；已提交的安装结果未确认，请先查询 guard status，不要重复安装。")),
                result = result => result.map_err(|_| fail(6, "组件安装结果不明，请先查询 guard status，不要重复安装。"))?,
            };
            match installed {
                Ok(_) => {
                    println!("监听组件已安装并核验，自动检查将接入；请以接下来的保护状态为准。")
                }
                Err(app_proxy_windows::Error::Invalid("GUARD_INSTALL_CANCELLED")) => {
                    return Err(fail(5, "已取消 Windows 授权，实例配置保留。"));
                }
                Err(error) => {
                    return Err(fail(
                        6,
                        format!(
                            "监听组件安装未确认：{error}。请先查询 guard status；不会自动重试。"
                        ),
                    ));
                }
            }
            foreground.check()?;
            status = coordinator::guard_status(root.clone(), id)
                .await
                .map_err(|e| fail(6, e.to_string()))?;
        }
    }
    if confirmed_revision != status.revision {
        return Err(fail(
            4,
            "配置已变化；未继续登记登录入口，请重新确认保护设置。",
        ));
    }
    let mut login = login_view(root.clone()).await;
    let mut login_operation = None;
    if desired == Some(Desired::Enabled) {
        foreground.check()?;
        if login.revision != 0 && login.revision != confirmed_revision {
            return Err(fail(4, "配置已变化，请重新确认保护设置。"));
        }
        let listener_verified = matches!(
            status.listener,
            ComponentState::ActiveEtw | ComponentState::ActivePolling
        ) || status.diagnostic.as_deref()
            == Some("GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED");
        if listener_verified && login.integration.is_none() && login.diagnostic.is_none() {
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
                status: Some(status),
                login: Some(login),
                login_operation,
                requires_action
            })
            .map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        crate::login_cli::print_view(&login);
        if let Some(outcome) = &login_operation {
            println!("登录入口请求：{}。", outcome.request_id);
            if let Some(code) = &outcome.error {
                println!(
                    "登录入口诊断：{code}；请用 guard login request {} 查询原请求。",
                    outcome.request_id
                );
            }
        }
        println!(
            "实例 {id}：{}；监听：{}；IFEO：{}。",
            match status.phase {
                GuardPhase::Disabled => "保护已关闭",
                GuardPhase::NeedsAuthorization => "保护未生效，等待组件授权",
                GuardPhase::Blocked => "保护受阻，请查看诊断",
                GuardPhase::Starting => "保护正在检查或纠正",
                GuardPhase::Active => "保护运行中",
                GuardPhase::Degraded => "保护降级，请查看诊断",
            },
            component(status.listener),
            component(status.ifeo)
        );
        if let Some(scan) = status.scan {
            use crate::launch_engine::GuardObservation;
            match scan.observation {
                GuardObservation::Disabled {} => {}
                GuardObservation::Pending { attempt_id } => {
                    println!("启动请求 {attempt_id} 尚未结束，暂缓检查。")
                }
                GuardObservation::Session { process, .. } => {
                    println!("已确认会话 PID {} 仍按启动时的配置保留。", process.pid)
                }
                GuardObservation::Absent {} => println!("未发现目标实例主进程。"),
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
        if let Some(code) = status.diagnostic {
            if code == "GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED" {
                println!("监听组件注册已核验，运行连接尚未确认。");
            } else {
                println!("保护诊断：{code}。");
            }
        }
        if requires_action.is_some() {
            println!(
                "实例配置已保留；监听组件可在交互式 guard enable 中授权安装，后台不会弹出 UAC。完整保护仍待组件就绪。"
            );
        }
    }
    if desired.is_some() && requires_action.is_some() {
        return Err(fail(5, "保护尚未完全就绪，请按状态提示处理。"));
    }
    Ok(())
}
