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
    /// 查看保护实际状态和只读进程检查；不会启用或纠正
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
    requires_action: Option<&'static str>,
}

fn component(value: ComponentState) -> &'static str {
    match value {
        ComponentState::NotApplicable => "不适用",
        ComponentState::NeedsAuthorization => "待授权安装",
        ComponentState::Unverified => "组件待核验或等待监听",
    }
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    let (id, desired) = match command {
        Command::Status { id } => (id, None),
        Command::Enable { id } => (id, Some(Desired::Enabled)),
        Command::Disable { id } => (id, Some(Desired::Disabled)),
    };
    // Check protocol support before changing configuration with an older host.
    let mut status = coordinator::guard_status(root.clone(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    let mut request_id = None;
    let mut receipt = None;
    if let Some(desired) = desired
        && status.desired != desired
    {
        let catalog = coordinator::catalog(root.clone())
            .await
            .map_err(|e| fail(3, e.to_string()))?;
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
    if desired == Some(Desired::Enabled)
        && status.listener == ComponentState::NeedsAuthorization
        && !json
        && std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
    {
        let mut foreground = crate::foreground::Foreground::new();
        println!(
            "缺少进程监听组件，需要 Windows 管理员授权，安装位置自动选择。\n1. 安装监听组件\n2. 暂不安装（保留实例配置）\n组件安装后还需完成监听连接和保护核验，当前不会接管或关闭应用。"
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
                    println!("监听组件已安装并核验；保护尚未生效，仍需连接监听和完成其余组件。")
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
    let requires_action = match status.phase {
        GuardPhase::Disabled => None,
        GuardPhase::NeedsAuthorization => Some("authorize_guard_components"),
        GuardPhase::Blocked => Some("verify_guard_integrations"),
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                request_id,
                receipt,
                status: Some(status),
                requires_action
            })
            .map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        println!(
            "实例 {id}：{}；监听：{}；IFEO：{}。",
            match status.phase {
                GuardPhase::Disabled => "保护已关闭",
                GuardPhase::NeedsAuthorization => "保护未生效，等待组件授权",
                GuardPhase::Blocked => "保护组件待核验",
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
                    "主进程 PID {} 的代理参数不匹配。本次检查保留进程，等待保护组件就绪。",
                    target.process.pid
                ),
                GuardObservation::Blocked { code } => println!("无法确认进程状态：{code}。"),
            }
        }
        if let Some(code) = status.diagnostic {
            if code == "GUARD_LISTENER_REGISTERED_LIVENESS_UNVERIFIED" {
                println!("监听组件注册已核验，运行连接尚未确认。");
            } else {
                println!("保护核验尚未完成：{code}。");
            }
        }
        if requires_action.is_some() {
            println!(
                "实例配置已保留；监听组件可在交互式 guard enable 中授权安装，后台不会弹出 UAC。完整保护仍待组件就绪。"
            );
        }
    }
    if desired.is_some() && requires_action.is_some() {
        return Err(fail(5, "保护尚未完成：需要前台组件授权或核验。"));
    }
    Ok(())
}
