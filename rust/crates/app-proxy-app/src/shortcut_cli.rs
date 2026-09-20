//! Shared foreground entry for desktop links; retries always retain request IDs.
use crate::{
    coordinator,
    foreground::Foreground,
    instance_cli::{Failure, fail},
    shortcuts::InstanceStatus,
};
use app_proxy_windows::{
    Error,
    shortcuts::journal::{Action, Request, Status},
};
use clap::Subcommand;
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Command {
    /// 在当前用户桌面创建实例入口，路径与图标自动选择
    Create { id: Uuid },
    /// 移除本工具登记的入口；用户改动保留并提示冲突
    Remove { id: Uuid },
    /// 查看实例入口登记及未完成请求，不操作桌面文件
    Status { id: Uuid },
    /// 核验登记、链接文件、启动器及图标；不修复或启动应用
    Check { id: Uuid },
    /// 在原位置恢复丢失的已登记入口；被修改的文件保留
    Repair { id: Uuid },
    /// 查询原请求，不自动继续创建或删除
    Request { id: Uuid },
    /// 按原请求显式恢复创建或删除
    Resume { id: Uuid },
}
#[derive(Serialize)]
struct Report<'a> {
    request_id: Uuid,
    status: Option<&'a Status>,
    error: Option<&'a str>,
}
fn clean(value: &Path) -> String {
    value
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn print(
    id: Uuid,
    status: Option<&Status>,
    error: Option<&str>,
    json: bool,
) -> Result<(), Failure> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&Report {
                request_id: id,
                status,
                error
            })
            .map_err(|_| fail(10, "OUTPUT_SERIALIZE_FAILED"))?
        );
    } else {
        println!("快捷方式请求：{id}");
        match status {
            Some(Status::Created { path, .. }) => {
                println!("创建已完成：{}（历史结果，当前文件未核验）。", clean(path))
            }
            Some(Status::Removed { path, .. }) => println!("入口登记已解除：{}。", clean(path)),
            Some(Status::Repaired { path, .. }) => {
                println!("入口恢复已完成：{}（历史结果）。", clean(path))
            }
            Some(Status::Cancelled { .. }) => println!("原创建已取消。"),
            Some(Status::Pending { action, path }) => println!(
                "{}待核对：{}。",
                match action {
                    Action::Create => "创建",
                    Action::Remove => "删除",
                    Action::Repair => "恢复",
                },
                clean(path)
            ),
            None => println!("尚未查到持久结果；不据此推断仍在准备的操作已取消。"),
        }
        if let Some(code) = error {
            println!("操作诊断：{code}");
        }
    }
    Ok(())
}
pub(crate) fn print_registration(view: &InstanceStatus) {
    if let Some(registration) = &view.integration {
        let _ = print(
            registration.request.id,
            Some(&registration.status),
            None,
            false,
        );
    } else {
        println!("尚未登记桌面快捷方式。");
    }
}
pub(crate) fn print_check(view: &app_proxy_windows::shortcuts::journal::Check) {
    use app_proxy_windows::shortcuts::journal::CheckState;
    println!(
        "桌面入口：{}。",
        match view.state {
            CheckState::Unregistered => "未登记",
            CheckState::Pending => "有未完成操作，请查询或继续原请求",
            CheckState::Verified => "链接、启动器文件及图标已核验；未启动应用",
            CheckState::Missing => "链接丢失，可在原位置恢复",
            CheckState::Blocked => "核验受阻，保留现有文件",
        }
    );
    if let Some(id) = view.request_id {
        println!("关联请求：{id}");
    }
    if let Some(code) = &view.diagnostic {
        println!("入口诊断：{code}");
    }
}
fn diagnostic(error: Error) -> &'static str {
    match error {
        Error::Invalid(code) => code,
        _ => "SHORTCUT_OPERATION_FAILED",
    }
}
fn unresolved(id: Uuid, root: &Path) -> Failure {
    fail(
        4,
        format!(
            "快捷方式操作尚未确认。请求：{id}；数据目录：{}。请用 shortcut request {id} 查询，或在管理实例的桌面快捷方式菜单中核对后继续。",
            clean(root)
        ),
    )
}
async fn perform(
    root: &Path,
    id: Uuid,
    request: Option<Request>,
    json: bool,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    foreground.check()?;
    if !json {
        println!("正在处理快捷方式，请求：{id}");
    }
    let operation = async {
        match request {
            Some(request) => coordinator::shortcut_apply(root.into(), request).await,
            None => coordinator::shortcut_resume(root.into(), id).await,
        }
    };
    let result = tokio::select! {
        biased;
        _ = foreground.cancelled() => {
            print(id, None, Some("SHORTCUT_WAIT_CANCELLED"), json)?;
            return Err(unresolved(id, root));
        },
        result = operation => result,
    };
    let (status, error) = match result {
        Ok(status) => (Some(status), None),
        Err(error) => {
            let error = diagnostic(error);
            // The original request may have completed after losing its reply.
            // An ID conflict is never interpreted as this operation's success.
            let status = if error == "REQUEST_ID_CONFLICT" {
                None
            } else {
                tokio::select! {
                    biased;
                    _ = foreground.cancelled() => {
                        print(id, None, Some("SHORTCUT_WAIT_CANCELLED"), json)?;
                        return Err(unresolved(id, root));
                    },
                    status = coordinator::shortcut_request(root.into(), id) => status.ok().flatten(),
                }
            };
            (status, Some(error))
        }
    };
    print(id, status.as_ref(), error, json)?;
    match status {
        Some(
            Status::Created { .. }
            | Status::Removed { .. }
            | Status::Cancelled { .. }
            | Status::Repaired { .. },
        ) => Ok(()),
        _ => Err(unresolved(id, root)),
    }
}
pub(crate) async fn change(
    root: &Path,
    instance_id: Uuid,
    revision: u64,
    action: Action,
    expected_creation: Option<Uuid>,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    let request = Request {
        id: Uuid::new_v4(),
        instance_id,
        expected_revision: revision,
        action,
        expected_creation,
    };
    perform(root, request.id, Some(request), false, foreground).await
}
pub(crate) async fn resume(
    root: &Path,
    request_id: Uuid,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    perform(root, request_id, None, false, foreground).await
}
pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    app_proxy_windows::store::describe(&root).map_err(|e| fail(3, e.to_string()))?;
    let mut foreground = Foreground::new();
    match command {
        Command::Request { id } => {
            let status = coordinator::shortcut_request(root.clone(), id)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            print(id, status.as_ref(), None, json)
        }
        Command::Resume { id } => perform(&root, id, None, json, &mut foreground).await,
        Command::Check { id } => {
            let view = coordinator::shortcut_check(root, id)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view)
                        .map_err(|_| fail(10, "OUTPUT_SERIALIZE_FAILED"))?
                );
            } else {
                print_check(&view);
            }
            Ok(())
        }
        Command::Status { id } => {
            let view = coordinator::shortcut_status(root, id)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view)
                        .map_err(|_| fail(10, "OUTPUT_SERIALIZE_FAILED"))?
                );
            } else {
                print_registration(&view);
            }
            Ok(())
        }
        Command::Create { id } | Command::Remove { id } | Command::Repair { id } => {
            let action = match command {
                Command::Create { .. } => Action::Create,
                Command::Repair { .. } => Action::Repair,
                _ => Action::Remove,
            };
            let view = coordinator::shortcut_status(root.clone(), id)
                .await
                .map_err(|e| fail(3, e.to_string()))?;
            let expected_creation = if action != Action::Create {
                let entry = view
                    .integration
                    .as_ref()
                    .ok_or_else(|| fail(2, "未登记快捷方式。"))?;
                if entry.request.action != Action::Create {
                    return Err(fail(
                        4,
                        format!(
                            "已有未完成请求；请使用 shortcut resume {} 继续。",
                            entry.request.id
                        ),
                    ));
                }
                Some(entry.request.id)
            } else {
                None
            };
            let request = Request {
                id: Uuid::new_v4(),
                instance_id: id,
                expected_revision: view.revision,
                action,
                expected_creation,
            };
            perform(&root, request.id, Some(request), json, &mut foreground).await
        }
    }
}
