//! Explicit core controls. A timeout only queries the original request ID.
use crate::{
    coordinator,
    core_manager::CoreObserved,
    instance_cli::{Failure, fail},
};
use app_proxy_core::core_control::{CoreAction, CoreOutcome, CoreRequestStatus};
use clap::Subcommand;
use std::{path::PathBuf, time::Duration};
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
    /// 查看本工具的进程及监听状态，不执行网络健康检查
    Status,
    /// 查询原请求的历史结果，不会重新执行启动或停止
    Request { id: Uuid },
}

fn output(id: Uuid, status: &Option<CoreRequestStatus>, json: bool) -> Result<(), Failure> {
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
                Some(CoreRequestStatus::Pending {}) => "仍在执行。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Ready { .. },
                    ..
                }) => "该请求已完成启动与健康检查；当前状态请使用 core status。",
                Some(CoreRequestStatus::Complete {
                    outcome: CoreOutcome::Stopped {},
                    ..
                }) => "该请求已完成停止。",
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

fn outcome(status: Option<CoreRequestStatus>) -> Result<(), Failure> {
    match status {
        Some(CoreRequestStatus::Complete {
            outcome: CoreOutcome::Ready { .. } | CoreOutcome::Stopped {},
            ..
        }) => Ok(()),
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
        Command::Start { profiles, required } => {
            let required = required
                .or_else(|| profiles.first().copied())
                .ok_or_else(|| fail(2, "PROFILE_REQUIRED"))?;
            CoreAction::Start { profiles, required }
        }
    };
    let mut action = action;
    action.normalize().map_err(|e| fail(2, e.0))?;
    let id = Uuid::new_v4();
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
    while matches!(status, Some(CoreRequestStatus::Pending {}))
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
        status = coordinator::core_request_status(root.clone(), id)
            .await
            .ok()
            .flatten();
    }
    output(id, &status, json)?;
    outcome(status)
}
