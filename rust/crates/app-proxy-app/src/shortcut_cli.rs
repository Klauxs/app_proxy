//! Shared foreground entry for desktop links; retries always retain request IDs.
use crate::{
    coordinator,
    exit::{self, Failure, fail},
    foreground::Foreground,
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
            .map_err(|_| fail(exit::INTERNAL, "OUTPUT_SERIALIZE_FAILED"))?
        );
    } else {
        match status {
            Some(Status::Created { path, .. }) => {
                println!("已有桌面快捷方式创建记录。\n位置：{}", clean(path))
            }
            Some(Status::Removed { path, .. }) => {
                println!("已移除桌面快捷方式登记。\n位置：{}", clean(path))
            }
            Some(Status::Repaired { path, .. }) => {
                println!("已有桌面快捷方式恢复记录。\n位置：{}", clean(path))
            }
            Some(Status::Cancelled { .. }) => println!("原创建已取消。"),
            Some(Status::Pending { action, path }) => println!(
                "桌面快捷方式尚未完成{}。\n位置：{}",
                match action {
                    Action::Create => "创建",
                    Action::Remove => "删除",
                    Action::Repair => "恢复",
                },
                clean(path)
            ),
            None => println!("暂时无法确认快捷方式的处理结果。"),
        }
        if let Some(code) = error {
            println!("原因：{}", friendly_error(code));
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
        println!("尚未创建桌面快捷方式。");
    }
}
pub(crate) fn print_check(view: &app_proxy_windows::shortcuts::journal::Check) {
    use app_proxy_windows::shortcuts::journal::CheckState;
    println!(
        "桌面入口：{}。",
        match view.state {
            CheckState::Unregistered => "尚未创建",
            CheckState::Pending => "处理未完成，可以继续处理",
            CheckState::Verified => "可用，双击即可启动此实例",
            CheckState::Missing => "链接丢失，可在原位置恢复",
            CheckState::Blocked => "核验受阻，保留现有文件",
        }
    );
    if let Some(code) = &view.diagnostic {
        println!("原因：{}", friendly_error(code));
    }
}
fn diagnostic(error: Error) -> String {
    match error {
        Error::Invalid(code) => code.into(),
        Error::Windows {
            operation: "ShortcutCom",
            code,
        } => format!("SHORTCUT_COM_ERROR:{code}"),
        _ => "SHORTCUT_OPERATION_FAILED".into(),
    }
}
fn friendly_error(code: &str) -> String {
    match code {
        "SHORTCUT_COM_ERROR:2147942487" => "Windows 无法接受快捷方式的路径或参数。".into(),
        "SHORTCUT_FILE_BUSY" => "快捷方式正在被其他程序占用，请稍后重试。".into(),
        "SHORTCUT_PATH_OCCUPIED" => "目标位置已有同名文件，未覆盖；请先移动或改名该文件。".into(),
        "SHORTCUT_CHANGED" | "SHORTCUT_CONTENT_CONFLICT" => {
            "快捷方式已被修改，已保留现有文件。".into()
        }
        "SHORTCUT_OPERATION_BUSY" => "另一项快捷方式操作正在进行，请稍后重试。".into(),
        "SHORTCUT_WAIT_CANCELLED" => "已停止等待，后台操作可能仍在进行。".into(),
        "COORDINATOR_OPERATION_FAILED" | "SHORTCUT_OPERATION_FAILED" => "后台未能完成操作。".into(),
        _ => {
            if let Some(value) = code
                .strip_prefix("SHORTCUT_COM_ERROR:")
                .and_then(|v| v.parse::<u32>().ok())
            {
                format!("Windows 快捷方式接口失败（错误码 0x{value:08X}）。")
            } else {
                format!("操作受阻（{code}）。")
            }
        }
    }
}
fn unresolved() -> Failure {
    fail(
        exit::CONFLICT,
        "请进入“管理实例 → 桌面快捷方式”，选择“继续处理”重试。",
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
        println!("正在处理桌面快捷方式…");
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
            return Err(unresolved());
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
                        return Err(unresolved());
                    },
                    status = coordinator::shortcut_request(root.into(), id) => status.ok().flatten(),
                }
            };
            (status, Some(error))
        }
    };
    if !json && let Some(Status::Created { path, .. } | Status::Repaired { path, .. }) = &status {
        println!("桌面快捷方式已保存。\n位置：{}", clean(path));
    } else {
        print(id, status.as_ref(), error.as_deref(), json)?;
    }
    match status {
        Some(
            Status::Created { .. }
            | Status::Removed { .. }
            | Status::Cancelled { .. }
            | Status::Repaired { .. },
        ) => Ok(()),
        _ => Err(unresolved()),
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
    app_proxy_windows::store::describe(&root)
        .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
    let mut foreground = Foreground::new();
    match command {
        Command::Request { id } => {
            let status = coordinator::shortcut_request(root.clone(), id)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            print(id, status.as_ref(), None, json)
        }
        Command::Resume { id } => perform(&root, id, None, json, &mut foreground).await,
        Command::Check { id } => {
            let view = coordinator::shortcut_check(root, id)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view)
                        .map_err(|_| fail(exit::INTERNAL, "OUTPUT_SERIALIZE_FAILED"))?
                );
            } else {
                print_check(&view);
            }
            Ok(())
        }
        Command::Status { id } => {
            let view = coordinator::shortcut_status(root, id)
                .await
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&view)
                        .map_err(|_| fail(exit::INTERNAL, "OUTPUT_SERIALIZE_FAILED"))?
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
                .map_err(|e| fail(exit::UNAVAILABLE, e.to_string()))?;
            let expected_creation = if action != Action::Create {
                let entry = view
                    .integration
                    .as_ref()
                    .ok_or_else(|| fail(exit::INVALID, "未登记快捷方式。"))?;
                if entry.request.action != Action::Create {
                    return Err(fail(
                        exit::CONFLICT,
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
