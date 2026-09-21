use crate::{
    coordinator,
    exit::{self, Failure, fail},
    foreground::Foreground,
    login_tasks::View,
};
use app_proxy_windows::guard_task::login::journal::{Action, Request, Status};
use clap::Subcommand;
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Command {
    /// 核验本数据目录的登录启动入口，不修改系统任务
    Status,
    /// 查询原请求的历史结果
    Request { id: Uuid },
    /// 显式继续原登录入口请求
    Resume { id: Uuid },
    /// 删除本工具登记的空闲登录入口，保留应用与实例配置
    Remove,
}
#[derive(Serialize)]
pub(crate) struct Outcome {
    pub request_id: Uuid,
    pub status: Option<Status>,
    pub error: Option<String>,
}
pub(crate) fn print_view(view: &View) {
    println!(
        "守护进程随登录启动（所有实例共用）：{}。",
        if view.ready {
            "已开启"
        } else if view.integration.is_none() && view.diagnostic.is_none() {
            "未登记"
        } else {
            "待处理"
        }
    );
    if let Some(entry) = &view.integration
        && matches!(entry.status, Status::Pending { .. })
    {
        println!(
            "未完成请求 {}；可用 guard login resume {} 继续原操作。",
            entry.request.id, entry.request.id
        );
    }
    if let Some(code) = &view.diagnostic {
        println!("登录入口诊断：{code}。");
    }
}
fn print_outcome(outcome: &Outcome, json: bool) -> Result<(), Failure> {
    if json {
        crate::output::json(outcome)?;
    } else {
        println!("登录入口请求：{}。", outcome.request_id);
        match &outcome.status {
            Some(Status::Created { .. }) => {
                println!("登记已完成（历史结果，当前就绪状态需另行查询）。")
            }
            Some(Status::Removed { .. }) => println!("登录入口已移除。"),
            Some(Status::Cancelled {}) => println!("原创建已取消。"),
            _ => println!(
                "结果尚未确认；请用 guard login request {} 查询，或 guard login resume {} 继续原操作。",
                outcome.request_id, outcome.request_id
            ),
        }
        if let Some(code) = &outcome.error {
            println!("登录入口诊断：{code}。");
        }
    }
    Ok(())
}
pub(crate) async fn perform(
    root: &Path,
    id: Uuid,
    request: Option<Request>,
    foreground: &mut Foreground,
) -> Result<Outcome, Failure> {
    foreground.check()?;
    let operation = async {
        match request {
            Some(request) => coordinator::login_apply(root.into(), request).await,
            None => coordinator::login_resume(root.into(), id).await,
        }
    };
    let result = tokio::select! {
        biased;
        _ = foreground.cancelled() => return Ok(Outcome { request_id: id, status: None, error: Some("GUARD_LOGIN_WAIT_CANCELLED".into()) }),
        result = operation => result,
    };
    let (status, error) = match result {
        Ok(status) => (Some(status), None),
        Err(error) => {
            let code = match error {
                app_proxy_windows::Error::Invalid(code) => code,
                _ => "GUARD_LOGIN_OPERATION_UNCONFIRMED",
            };
            let status = if code == "REQUEST_ID_CONFLICT" {
                None
            } else {
                tokio::select! {
                    biased;
                    _ = foreground.cancelled() => None,
                    result = coordinator::login_request(root.into(), id) => result.ok().flatten(),
                }
            };
            (status, Some(code.into()))
        }
    };
    Ok(Outcome {
        request_id: id,
        status,
        error,
    })
}
pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    let mut foreground = Foreground::new();
    let outcome = match command {
        Command::Status => {
            let view = coordinator::login_status(root)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            if json {
                crate::output::json(&view)?;
            } else {
                print_view(&view);
            }
            return Ok(());
        }
        Command::Request { id } => {
            let status = coordinator::login_request(root, id)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            return print_outcome(
                &Outcome {
                    request_id: id,
                    status,
                    error: None,
                },
                json,
            );
        }
        Command::Resume { id } => perform(&root, id, None, &mut foreground).await?,
        Command::Remove => {
            let view = coordinator::login_status(root.clone())
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            let entry = view
                .integration
                .ok_or_else(|| fail(exit::INVALID, "未登记登录启动入口。"))?;
            if entry.request.action == Action::Remove {
                return Err(fail(
                    exit::CONFLICT,
                    format!(
                        "已有未完成删除请求，请用 guard login resume {} 继续。",
                        entry.request.id
                    ),
                ));
            }
            let request = Request {
                id: Uuid::new_v4(),
                expected_revision: view.revision,
                action: Action::Remove,
                expected_creation: Some(entry.request.id),
            };
            perform(&root, request.id, Some(request), &mut foreground).await?
        }
    };
    print_outcome(&outcome, json)?;
    if matches!(
        outcome.status,
        Some(Status::Created { .. } | Status::Removed { .. } | Status::Cancelled {})
    ) {
        Ok(())
    } else {
        Err(fail(
            exit::UNCONFIRMED,
            format!(
                "登录入口操作尚未确认，保留原请求 {}，请先查询结果。",
                outcome.request_id
            ),
        ))
    }
}
