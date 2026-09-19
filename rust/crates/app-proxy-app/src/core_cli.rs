//! Explicit core controls. A timeout only queries the original request ID.
use crate::{
    coordinator,
    core_manager::CoreObserved,
    instance_cli::{Failure, fail},
};
use app_proxy_core::core_control::{CoreAction, CoreOutcome, CoreRequestStatus, InstallPhase};
use clap::Subcommand;
use std::{
    io::{IsTerminal, Write},
    path::PathBuf,
    time::Duration,
};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Command {
    /// 启动或复用本工具拥有的共享代理；健康检查使用已保存设置
    Start {
        #[arg(required = true, num_args = 1..)]
        profiles: Vec<Uuid>,
        /// 本次必须通过健康检查的代理，默认第一个
        #[arg(long)]
        required: Option<Uuid>,
    },
    /// 停止本工具拥有的共享代理；不会关闭应用
    Stop,
    /// 从固定官方发布安装 sing-box，自动管理目录；不启动应用
    Install,
    /// 请求取消指定的安装操作，不影响应用；以原请求结果为准
    Cancel { id: Uuid },
    /// 执行已检查的具体变更计划；会重启该计划中的共享代理
    ApplyUpdate { id: Uuid },
    /// 核对并恢复中断的重配置；不重放未知的进程创建
    RecoverUpdate { id: Uuid },
    /// 查看本工具的进程及监听状态，不执行网络健康检查
    Status,
    /// 查询原请求的历史结果，不会重新执行启动或停止
    Request { id: Uuid },
}

pub(crate) fn output(
    id: Uuid,
    status: &Option<CoreRequestStatus>,
    json: bool,
) -> Result<(), Failure> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"request_id": id, "result": status}))
                .map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        println!(
            "请求 {id}：{}",
            match status {
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Prepared { .. },
                    ..
                }) => "候选配置检查通过，等待确认后切换。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Reconfigured { .. },
                    ..
                }) => "变更已提交；切换前已通过健康检查，当前状态请使用 core status。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Restored { core_down: false },
                    ..
                }) => "变更失败，已恢复旧配置及共享代理。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Restored { core_down: true },
                    ..
                }) => "变更失败，旧配置已保留，但共享代理未恢复。应用保持运行。",
                Some(CoreRequestStatus::Pending { .. }) => "仍在执行。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Ready { .. },
                    ..
                }) => "该请求已完成启动与健康检查；当前状态请使用 core status。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Stopped {},
                    ..
                }) => "该请求已完成停止。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Installed { .. },
                    ..
                }) => "sing-box 已安装；代理仍需配置与健康检查。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Cancelled {},
                    ..
                }) => "安装已取消。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::CancelRequested { .. },
                    ..
                }) => "已发出取消请求；请查询原安装编号确认结果。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Failed { .. },
                    ..
                }) => "操作失败。",
                _ => "结果未确认，请保留原请求编号。",
            }
        );
    }
    Ok(())
}

pub(crate) fn outcome(status: Option<CoreRequestStatus>) -> Result<(), Failure> {
    match status {
        Some(CoreRequestStatus::Complete {
            outcome:
                CoreOutcome::Ready { .. }
                | CoreOutcome::Prepared { .. }
                | CoreOutcome::Reconfigured { .. }
                | CoreOutcome::Stopped {}
                | CoreOutcome::Installed { .. }
                | CoreOutcome::CancelRequested { .. },
            ..
        }) => Ok(()),
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Restored { core_down },
            ..
        }) => Err(fail(
            3,
            if core_down {
                "变更失败，旧配置已保留但代理未恢复；请检查 core status。"
            } else {
                "变更失败，已恢复旧配置和代理。"
            },
        )),
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Cancelled {},
            ..
        }) => Err(fail(5, "安装已取消；保留代理配置。")),
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Failed { code },
            ..
        }) => Err(fail(3, code)),
        _ => Err(fail(6, "请求结果未确认；请查询原编号，不要自动重新提交。")),
    }
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    let action = match command {
        Command::Status => {
            let snapshot = coordinator::core_status(root)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&snapshot)
                        .map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
                );
            } else {
                println!(
                    "{}",
                    match snapshot.observed {
                        CoreObserved::Stopped => "共享代理已停止。",
                        CoreObserved::Down => "共享代理已退出，保留恢复配置。",
                        CoreObserved::Listening =>
                            "共享代理进程与端口归属已确认；未执行网络健康检查。",
                        CoreObserved::Indeterminate => "共享代理状态无法确认，保留现有进程。",
                    }
                );
                if let Some(update) = snapshot.update {
                    use app_proxy_windows::core_update::UpdatePhase;
                    let phase = match update.phase {
                        UpdatePhase::Prepared {} => "候选已检查，尚未切换",
                        UpdatePhase::Switching {} => "切换中或中断待核对",
                        UpdatePhase::Committing {} => "配置提交尚待完成",
                        UpdatePhase::Committed {} => "变更已提交",
                        UpdatePhase::Restoring {} => "恢复中或中断待核对",
                        UpdatePhase::Restored { core_down: false } => "旧配置已恢复",
                        UpdatePhase::Restored { core_down: true } => "旧配置已保留，代理未恢复",
                    };
                    println!("最近重配置计划 {}：{phase}。", update.impact.plan_id);
                    if matches!(
                        update.phase,
                        UpdatePhase::Switching {}
                            | UpdatePhase::Committing {}
                            | UpdatePhase::Restoring {}
                    ) {
                        println!(
                            "如操作已中断，运行 core recover-update {} 核对。",
                            update.impact.plan_id
                        );
                    }
                }
            }
            return Ok(());
        }
        Command::Request { id } => {
            let status = coordinator::core_request_status(root, id)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            output(id, &status, json)?;
            return outcome(status);
        }
        Command::Stop => CoreAction::Stop {},
        Command::Install => CoreAction::Install {},
        Command::Cancel { id } => CoreAction::CancelInstall { request_id: id },
        Command::ApplyUpdate { id } => CoreAction::ApplyUpdate { plan_id: id },
        Command::RecoverUpdate { id } => CoreAction::RecoverUpdate { plan_id: id },
        Command::Start { profiles, required } => {
            let required = required
                .or_else(|| profiles.first().copied())
                .ok_or_else(|| fail(2, "PROFILE_REQUIRED"))?;
            CoreAction::Start { profiles, required }
        }
    };
    let mut action = action;
    action.normalize().map_err(|e| fail(2, e.0))?;
    let (mut id, mut status, interrupted) = submit(root.clone(), action.clone(), json).await;
    if interrupted {
        output(id, &status, json)?;
        return Err(fail(5, "已返回原流程；如安装结果未确认，请查询原编号。"));
    }
    if missing_binary(&status)
        && !json
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
    {
        if !choose("未找到可用的 sing-box。", "安装并继续")? {
            return Err(fail(5, "已返回；保留代理配置。"));
        }
        loop {
            let (install_id, installed, interrupted) =
                submit(root.clone(), CoreAction::Install {}, false).await;
            output(install_id, &installed, false)?;
            if interrupted {
                return Err(fail(5, "已返回原流程；不会继续启动。"));
            }
            match &installed {
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Installed { .. },
                    ..
                }) => break,
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Failed { code },
                    ..
                }) => {
                    eprintln!("安装失败：{code}");
                    if !choose("可以重新下载安装。", "重试")? {
                        return Err(fail(5, "已返回；保留代理配置。"));
                    }
                }
                _ => return outcome(installed),
            }
        }
        // Only the still-active client resumes a definitively failed start.
        // Completing installation alone never starts a core or an application.
        let resumed = submit(root, action, false).await;
        id = resumed.0;
        status = resumed.1;
    }
    output(id, &status, json)?;
    outcome(status)
}

fn missing_binary(status: &Option<CoreRequestStatus>) -> bool {
    matches!(status, Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_BINARY_MISSING")
}

fn choose(message: &str, primary: &str) -> Result<bool, Failure> {
    loop {
        eprint!("{message}\n1. {primary}（默认）  2. 返回\n请选择 [1/2]：");
        std::io::stderr()
            .flush()
            .map_err(|_| fail(10, "PROMPT_WRITE_FAILED"))?;
        let mut input = String::new();
        if std::io::stdin()
            .read_line(&mut input)
            .map_err(|_| fail(10, "PROMPT_READ_FAILED"))?
            == 0
        {
            return Ok(false);
        }
        match input.trim() {
            "" | "1" => return Ok(true),
            "2" => return Ok(false),
            _ => eprintln!("请输入 1 或 2。"),
        }
    }
}

pub(crate) async fn submit(
    root: PathBuf,
    action: CoreAction,
    json: bool,
) -> (Uuid, Option<CoreRequestStatus>, bool) {
    let id = Uuid::new_v4();
    let installing = matches!(action, CoreAction::Install {});
    // Persist this in the caller's terminal before any request can be accepted.
    eprintln!("请求编号：{id}；结果不明时运行 core request {id} 查询。");
    let initial = coordinator::control_core(root.clone(), id, action).await;
    let mut status = match initial {
        Ok(status) => Some(status),
        Err(_) => coordinator::core_request_status(root.clone(), id)
            .await
            .ok()
            .flatten(),
    };
    let started = tokio::time::Instant::now();
    let mut deadline = started + Duration::from_secs(if installing { 720 } else { 90 });
    let mut progress_at = started;
    let interrupt = tokio::signal::ctrl_c();
    tokio::pin!(interrupt);
    let mut interrupted = false;
    let mut signal_available = true;
    while matches!(status, Some(CoreRequestStatus::Pending { .. }))
        && tokio::time::Instant::now() < deadline
    {
        tokio::select! {
            signal = &mut interrupt, if installing && signal_available => {
                signal_available = false;
                if signal.is_ok() {
                    interrupted = true;
                    let cancel_id = Uuid::new_v4();
                    eprintln!("正在请求取消安装；取消请求编号 {cancel_id}。");
                    let _ = coordinator::control_core(root.clone(), cancel_id, CoreAction::CancelInstall { request_id: id }).await;
                    deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
        }
        if installing && !json && tokio::time::Instant::now() >= progress_at {
            if let Some(CoreRequestStatus::Pending {
                progress: Some(progress),
            }) = &status
            {
                let phase = match progress.phase {
                    InstallPhase::CheckingExisting => "检查已有安装",
                    InstallPhase::Downloading => "下载",
                    InstallPhase::Verifying => "校验安装包",
                    InstallPhase::CheckingBinary => "验证程序",
                    InstallPhase::Publishing => "提交安装",
                };
                eprintln!(
                    "sing-box：{phase}；已下载 {:.1}/{:.1} MiB，等待 {} 秒…",
                    progress.downloaded as f64 / 1048576.0,
                    progress.total as f64 / 1048576.0,
                    started.elapsed().as_secs()
                );
            }
            progress_at = tokio::time::Instant::now() + Duration::from_secs(5);
        }
        status = coordinator::core_request_status(root.clone(), id)
            .await
            .ok()
            .flatten();
    }
    (id, status, interrupted)
}
