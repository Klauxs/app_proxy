//! Advanced edits use protected input and the existing configuration transaction.
use crate::{
    coordinator,
    foreground::Foreground,
    instance_cli::{self, Failure, fail},
};
use app_proxy_core::{
    model::{Manifest, WorkingDirectory},
    registry::{ConfigAction, EnvironmentAssignment, EnvironmentEdit, InstanceEdit},
};
use app_proxy_windows::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

const INPUT_LIMIT: usize = 128 * 1024;
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Summary {
    pub instance_id: Uuid,
    pub revision: u64,
    pub argument_count: usize,
    pub application_directory: bool,
    pub set_count: usize,
    pub unset_count: usize,
    pub set_names: Vec<String>,
    pub unset_names: Vec<String>,
    pub names_truncated: bool,
}
pub(crate) fn summary(manifest: &Manifest, instance_id: Uuid) -> Result<Summary> {
    let instance = manifest
        .instances
        .iter()
        .find(|i| i.id == instance_id)
        .ok_or(Error::Invalid("INSTANCE_NOT_FOUND"))?;
    let names = |values: Vec<&String>| {
        values
            .into_iter()
            .take(64)
            .map(|s| s.chars().take(256).collect())
            .collect()
    };
    Ok(Summary {
        instance_id,
        revision: manifest.revision,
        argument_count: instance.args.len(),
        application_directory: matches!(instance.cwd, WorkingDirectory::Application {}),
        set_count: instance.env.set.len(),
        unset_count: instance.env.unset.len(),
        set_names: names(instance.env.set.keys().collect()),
        unset_names: names(instance.env.unset.iter().collect()),
        names_truncated: instance.env.set.len() > 64
            || instance.env.unset.len() > 64
            || instance
                .env
                .set
                .keys()
                .chain(&instance.env.unset)
                .any(|s| s.len() > 256),
    })
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    args: Option<Vec<String>>,
    cwd: Option<WorkingDirectory>,
    env: Option<EnvironmentInput>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentInput {
    #[serde(default)]
    set: Vec<AssignmentInput>,
    #[serde(default)]
    unset: Vec<String>,
    #[serde(default)]
    inherit: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AssignmentInput {
    name: String,
    value: String,
}
fn decode(bytes: &[u8]) -> Result<InstanceEdit> {
    if bytes.len() > INPUT_LIMIT {
        return Err(Error::Invalid("INSTANCE_EDIT_TOO_LARGE"));
    }
    let input: Input =
        serde_json::from_slice(bytes).map_err(|_| Error::Invalid("INVALID_INSTANCE_EDIT_FILE"))?;
    Ok(InstanceEdit {
        args: input.args,
        cwd: input.cwd,
        env: input.env.map(|env| EnvironmentEdit {
            set: env
                .set
                .into_iter()
                .map(|entry| EnvironmentAssignment {
                    name: entry.name,
                    value: entry.value,
                    secret_id: Uuid::new_v4(),
                })
                .collect(),
            unset: env.unset,
            inherit: env.inherit,
        }),
    })
}
pub(crate) async fn save(
    root: &Path,
    instance_id: Uuid,
    revision: u64,
    edit: InstanceEdit,
    json: bool,
    foreground: &mut Foreground,
) -> std::result::Result<(), Failure> {
    foreground.check()?;
    let (request_id, receipt) = instance_cli::submit(
        root,
        revision,
        ConfigAction::EditInstance { instance_id, edit },
        json,
    )
    .await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "request_id": request_id, "receipt": receipt, "takes_effect": "next_launch", "application_restarted": false })).map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?);
    } else {
        println!("高级设置已保存，下次启动生效。当前应用未重启。");
    }
    Ok(())
}
pub fn edit_file(root: &Path, id: Uuid, path: &Path, revision: u64) -> Result<InstanceEdit> {
    // No coordinator/store creation before the private input is accepted.
    app_proxy_windows::store::describe(root)?;
    if id.is_nil() || revision == 0 {
        return Err(Error::Invalid("INVALID_INSTANCE_EDIT"));
    }
    decode(&app_proxy_windows::store::read_private_input(
        path,
        INPUT_LIMIT,
    )?)
}
pub async fn show(root: &Path, instance_id: Uuid, json: bool) -> std::result::Result<(), Failure> {
    let view = coordinator::instance_settings(root.into(), instance_id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
        );
    } else {
        display(&view);
    }
    Ok(())
}
pub(crate) fn display(view: &Summary) {
    let names = |values: &[String]| {
        values
            .iter()
            .map(|s| {
                s.chars()
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    println!(
        "启动参数：{} 项；工作目录：{}。",
        view.argument_count,
        if view.application_directory {
            "应用目录"
        } else {
            "自定义目录"
        }
    );
    println!(
        "设置环境变量 {} 项：{}",
        view.set_count,
        names(&view.set_names)
    );
    println!(
        "移除环境变量 {} 项：{}",
        view.unset_count,
        names(&view.unset_names)
    );
    if view.names_truncated {
        println!("变量名称较多或过长，仅显示部分名称。");
    }
    println!("参数和环境值不回显。编辑仅影响下次启动。");
}

#[cfg(test)]
mod tests;
