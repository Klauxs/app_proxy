//! Instance configuration commands. Application launch and protection are separate
//! operations; saving a desired guard must never be reported as active protection.
use crate::{configuration::CatalogPage, coordinator};
use app_proxy_core::{model::*, registry::*};
use app_proxy_windows::{
    config_transaction::{ConfigOutcome, ConfigRequestStatus},
    installation, package,
};
use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Clone, ValueEnum)]
pub enum Preset {
    Codex,
    Claude,
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Adapter {
    Codex,
    Claude,
    Chromium,
    Environment,
}
impl Adapter {
    fn template(self) -> Template {
        match self {
            Self::Codex => Template::Codex,
            Self::Claude => Template::Claude,
            Self::Chromium => Template::Chromium,
            Self::Environment => Template::Environment,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
pub enum Data {
    Original,
    Isolated,
}

#[derive(Args)]
#[group(required = true, multiple = false)]
pub struct Network {
    #[arg(long)]
    pub direct: bool,
    #[arg(long)]
    pub proxy: Option<Uuid>,
}
impl Network {
    fn binding(&self) -> NetworkBinding {
        self.proxy
            .map(|profile_id| NetworkBinding::Profile { profile_id })
            .unwrap_or(NetworkBinding::Direct {})
    }
}

#[derive(Subcommand)]
pub enum Command {
    /// 查看高级设置摘要，不显示参数或环境变量值
    Settings { id: Uuid },
    /// 从受保护 JSON 文件修改高级设置，仅下次启动生效
    Edit {
        id: Uuid,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        revision: u64,
    },
    /// 列出已登记实例；不读取参数、环境值或代理凭据
    List,
    /// 查看实例进程、历史会话及实际保护状态；不启动应用
    Inspect { id: Uuid },
    /// 保存新实例配置；默认原版，分身始终为空白数据
    Create {
        #[arg(long, required_unless_present = "exe", conflicts_with = "exe")]
        preset: Option<Preset>,
        #[arg(long, required_unless_present = "preset", requires = "adapter")]
        exe: Option<PathBuf>,
        #[arg(long, requires = "exe", conflicts_with = "preset")]
        adapter: Option<Adapter>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "original")]
        data: Data,
        #[command(flatten)]
        network: Network,
    },
    /// 复制配置创建空白分身，不复制登录或运行状态
    Clone {
        id: Uuid,
        #[arg(long)]
        name: String,
        #[arg(long, conflicts_with = "proxy")]
        direct: bool,
        #[arg(long)]
        proxy: Option<Uuid>,
    },
    /// 改名，保持实例 ID 与数据目录
    Rename { id: Uuid, name: String },
    /// 修改下次启动使用的代理绑定
    Bind {
        id: Uuid,
        #[command(flatten)]
        network: Network,
    },
    /// 移除登记，保留应用和实例数据
    Remove { id: Uuid },
    /// 查询中断或超时操作的原请求结果
    Request { id: Uuid },
}

#[derive(Debug)]
pub struct Failure {
    pub exit_code: i32,
    message: String,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Failure {}
pub(crate) fn fail(code: i32, message: impl Into<String>) -> Failure {
    Failure {
        exit_code: code,
        message: message.into(),
    }
}
fn dependency(e: app_proxy_windows::Error) -> Failure {
    fail(3, e.to_string())
}
fn print(value: &impl Serialize) -> Result<(), Failure> {
    println!(
        "{}",
        serde_json::to_string_pretty(value).map_err(|_| fail(10, "OUTPUT_ENCODING_FAILED"))?
    );
    Ok(())
}
fn display(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub async fn run(root: PathBuf, command: Command, json: bool) -> Result<(), Failure> {
    if let Command::Settings { id } = command {
        return crate::instance_settings::show(&root, id, json).await;
    }
    if let Command::Edit { id, file, revision } = command {
        let edit =
            crate::instance_settings::edit_file(&root, id, &file, revision).map_err(dependency)?;
        return crate::instance_settings::save(
            &root,
            id,
            revision,
            edit,
            json,
            &mut crate::foreground::Foreground::new(),
        )
        .await;
    }
    if let Command::Inspect { id } = command {
        return crate::instance_status::inspect(&root, id, json).await;
    }
    if let Command::Request { id } = command {
        let result = coordinator::request_status(root, id)
            .await
            .map_err(dependency)?;
        if json {
            print(&serde_json::json!({"request_id":id,"result":result}))?;
        } else {
            println!(
                "{}",
                match &result {
                    None => "没有找到此请求（可能未接受或已过保留期）。",
                    Some(ConfigRequestStatus::Pending {}) => "请求仍未完成，请保留原请求编号。",
                    Some(ConfigRequestStatus::Complete {
                        outcome: ConfigOutcome::Applied { .. },
                    }) => "配置已保存。",
                    Some(ConfigRequestStatus::Complete {
                        outcome: ConfigOutcome::Rejected { .. },
                    }) => "请求被拒绝，配置未因本请求改变。",
                }
            );
        }
        return match result {
            Some(ConfigRequestStatus::Complete {
                outcome: ConfigOutcome::Applied { .. },
            }) => Ok(()),
            Some(ConfigRequestStatus::Complete {
                outcome: ConfigOutcome::Rejected { code, .. },
            }) => Err(fail(rejection_exit(&code), code)),
            _ => Err(fail(6, "请求结果未确认；请查询原编号，不要自动重复创建。")),
        };
    }
    if matches!(command, Command::List) {
        let catalog = coordinator::catalog(root.clone())
            .await
            .map_err(dependency)?;
        if json {
            print(&catalog)?;
        } else {
            println!(
                "配置版本 {}；以下为登记信息，运行及保护实际状态未检查，可使用 guard status 查询。",
                catalog.revision
            );
            for instance in &catalog.instances {
                let network = match instance.network {
                    NetworkBinding::Direct {} => "直连".into(),
                    NetworkBinding::Profile { profile_id } => format!("代理 {profile_id}"),
                };
                println!(
                    "{}  {}  {}  {}",
                    instance.id,
                    display(&instance.name),
                    if instance.isolated {
                        "分身"
                    } else {
                        "原版"
                    },
                    network
                );
            }
            if catalog.instances.is_empty() {
                println!("尚无登记实例。");
            }
        }
        return Ok(());
    }
    let (request_id, receipt, check_guard) = save(
        &root,
        command,
        json,
        None,
        &mut crate::foreground::Foreground::new(),
    )
    .await?;
    report_saved(root, request_id, receipt, check_guard, json).await
}

/// Menu and CLI share creation, installation resolution and durable writes.
/// A menu supplies the revision shown in its confirmation summary.
pub(crate) async fn save(
    root: &std::path::Path,
    command: Command,
    json: bool,
    expected_revision: Option<u64>,
    foreground: &mut crate::foreground::Foreground,
) -> Result<(Uuid, ConfigReceipt, bool), Failure> {
    foreground.check()?;
    let mut catalog = coordinator::catalog(root.into())
        .await
        .map_err(dependency)?;
    if expected_revision.is_some_and(|revision| revision != catalog.revision) {
        return Err(fail(4, "配置已变化，请重新确认。"));
    }
    let action = match command {
        Command::Create {
            preset,
            exe,
            adapter,
            name,
            data,
            network,
        } => {
            let (locator, template, app_name) = match preset {
                Some(preset) => {
                    let (key, template, title) = match preset {
                        Preset::Codex => ("codex", Template::Codex, "Codex"),
                        Preset::Claude => ("claude", Template::Claude, "Claude"),
                    };
                    let package = package::discover(key).map_err(dependency)?;
                    (
                        ApplicationLocator::Msix {
                            family_name: package.family_name,
                            app_id: package.app_id,
                        },
                        template,
                        title.to_owned(),
                    )
                }
                None => {
                    let path = exe.ok_or_else(|| fail(2, "EXE_REQUIRED"))?;
                    let name = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("应用")
                        .to_owned();
                    (
                        ApplicationLocator::Exe { path },
                        adapter
                            .ok_or_else(|| fail(2, "ADAPTER_REQUIRED"))?
                            .template(),
                        name,
                    )
                }
            };
            if matches!(data, Data::Isolated) && !template.supports_isolation() {
                return Err(fail(2, "ISOLATION_UNSUPPORTED"));
            }
            let name = name.unwrap_or_else(|| {
                format!(
                    "{} {}",
                    app_name,
                    if matches!(data, Data::Original) {
                        "原版"
                    } else {
                        "分身"
                    }
                )
            });
            if name.trim().is_empty() || name.contains('\0') {
                return Err(fail(2, "INVALID_INSTANCE_NAME"));
            }
            check_profile(&catalog, network.binding())?;
            let resolved = installation::resolve(&locator).map_err(dependency)?;
            let storage = storage(root, &resolved)?;
            let mut existing = None;
            for application in catalog
                .applications
                .iter()
                .filter(|a| a.template_ref == template)
            {
                if application.locator == locator {
                    existing = Some(application.id);
                    break;
                }
                if let Ok(other) = installation::resolve(&application.locator)
                    && resolved.same_installation(&other)
                {
                    existing = Some(application.id);
                    break;
                }
            }
            drop(resolved);
            let application_id = if let Some(id) = existing {
                id
            } else {
                let application = Application {
                    id: Uuid::new_v4(),
                    name: app_name,
                    revision: 1,
                    locator,
                    template_ref: template,
                };
                foreground.check()?;
                let (_, receipt) = submit(
                    root,
                    catalog.revision,
                    ConfigAction::AddApplication { application },
                    json,
                )
                .await?;
                catalog.revision = receipt.revision;
                receipt.entity_id
            };
            ConfigAction::CreateInstance {
                instance: NewInstance {
                    id: Uuid::new_v4(),
                    application_id,
                    name,
                    data: match data {
                        Data::Original => NewData::Original {},
                        Data::Isolated => NewData::Isolated { storage },
                    },
                    network: network.binding(),
                    guard: None,
                    args: vec![],
                    env: SavedEnvironment::default(),
                    cwd: WorkingDirectory::Application {},
                },
            }
        }
        Command::Clone {
            id,
            name,
            direct,
            proxy,
        } => {
            let instance = catalog
                .instances
                .iter()
                .find(|i| i.id == id)
                .ok_or_else(|| fail(2, "INSTANCE_NOT_FOUND"))?;
            let application = catalog
                .applications
                .iter()
                .find(|a| a.id == instance.application_id)
                .ok_or_else(|| fail(2, "APPLICATION_NOT_FOUND"))?;
            let resolved = installation::resolve(&application.locator).map_err(dependency)?;
            let network = proxy
                .map(|profile_id| NetworkBinding::Profile { profile_id })
                .or(if direct {
                    Some(NetworkBinding::Direct {})
                } else {
                    None
                });
            if let Some(binding) = network {
                check_profile(&catalog, binding)?;
            }
            ConfigAction::CloneInstance {
                source_id: id,
                instance_id: Uuid::new_v4(),
                name,
                storage: storage(root, &resolved)?,
                network,
                guard: None,
            }
        }
        Command::Rename { id, name } => ConfigAction::RenameInstance {
            instance_id: id,
            name,
        },
        Command::Bind { id, network } => {
            check_profile(&catalog, network.binding())?;
            ConfigAction::BindInstance {
                instance_id: id,
                network: network.binding(),
                guard: None,
            }
        }
        Command::Remove { id } => ConfigAction::RemoveInstance { instance_id: id },
        Command::Request { .. }
        | Command::List
        | Command::Inspect { .. }
        | Command::Edit { .. }
        | Command::Settings { .. } => {
            return Err(fail(2, "CONFIGURATION_EDIT_REQUIRED"));
        }
    };
    let check_guard = matches!(
        &action,
        ConfigAction::CreateInstance { .. }
            | ConfigAction::CloneInstance { .. }
            | ConfigAction::BindInstance { .. }
    );
    foreground.check()?;
    let (request_id, receipt) = submit(root, catalog.revision, action, json).await?;
    Ok((request_id, receipt, check_guard))
}

async fn report_saved(
    root: PathBuf,
    request_id: Uuid,
    receipt: ConfigReceipt,
    check_guard: bool,
    json: bool,
) -> Result<(), Failure> {
    let mut protection = None;
    if check_guard {
        match coordinator::guard_status(root.clone(), receipt.entity_id).await {
            Ok(status)
                if status.desired == Desired::Enabled
                    && status.phase != crate::guard_control::GuardPhase::Active =>
            {
                let requires_action = status.phase.required_action();
                if json {
                    print(
                        &serde_json::json!({"request_id":request_id,"receipt":receipt,"application_started":false,
                        "protection":status,"requires_action":requires_action}),
                    )?;
                } else {
                    println!(
                        "实例 {} 已保存，Guard 尚未完全就绪；可用 guard status 查看检查进度和诊断，或用 guard enable 授权缺少的监听组件。",
                        receipt.entity_id
                    );
                }
                return Err(fail(5, "实例已保存；保护需要前台组件授权或核验。"));
            }
            Ok(status) => protection = Some(status),
            Err(_) => {
                if json {
                    print(
                        &serde_json::json!({"request_id":request_id,"receipt":receipt,"application_started":false,
                        "protection":"unknown","requires_action":"query_guard_status"}),
                    )?;
                }
                return Err(fail(
                    6,
                    "实例配置已保存；保护状态暂未确认，请查询 guard status，不要重复创建。",
                ));
            }
        }
    }
    if json {
        print(
            &serde_json::json!({"request_id":request_id,"receipt":receipt,"application_started":false,"protection":protection}),
        )
    } else {
        println!(
            "配置已保存：实例 {}，版本 {}。应用未启动；保护实际状态可使用 guard status 查询。",
            receipt.entity_id, receipt.revision
        );
        Ok(())
    }
}

fn storage(
    root: &Path,
    resolved: &installation::ResolvedApplication,
) -> Result<NewStorage, Failure> {
    if resolved.package().is_some_and(|p| p.isolated_storage)
        && !app_proxy_windows::layout::shared_package_files(root).map_err(dependency)?
    {
        Ok(NewStorage::PackageLocalState)
    } else {
        Ok(NewStorage::Store)
    }
}
fn check_profile(catalog: &CatalogPage, binding: NetworkBinding) -> Result<(), Failure> {
    if let NetworkBinding::Profile { profile_id } = binding
        && !catalog.profiles.iter().any(|p| p.id == profile_id)
    {
        return Err(fail(2, "PROFILE_NOT_FOUND"));
    }
    Ok(())
}
fn rejection_exit(code: &str) -> i32 {
    match code {
        "STALE_MANIFEST_REVISION"
        | "DUPLICATE_ORIGINAL"
        | "DUPLICATE_PHYSICAL_ORIGINAL"
        | "DUPLICATE_PHYSICAL_APPLICATION" => 4,
        "APP_NOT_INSTALLED"
        | "INSTALLATION_CHECK_FAILED"
        | "INSTALLATION_ACCESS_DENIED"
        | "AMBIGUOUS_PACKAGE" => 3,
        "INTEGRATION_CLEANUP_REQUIRED" | "CORE_RECONFIGURATION_REQUIRED" => 5,
        _ => 2,
    }
}
pub(crate) async fn submit(
    root: &Path,
    revision: u64,
    action: ConfigAction,
    json: bool,
) -> Result<(Uuid, ConfigReceipt), Failure> {
    let request_id = Uuid::new_v4();
    let result = coordinator::configure(
        root.into(),
        ConfigRequest {
            request_id,
            expected_revision: revision,
            action,
        },
    )
    .await;
    match result {
        Ok(ConfigOutcome::Applied { receipt }) => Ok((request_id, receipt)),
        Ok(outcome @ ConfigOutcome::Rejected { .. }) => {
            if json {
                print(&serde_json::json!({"request_id":request_id,"outcome":outcome}))?;
            }
            let ConfigOutcome::Rejected { code, .. } = outcome else {
                unreachable!()
            };
            let exit = rejection_exit(&code);
            let message = if code == "CORE_RECONFIGURATION_REQUIRED" {
                "该代理仍属于共享内核的当前配置；需确认影响后重配置。此流程尚未接入，当前配置保持不变。".into()
            } else {
                code
            };
            Err(fail(exit, message))
        }
        Err(_) => Err(fail(
            6,
            format!("结果未确认，请查询原请求：instance request {request_id}；不要自动重新创建。"),
        )),
    }
}
