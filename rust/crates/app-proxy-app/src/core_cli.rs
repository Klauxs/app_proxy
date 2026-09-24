//! Explicit core controls. A timeout only queries the original request ID.
use crate::{
    coordinator,
    core_manager::CoreObserved,
    exit::{self, Failure, fail},
    foreground::Foreground,
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
        /// 同意新增入口时重启共享代理，保留现有入口及应用进程
        #[arg(long)]
        apply_to_running: bool,
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
    /// 核对中断的内核创建，不自动重启或宣称代理健康
    RecoverStart,
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
        crate::output::json(&serde_json::json!({"request_id": id, "result": status}))?;
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
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::ProfileRemoved { .. },
                    ..
                }) => "代理配置已移除；应用和凭据文件保留。",
                Some(CoreRequestStatus::Complete {
                    outcome:
                        CoreOutcome::Reconciled {
                            process: Some(_), ..
                        },
                    ..
                }) => "已核对并记录原内核进程；尚未验证代理健康。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Reconciled { process: None, .. },
                    ..
                }) => "已确认原创建没有存活内核；未自动重启。",
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
                | CoreOutcome::ProfileRemoved { .. }
                | CoreOutcome::Reconciled { .. }
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
            exit::UNAVAILABLE,
            if core_down {
                "变更失败，旧配置已保留但代理未恢复；请检查 core status。"
            } else {
                "变更失败，已恢复旧配置和代理。"
            },
        )),
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Cancelled {},
            ..
        }) => Err(fail(exit::ACTION_REQUIRED, "安装已取消；保留代理配置。")),
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Failed { code },
            ..
        }) => Err(fail(
            exit::UNAVAILABLE,
            crate::proxy_health::failure_message(&code)
                .map_or(code.clone(), |message| format!("{message}（{code}）")),
        )),
        _ => Err(fail(
            exit::UNCONFIRMED,
            "请求结果未确认；请查询原编号，不要自动重新提交。",
        )),
    }
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    let mut foreground = Foreground::new();
    let mut apply = false;
    let action = match command {
        Command::Status => {
            let snapshot = coordinator::core_status(root)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            if json {
                crate::output::json(&snapshot)?;
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
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            output(id, &status, json)?;
            return outcome(status);
        }
        Command::Stop => CoreAction::Stop {},
        Command::RecoverStart => {
            use app_proxy_windows::core_state::CoreState;
            let snapshot = coordinator::core_status(root.clone())
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            let generation = match snapshot.recorded {
                CoreState::Starting { generation }
                | CoreState::Running { generation, .. }
                | CoreState::Down { generation } => generation,
                CoreState::Stopped {} => {
                    return Err(fail(exit::UNAVAILABLE, "没有待核对的内核创建。"));
                }
            };
            CoreAction::RecoverStart { generation }
        }
        Command::Install => CoreAction::Install {},
        Command::Cancel { id } => CoreAction::CancelInstall { request_id: id },
        Command::ApplyUpdate { id } => CoreAction::ApplyUpdate { plan_id: id },
        Command::RecoverUpdate { id } => CoreAction::RecoverUpdate { plan_id: id },
        Command::Start {
            profiles,
            required,
            apply_to_running,
        } => {
            apply = apply_to_running;
            let required = required
                .or_else(|| profiles.first().copied())
                .ok_or_else(|| fail(exit::INVALID, "PROFILE_REQUIRED"))?;
            CoreAction::Start { profiles, required }
        }
    };
    let mut action = action;
    action.normalize().map_err(|e| fail(exit::INVALID, e.0))?;
    let (mut id, mut status, interrupted) =
        submit(root.clone(), action.clone(), json, &mut foreground).await;
    if interrupted {
        output(id, &status, json)?;
        return Err(fail(
            exit::ACTION_REQUIRED,
            "已返回原流程；如操作结果未确认，请查询原编号。",
        ));
    }
    if missing_binary(&status)
        && !json
        && std::io::stdin().is_terminal()
        && std::io::stderr().is_terminal()
    {
        install_interactively(root.clone(), &mut foreground).await?;
        // Only the still-active client resumes a definitively failed start.
        // Completing installation alone never starts a core or an application.
        foreground.check()?;
        let resumed = submit(root.clone(), action.clone(), false, &mut foreground).await;
        id = resumed.0;
        status = resumed.1;
        if resumed.2 {
            output(id, &status, json)?;
            return Err(fail(exit::ACTION_REQUIRED, "已返回；请查询原操作编号。"));
        }
    }
    if matches!(&status, Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_RECONFIGURE_REQUIRES_CONFIRMATION")
        && let CoreAction::Start { profiles, required } = action
    {
        let catalog = coordinator::catalog(root.clone())
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
        return prepare_and_apply_with_foreground(
            root,
            CoreAction::PrepareExpand {
                expected_revision: catalog.revision,
                profiles,
                required,
            },
            apply,
            json,
            &mut foreground,
        )
        .await;
    }
    output(id, &status, json)?;
    outcome(status)
}

fn missing_binary(status: &Option<CoreRequestStatus>) -> bool {
    matches!(status, Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_BINARY_MISSING")
}

pub(crate) fn interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

pub(crate) async fn install_interactively(
    root: PathBuf,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    if !choose("未找到可用的 sing-box。", "安装并继续", foreground).await? {
        return Err(fail(exit::ACTION_REQUIRED, "已返回；保留代理配置。"));
    }
    loop {
        let (id, installed, interrupted) =
            submit(root.clone(), CoreAction::Install {}, false, foreground).await;
        output(id, &installed, false)?;
        if interrupted {
            return Err(fail(exit::ACTION_REQUIRED, "已返回原流程；不会继续启动。"));
        }
        match &installed {
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Installed { .. },
                ..
            }) => return Ok(()),
            Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Failed { code },
                ..
            }) => {
                eprintln!("安装失败：{code}");
                if !choose("可以重新下载安装。", "重试", foreground).await? {
                    return Err(fail(exit::ACTION_REQUIRED, "已返回；保留代理配置。"));
                }
            }
            _ => return outcome(installed),
        }
    }
}

async fn choose(
    message: &str,
    primary: &str,
    foreground: &mut Foreground,
) -> Result<bool, Failure> {
    loop {
        eprint!("{message}\n1. {primary}（默认）  2. 返回\n请选择 [1/2]：");
        std::io::stderr()
            .flush()
            .map_err(|_| fail(exit::INTERNAL, "PROMPT_WRITE_FAILED"))?;
        let input = foreground.read_line().await?;
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
    foreground: &mut Foreground,
) -> (Uuid, Option<CoreRequestStatus>, bool) {
    let id = Uuid::new_v4();
    if foreground.is_cancelled() {
        return (id, None, true);
    }
    let installing = matches!(action, CoreAction::Install {});
    let before = if !json
        && !foreground.quiet()
        && matches!(
            action,
            CoreAction::Start { .. }
                | CoreAction::ApplyUpdate { .. }
                | CoreAction::RecoverUpdate { .. }
        ) {
        coordinator::catalog(root.clone()).await.ok()
    } else {
        None
    };
    foreground.remember_core(id);
    // The foreground retains recovery IDs; normal success needs no UUID banner.
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
    let mut interrupted = false;
    while matches!(status, Some(CoreRequestStatus::Pending { .. }))
        && tokio::time::Instant::now() < deadline
    {
        tokio::select! {
            _ = foreground.cancelled(), if !interrupted => {
                interrupted = true;
                if installing {
                    let cancel_id = Uuid::new_v4();
                    eprintln!("正在请求取消安装；取消请求编号 {cancel_id}。");
                    let _ = coordinator::control_core(root.clone(), cancel_id, CoreAction::CancelInstall { request_id: id }).await;
                } else { eprintln!("已停止后续操作；正在核对已接受的共享代理操作，应用会保留。"); }
                deadline = tokio::time::Instant::now() + Duration::from_secs(20);
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
    if let Some(before) = before {
        report_port_changes(&root, &before).await;
    }
    (id, status, interrupted || foreground.is_cancelled())
}

pub(crate) async fn report_port_changes(
    root: &std::path::Path,
    before: &crate::configuration::CatalogPage,
) {
    if let Ok(after) = coordinator::catalog(root.to_owned()).await {
        for old in &before.profiles {
            if let Some(new) = after.profiles.iter().find(|p| p.id == old.id)
                && old.endpoint != new.endpoint
            {
                eprintln!(
                    "代理 {} 的本地端口已从 {} 自动调整为 {}；使用旧端口的应用请重启。",
                    crate::output::plain(&new.name),
                    old.endpoint.port,
                    new.endpoint.port
                );
            }
        }
    }
}

pub(crate) async fn prepare_and_apply_with_foreground(
    root: PathBuf,
    action: CoreAction,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    let (id, status) = prepare_and_apply_result(root, action, apply, json, foreground).await?;
    output(id, &status, json)?;
    outcome(status)
}

async fn prepare_and_apply_result(
    root: PathBuf,
    action: CoreAction,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(Uuid, Option<CoreRequestStatus>), Failure> {
    foreground.check()?;
    let (request_id, status, interrupted) = submit(root.clone(), action, json, foreground).await;
    if interrupted {
        output(request_id, &status, json)?;
        return Err(fail(
            exit::ACTION_REQUIRED,
            "已停止后续操作；请查询原请求编号。",
        ));
    }
    let Some(CoreRequestStatus::Complete {
        outcome: CoreOutcome::Prepared { ref impact },
        ..
    }) = status
    else {
        output(request_id, &status, json)?;
        outcome(status)?;
        return Err(fail(exit::UNCONFIRMED, "CORE_UPDATE_PLAN_MISSING"));
    };
    let plan_id = impact.plan_id;
    if json {
        if !apply {
            output(request_id, &status, true)?;
        }
    } else {
        show_impact(impact);
    }
    let confirmed = if apply {
        true
    } else if !json && interactive() {
        confirm_impact(foreground).await?
    } else {
        false
    };
    if !confirmed {
        return Err(fail(
            exit::ACTION_REQUIRED,
            format!("配置未切换；确认此计划可运行 core apply-update {plan_id}。"),
        ));
    }
    foreground.check()?;
    let (id, status, interrupted) =
        submit(root, CoreAction::ApplyUpdate { plan_id }, json, foreground).await;
    if interrupted {
        output(id, &status, json)?;
        return Err(fail(
            exit::ACTION_REQUIRED,
            "已停止后续操作；请查询原请求编号。",
        ));
    }
    Ok((id, status))
}

/// Prepare a route for a subscription download with the same installation and
/// concrete shared-core confirmation flow as an explicit start.
pub(crate) async fn ensure_download_profile(
    root: PathBuf,
    profile_id: Uuid,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    ensure_profile(root, profile_id, apply, json, foreground, false).await
}

pub(crate) async fn ensure_instance_profile(
    root: PathBuf,
    profile_id: Uuid,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    ensure_profile(root, profile_id, false, false, foreground, true).await
}

async fn ensure_profile(
    root: PathBuf,
    profile_id: Uuid,
    apply: bool,
    json: bool,
    foreground: &mut Foreground,
    probe_current: bool,
) -> Result<(), Failure> {
    foreground.check()?;
    let snapshot = coordinator::core_status(root.clone())
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    if !probe_current
        && matches!(snapshot.observed, CoreObserved::Listening)
        && snapshot.profiles.iter().any(|p| p.id == profile_id)
    {
        return foreground.check();
    }
    let action = CoreAction::Start {
        profiles: vec![profile_id],
        required: profile_id,
    };
    let (mut id, mut status, mut interrupted) =
        submit(root.clone(), action.clone(), json, foreground).await;
    if !interrupted && missing_binary(&status) && !json && interactive() {
        install_interactively(root.clone(), foreground).await?;
        foreground.check()?;
        (id, status, interrupted) = submit(root.clone(), action, json, foreground).await;
    }
    if interrupted {
        output(id, &status, json)?;
        return Err(fail(
            exit::ACTION_REQUIRED,
            "已停止下载；请查询原代理操作编号。",
        ));
    }
    if matches!(&status, Some(CoreRequestStatus::Complete { outcome: CoreOutcome::Failed { code }, .. }) if code == "CORE_RECONFIGURE_REQUIRES_CONFIRMATION")
    {
        let revision = coordinator::catalog(root.clone())
            .await
            .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?
            .revision;
        (id, status) = prepare_and_apply_result(
            root,
            CoreAction::PrepareExpand {
                expected_revision: revision,
                profiles: vec![profile_id],
                required: profile_id,
            },
            apply,
            json,
            foreground,
        )
        .await?;
    }
    if !matches!(
        status,
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Ready { .. } | CoreOutcome::Reconfigured { .. },
            ..
        })
    ) {
        output(id, &status, json)?;
        return outcome(status);
    }
    foreground.check()
}

pub(crate) fn show_impact(impact: &app_proxy_core::core_control::UpdateImpact) {
    println!(
        "计划 {}：候选检查通过；将切换共享代理配置。以下代理的连接会中断：",
        impact.plan_id
    );
    for id in &impact.affected_profiles {
        println!("  代理 {id}");
    }
    for id in &impact.removed_profiles {
        println!("  移除代理 {id}（最后一个入口移除后内核停止）");
    }
    for id in &impact.added_profiles {
        println!("  新增代理 {id}");
    }
    println!("使用这些代理的已登记实例（不代表正在运行）：");
    for id in &impact.bound_instances {
        println!("  实例 {id}");
    }
    println!("应用进程保留；切换失败会尝试恢复旧代理。");
}

pub(crate) async fn confirm_impact(foreground: &mut Foreground) -> Result<bool, Failure> {
    eprint!("1. 应用变更  2. 返回（默认）\n请选择 [1/2]：");
    std::io::stderr()
        .flush()
        .map_err(|_| fail(exit::INTERNAL, "PROMPT_WRITE_FAILED"))?;
    let answer = foreground.read_line().await?;
    Ok(answer.trim() == "1")
}
