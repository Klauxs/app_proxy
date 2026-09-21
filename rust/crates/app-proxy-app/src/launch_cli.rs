//! Foreground launch flow. A request is never retried with another ID while its
//! result is uncertain; dependency repair resumes only a definite pre-spawn failure.
use crate::{
    coordinator, core_cli,
    exit::{self, Failure, fail},
    foreground::Foreground,
};
use app_proxy_core::{
    core_control::{CoreAction, CoreOutcome, CoreRequestStatus, UpdateImpact},
    launch::{LaunchAttempt, LaunchOrigin, LaunchPhase, LaunchRequest},
    model::{Desired, NetworkBinding},
};
use clap::{Args, Subcommand};
use serde::Serialize;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct Command {
    /// 启动已登记实例；代理未就绪时不会启动应用
    #[arg(required = true, value_name = "INSTANCE")]
    instance: Option<Uuid>,
    /// 保留此编号查询或重放同一请求，结果不明时不要换编号重试
    #[arg(long)]
    request_id: Option<Uuid>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    action: Option<Query>,
}

#[derive(Subcommand)]
enum Query {
    /// 查询原启动请求；不会再次创建应用
    Inspect { id: Uuid },
    /// 请求取消尚未创建的应用；已创建的应用会保留
    Cancel { id: Uuid },
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum RequiredAction {
    InstallSingBox {},
    ConfirmCoreUpdate { impact: UpdateImpact },
}

#[derive(Serialize)]
struct Report {
    request_id: Uuid,
    attempt: Option<LaunchAttempt>,
    #[serde(skip_serializing_if = "Option::is_none")]
    requires_action: Option<RequiredAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependency: Option<CoreDependency>,
}

#[derive(Serialize)]
struct CoreDependency {
    request_id: Uuid,
    result: Option<CoreRequestStatus>,
}
impl Report {
    fn new(request_id: Uuid, attempt: Option<LaunchAttempt>) -> Self {
        Self {
            request_id,
            attempt,
            requires_action: None,
            error: None,
            dependency: None,
        }
    }
    fn output(&self, json: bool, foreground: &Foreground) -> Result<(), Failure> {
        if foreground.quiet() {
            return Ok(());
        }
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(self)
                    .map_err(|_| fail(exit::INTERNAL, "OUTPUT_ENCODING_FAILED"))?
            );
        } else {
            println!(
                "启动请求 {}：{}",
                self.request_id,
                match self.attempt.as_ref().map(|a| (&a.phase, a.session_exited)) {
                    Some((LaunchPhase::Confirmed { .. }, true)) => "本次启动已确认，应用已经退出。",
                    Some((LaunchPhase::Confirmed { .. }, false)) =>
                        "启动已确认；保护状态不包含在此结果中。",
                    Some((LaunchPhase::Cancelled {}, _)) => "已取消，应用未创建。",
                    Some((LaunchPhase::Failed { .. }, _)) => "启动失败，保留应用和实例配置。",
                    Some((LaunchPhase::Indeterminate {}, _)) | None =>
                        "结果未确认，请保留请求编号并查询，不要换编号重试。",
                    _ => "仍在执行，请查询原请求编号。",
                }
            );
            if let Some(RequiredAction::InstallSingBox {}) = &self.requires_action {
                println!("需要安装 sing-box；请在交互终端启动，或先运行 core install。");
            }
            if let Some(RequiredAction::ConfirmCoreUpdate { impact }) = &self.requires_action {
                core_cli::show_impact(impact);
                println!(
                    "确认此计划可运行 core apply-update {}；成功后再发起启动。",
                    impact.plan_id
                );
            }
        }
        Ok(())
    }
    fn outcome(&self) -> Result<(), Failure> {
        if let Some(code) = &self.error {
            return Err(fail(error_exit(code), code));
        }
        if self.requires_action.is_some() {
            return Err(fail(
                exit::ACTION_REQUIRED,
                "需要完成上述前台操作；应用尚未创建。",
            ));
        }
        match self.attempt.as_ref().map(|a| &a.phase) {
            Some(LaunchPhase::Confirmed { .. }) => Ok(()),
            Some(LaunchPhase::Cancelled {}) => Err(fail(exit::ACTION_REQUIRED, "启动已取消。")),
            Some(LaunchPhase::Failed { code }) => Err(fail(error_exit(code), code)),
            _ => Err(fail(
                exit::UNCONFIRMED,
                "启动结果未确认；请使用 launch inspect 查询原编号。",
            )),
        }
    }
}

/// A failed launch is a dependency problem unless its code says otherwise.
fn error_exit(code: &str) -> i32 {
    exit::for_code(code).unwrap_or(exit::UNAVAILABLE)
}

pub async fn run(root: PathBuf, command: Command) -> Result<(), Failure> {
    run_with_foreground(
        root,
        command,
        None,
        LaunchOrigin::Interactive,
        &mut Foreground::new(),
    )
    .await
}

/// Hidden shortcut entry. It cannot initialize a missing store. Dependency
/// prompts use the same workflow and request IDs as CLI/menu launch.
pub async fn from_shortcut(root: PathBuf, instance: Uuid, notify: bool) -> Result<(), Failure> {
    app_proxy_windows::store::describe(&root).map_err(|e| {
        fail(
            exit::UNAVAILABLE,
            format!("无法打开实例数据目录：{}\n{e}", root.display()),
        )
    })?;
    let mut foreground = Foreground::detached(notify);
    let result = run_with_foreground(
        root.clone(),
        Command {
            instance: Some(instance),
            request_id: None,
            json: false,
            action: None,
        },
        None,
        LaunchOrigin::Shortcut,
        &mut foreground,
    )
    .await;
    result.map_err(|error| {
        fail(
            error.exit_code,
            format!(
                "{error}\n\n{}数据目录：{}\n结果未确认时先查询原编号，不要重复启动。",
                foreground.request_summary(),
                root.display()
            ),
        )
    })
}

pub(crate) async fn from_menu(
    root: PathBuf,
    instance: Uuid,
    revision: u64,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    run_with_foreground(
        root,
        Command {
            instance: Some(instance),
            request_id: None,
            json: false,
            action: None,
        },
        Some(revision),
        LaunchOrigin::Interactive,
        foreground,
    )
    .await
}

async fn run_with_foreground(
    root: PathBuf,
    command: Command,
    menu_revision: Option<u64>,
    origin: LaunchOrigin,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    foreground.check()?;
    let json = command.json;
    if let Some(action) = command.action {
        let (id, result) = match action {
            Query::Inspect { id } => (id, coordinator::launch_status(root, id).await),
            Query::Cancel { id } => (id, coordinator::cancel_launch(root, id).await.map(Some)),
        };
        let mut report = Report::new(id, None);
        match result {
            Ok(attempt) => report.attempt = attempt,
            Err(e) => report.error = Some(e.to_string()),
        }
        report.output(json, foreground)?;
        return report.outcome();
    }
    let instance_id = command
        .instance
        .ok_or_else(|| fail(exit::INVALID, "INSTANCE_REQUIRED"))?;
    let mut id = command.request_id.unwrap_or_else(Uuid::new_v4);
    if id.is_nil() || instance_id.is_nil() {
        return Err(fail(exit::INVALID, "INVALID_LAUNCH_REQUEST"));
    }
    // This snapshot is used only for foreground dependency repair. Replays still
    // work if the instance has subsequently been removed from the catalog.
    let catalog = coordinator::catalog(root.clone())
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    if menu_revision.is_some_and(|revision| revision != catalog.revision) {
        return Err(fail(exit::CONFLICT, "配置已变化，请重新确认启动。"));
    }
    let instance = catalog.instances.iter().find(|i| i.id == instance_id);
    if !foreground.quiet() && instance.is_some_and(|i| i.guard == Desired::Enabled) {
        eprintln!("启动结果不表示保护已生效；请以实例详情中的实际保护状态为准。");
    }
    let mut installed = false;
    let mut expanded = false;
    loop {
        foreground.check()?;
        let request = LaunchRequest {
            request_id: id,
            instance_id,
            origin,
        };
        let expected =
            menu_revision.or_else(|| (installed || expanded).then_some(catalog.revision));
        let (mut report, fresh, interrupted) =
            submit(root.clone(), request, expected, foreground).await;
        let repairable = fresh
            && !interrupted
            && report
                .attempt
                .as_ref()
                .is_some_and(|a| a.dispatch_id.is_none() && !a.resource_pending);
        let code = report.attempt.as_ref().and_then(|a| match &a.phase {
            LaunchPhase::Failed { code } => Some(code.as_str()),
            _ => None,
        });
        let install = repairable && !installed && code == Some("CORE_BINARY_MISSING");
        let expand =
            repairable && !expanded && code == Some("CORE_RECONFIGURE_REQUIRES_CONFIRMATION");
        if !install && !expand {
            report.output(json, foreground)?;
            return if interrupted {
                Err(fail(
                    exit::ACTION_REQUIRED,
                    "已请求取消；以上述持久状态为准，已创建的应用会保留。",
                ))
            } else {
                report.outcome()
            };
        }
        if install {
            report.requires_action = Some(RequiredAction::InstallSingBox {});
            if json || !foreground.can_prompt() {
                report.output(json, foreground)?;
                return report.outcome();
            }
            foreground.show_console()?;
            core_cli::install_interactively(root.clone(), foreground).await?;
            installed = true;
        } else {
            if !json && foreground.can_prompt() {
                foreground.show_console()?;
            }
            let profile = match instance.map(|i| &i.network) {
                Some(NetworkBinding::Profile { profile_id }) => *profile_id,
                _ => return Err(fail(exit::CONFLICT, "LAUNCH_CONFIG_CHANGED")),
            };
            ensure_revision(&root, catalog.revision).await?;
            foreground.check()?;
            let (core_id, prepared, interrupted) = core_cli::submit(
                root.clone(),
                CoreAction::PrepareExpand {
                    expected_revision: catalog.revision,
                    profiles: vec![profile],
                    required: profile,
                },
                json,
                foreground,
            )
            .await;
            let Some(CoreRequestStatus::Complete {
                outcome: CoreOutcome::Prepared { impact },
                ..
            }) = &prepared
            else {
                if !json && !foreground.quiet() {
                    core_cli::output(core_id, &prepared, false)?;
                }
                report.dependency = Some(CoreDependency {
                    request_id: core_id,
                    result: prepared.clone(),
                });
                report.output(json, foreground)?;
                return core_cli::outcome(prepared);
            };
            report.requires_action = Some(RequiredAction::ConfirmCoreUpdate {
                impact: impact.clone(),
            });
            if interrupted || json || !foreground.can_prompt() {
                report.output(json, foreground)?;
                return report.outcome();
            }
            core_cli::show_impact(impact);
            if !core_cli::confirm_impact(foreground).await? {
                return Err(fail(
                    exit::ACTION_REQUIRED,
                    "已返回；共享代理未切换，应用未创建。",
                ));
            }
            foreground.check()?;
            let (core_id, applied, interrupted) = core_cli::submit(
                root.clone(),
                CoreAction::ApplyUpdate {
                    plan_id: impact.plan_id,
                },
                false,
                foreground,
            )
            .await;
            core_cli::output(core_id, &applied, false)?;
            if interrupted {
                return Err(fail(
                    exit::ACTION_REQUIRED,
                    "已停止后续启动；共享代理操作请按原编号查询。",
                ));
            }
            core_cli::outcome(applied)?;
            expanded = true;
        }
        ensure_revision(&root, catalog.revision).await?;
        // Installation/confirmation only resumes this active foreground flow,
        // after the previous attempt was durably proved not to have dispatched.
        id = Uuid::new_v4();
        eprintln!("依赖已就绪，继续启动；之前的失败请求保留原结果。");
    }
}

async fn ensure_revision(root: &std::path::Path, revision: u64) -> Result<(), Failure> {
    let current = coordinator::catalog(root.to_owned())
        .await
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    if current.revision != revision {
        return Err(fail(exit::CONFLICT, "LAUNCH_CONFIG_CHANGED"));
    }
    Ok(())
}

async fn submit(
    root: PathBuf,
    request: LaunchRequest,
    expected_revision: Option<u64>,
    foreground: &mut Foreground,
) -> (Report, bool, bool) {
    let id = request.request_id;
    foreground.remember_launch(id);
    if !foreground.quiet() {
        eprintln!("启动请求编号：{id}；结果不明时运行 launch inspect {id} 查询。");
    }
    let mut report = Report::new(id, None);
    let initial = coordinator::launch_at_revision(root.clone(), request, expected_revision).await;
    let fresh = matches!(&initial, Ok(a) if a.finished_at.is_none() && a.phase.before_spawn());
    match initial {
        Ok(attempt) => report.attempt = Some(attempt),
        Err(app_proxy_windows::Error::Invalid(code))
            if matches!(
                code,
                "REQUEST_ID_CONFLICT"
                    | "INSTANCE_NOT_FOUND"
                    | "INVALID_LAUNCH_REQUEST"
                    | "INSTANCE_RUNNING_WITH_OTHER_CONFIG"
                    | "INSTANCE_RUNNING_IN_OTHER_SESSION"
                    | "LAUNCH_OPERATION_LIMIT"
                    | "LAUNCH_CONFIG_CHANGED"
                    | "INSTANCE_RESOURCE_BUSY"
            ) =>
        {
            report.error = Some(code.into());
            return (report, false, false);
        }
        Err(_) => {
            report.attempt = coordinator::launch_status(root.clone(), id)
                .await
                .ok()
                .flatten()
        }
    }
    let mut deadline = tokio::time::Instant::now() + Duration::from_secs(100);
    let mut interrupted = false;
    while report.attempt.as_ref().is_some_and(|a| {
        a.finished_at.is_none() && !matches!(a.phase, LaunchPhase::Indeterminate {})
    }) && tokio::time::Instant::now() < deadline
    {
        tokio::select! {
            _ = foreground.cancelled(), if !interrupted => {
                    interrupted = true;
                    eprintln!("正在请求取消启动；已创建的应用会保留。");
                    let _ = coordinator::cancel_launch(root.clone(), id).await;
                    deadline = tokio::time::Instant::now() + Duration::from_secs(20);
            }
            _ = tokio::time::sleep(Duration::from_millis(200)) => {}
        }
        report.attempt = coordinator::launch_status(root.clone(), id)
            .await
            .ok()
            .flatten();
    }
    (report, fresh, interrupted || foreground.is_cancelled())
}
