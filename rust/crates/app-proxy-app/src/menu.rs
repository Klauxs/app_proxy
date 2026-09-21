//! Daily terminal workflow; all writes and launches use the existing services.
use crate::{
    configuration::{CatalogPage, InstanceSummary, ProfileProtocol},
    coordinator, core_cli,
    foreground::Foreground,
    guard_cli,
    guard_control::{GuardPhase, GuardStatus},
    instance_cli::{self, Adapter, Data, Failure, Network, Preset, fail},
    launch_cli,
    launch_engine::GuardObservation,
    proxy_cli, subscription_cli,
};
use app_proxy_core::{
    model::{Desired, ManualProtocol, NetworkBinding, Template},
    registry::{ConfigAction, ManualProxyInput, ProxyCredentialInput},
};
use std::{
    io::{self, IsTerminal, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

fn display(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}
fn returned() -> Failure {
    fail(5, "已返回，保留已保存的配置。")
}
async fn catalog(root: &Path) -> Result<CatalogPage, Failure> {
    coordinator::catalog(root.into())
        .await
        .map_err(|e| fail(3, e.to_string()))
}
fn prompt(text: &str) -> Result<(), Failure> {
    eprint!("{text}");
    io::stderr()
        .flush()
        .map_err(|_| fail(10, "PROMPT_WRITE_FAILED"))
}
async fn line(text: &str, foreground: &mut Foreground) -> Result<String, Failure> {
    prompt(text)?;
    foreground.read_optional_line().await?.ok_or_else(returned)
}
async fn text(
    text: &str,
    default: Option<&str>,
    foreground: &mut Foreground,
) -> Result<String, Failure> {
    let value = line(text, foreground).await?;
    let value = value.trim();
    if value.is_empty() {
        return default.map(str::to_owned).ok_or_else(returned);
    }
    if value.len() > 32768 || value.chars().any(char::is_control) {
        return Err(fail(2, "输入过长或包含控制字符。"));
    }
    Ok(value.into())
}

/// None is an explicit return/EOF; defaults apply only to an actual empty line.
fn selection(
    input: Option<&str>,
    count: usize,
    default: Option<usize>,
) -> Result<Option<usize>, ()> {
    let Some(input) = input else {
        return Ok(None);
    };
    let input = input.trim();
    if input.is_empty() {
        return Ok(default);
    }
    if input == "0" {
        return Ok(None);
    }
    input
        .parse::<usize>()
        .ok()
        .filter(|n| *n > 0 && *n <= count)
        .map(|n| Some(n - 1))
        .ok_or(())
}
async fn choose(
    title: &str,
    options: &[String],
    default: Option<usize>,
    foreground: &mut Foreground,
) -> Result<Option<usize>, Failure> {
    println!("\n{title}");
    for (index, option) in options.iter().enumerate() {
        println!(
            "{}. {}{}",
            index + 1,
            option,
            if default == Some(index) {
                "（默认）"
            } else {
                ""
            }
        );
        if option.contains('\n') {
            println!();
        }
    }
    println!("0. 返回");
    loop {
        prompt("请选择：")?;
        let input = foreground.read_optional_line().await?;
        if let Ok(value) = selection(input.as_deref(), options.len(), default) {
            return Ok(value);
        }
        println!("请输入列表中的编号。");
    }
}
async fn fixed(
    title: &str,
    options: &[&str],
    default: Option<usize>,
    foreground: &mut Foreground,
) -> Result<Option<usize>, Failure> {
    choose(
        title,
        &options.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        default,
        foreground,
    )
    .await
}
async fn confirm(title: &str, foreground: &mut Foreground) -> Result<(), Failure> {
    if fixed(title, &["确认", "返回"], Some(1), foreground).await? == Some(0) {
        Ok(())
    } else {
        Err(returned())
    }
}
fn network_label(catalog: &CatalogPage, network: NetworkBinding) -> String {
    match network {
        NetworkBinding::Direct {} => "直连".into(),
        NetworkBinding::Profile { profile_id } => catalog
            .profiles
            .iter()
            .find(|p| p.id == profile_id)
            .map(|p| format!("代理 {}", display(&p.name)))
            .unwrap_or_else(|| "代理记录不可用".into()),
    }
}
fn network_field(catalog: &CatalogPage, network: NetworkBinding) -> String {
    match network {
        NetworkBinding::Direct {} => "网络：直连".into(),
        NetworkBinding::Profile { profile_id } => format!(
            "代理：{}",
            catalog
                .profiles
                .iter()
                .find(|p| p.id == profile_id)
                .map(|p| display(&p.name))
                .unwrap_or_else(|| "代理记录不可用".into())
        ),
    }
}
fn instance_label(catalog: &CatalogPage, instance: &InstanceSummary) -> String {
    let app = catalog
        .applications
        .iter()
        .find(|a| a.id == instance.application_id)
        .map(|a| display(&a.name))
        .unwrap_or_else(|| "应用记录不可用".into());
    format!(
        "{}\n   应用：{}\n   数据：{}\n   {}",
        display(&instance.name),
        app,
        if instance.isolated {
            "分身/独立数据"
        } else {
            "原版"
        },
        network_field(catalog, instance.network)
    )
}
fn observation(status: Option<&GuardStatus>, revision: u64) -> (&'static str, &'static str) {
    let Some(status) = status.filter(|s| s.revision == revision) else {
        return ("未确认", "未确认");
    };
    let running = match status.scan.as_ref().map(|s| &s.observation) {
        Some(
            GuardObservation::Session { .. }
            | GuardObservation::Compliant { .. }
            | GuardObservation::Correction { .. },
        ) => "已观察到主进程",
        Some(GuardObservation::Absent {}) => "未发现主进程",
        Some(GuardObservation::Pending { .. }) => "启动请求待确认",
        _ => "未确认",
    };
    let guard = match status.phase {
        GuardPhase::Disabled => "已关闭",
        GuardPhase::NeedsAuthorization => "待授权",
        GuardPhase::Starting => "检查中",
        GuardPhase::Active => "运行中",
        GuardPhase::Degraded => "降级",
        GuardPhase::Blocked => "受阻",
    };
    (running, guard)
}

pub async fn run(root: PathBuf) -> Result<(), Failure> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() || !io::stderr().is_terminal() {
        return Err(fail(2, "菜单需要交互终端；自动化请使用 --help 中的命令。"));
    }
    let mut foreground = Foreground::new();
    println!("App Proxy · 应用实例与代理");
    loop {
        foreground.check()?;
        let Some(action) = fixed(
            "主菜单",
            &[
                "启动实例",
                "添加实例",
                "管理实例",
                "代理配置",
                "共享代理状态",
            ],
            None,
            &mut foreground,
        )
        .await?
        else {
            return Ok(());
        };
        let result = match action {
            0 => launch(&root, &mut foreground).await,
            1 => add_instance(&root, &mut foreground).await,
            2 => manage_instance(&root, &mut foreground).await,
            3 => proxies(&root, &mut foreground).await,
            4 => core_cli::run(root.clone(), core_cli::Command::Status, false).await,
            _ => unreachable!(),
        };
        // A cancelled reader/UAC wait must never be followed by another input
        // thread or action. The whole foreground process exits instead.
        foreground.check()?;
        if let Err(error) = result {
            eprintln!("{error}");
        }
    }
}

async fn select_instance(
    root: &Path,
    foreground: &mut Foreground,
    with_status: bool,
) -> Result<(CatalogPage, Uuid), Failure> {
    let snapshot = catalog(root).await?;
    if snapshot.instances.is_empty() {
        return Err(fail(2, "尚无实例，请先添加。"));
    }
    let mut options = Vec::new();
    if with_status {
        println!("正在读取实例及保护状态…");
    }
    for instance in &snapshot.instances {
        foreground.check()?;
        // Choosing what to launch needs configuration only. Read-only process
        // observations belong to management, not the ordinary launch path.
        if !with_status {
            options.push(instance_label(&snapshot, instance));
            continue;
        }
        let status = coordinator::guard_status(root.into(), instance.id)
            .await
            .ok();
        let (running, guard) = observation(status.as_ref(), snapshot.revision);
        let runtime = if running == "未确认" {
            coordinator::runtime_status(root.into(), instance.id)
                .await
                .ok()
        } else {
            None
        };
        let running = runtime.as_ref().map_or(running, |s| {
            crate::instance_status::label(s, snapshot.revision)
        });
        options.push(format!(
            "{}\n   进程：{running}\n   保护：{guard}",
            instance_label(&snapshot, instance)
        ));
    }
    let selected = choose("选择实例", &options, None, foreground)
        .await?
        .ok_or_else(returned)?;
    let id = snapshot.instances[selected].id;
    Ok((snapshot, id))
}
async fn launch(root: &Path, foreground: &mut Foreground) -> Result<(), Failure> {
    let (snapshot, id) = select_instance(root, foreground, false).await?;
    launch_confirmed(root, &snapshot, id, foreground).await
}
async fn launch_confirmed(
    root: &Path,
    snapshot: &CatalogPage,
    id: Uuid,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    let instance = snapshot
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| fail(4, "实例已变化，请重新选择。"))?;
    println!("将启动：{}", instance_label(snapshot, instance));
    confirm("启动此实例？", foreground).await?;
    launch_cli::from_menu(root.into(), id, snapshot.revision, foreground).await
}

async fn choose_network(
    root: &Path,
    foreground: &mut Foreground,
) -> Result<NetworkBinding, Failure> {
    let snapshot = catalog(root).await?;
    let mut options: Vec<String> = snapshot
        .profiles
        .iter()
        .map(|p| format!("代理 {}", display(&p.name)))
        .collect();
    options.push("明确使用直连".into());
    options.push("添加代理".into());
    let choice = choose("实例网络", &options, None, foreground)
        .await?
        .ok_or_else(returned)?;
    if choice < snapshot.profiles.len() {
        Ok(NetworkBinding::Profile {
            profile_id: snapshot.profiles[choice].id,
        })
    } else if choice == snapshot.profiles.len() {
        Ok(NetworkBinding::Direct {})
    } else {
        Ok(NetworkBinding::Profile {
            profile_id: add_proxy(root, foreground).await?,
        })
    }
}
fn network_input(network: NetworkBinding) -> Network {
    match network {
        NetworkBinding::Direct {} => Network {
            direct: true,
            proxy: None,
        },
        NetworkBinding::Profile { profile_id } => Network {
            direct: false,
            proxy: Some(profile_id),
        },
    }
}
async fn prepare_network(
    root: &Path,
    network: NetworkBinding,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    if let NetworkBinding::Profile { profile_id } = network {
        println!("验证所选代理；失败会保留代理配置，暂不创建实例。");
        core_cli::ensure_instance_profile(root.into(), profile_id, foreground).await?;
    }
    foreground.check()
}

fn automatic_instance_name(snapshot: &CatalogPage, title: &str, isolated: bool) -> String {
    if !isolated {
        return format!("{title} 原版");
    }
    for number in 1.. {
        let name = format!("{title} 分身 {number}");
        if !snapshot
            .instances
            .iter()
            .any(|instance| instance.name == name)
        {
            return name;
        }
    }
    unreachable!()
}

async fn add_instance(root: &Path, foreground: &mut Foreground) -> Result<(), Failure> {
    let app = fixed(
        "选择应用",
        &["Codex（自动识别）", "Claude（自动识别）", "其他应用"],
        None,
        foreground,
    )
    .await?
    .ok_or_else(returned)?;
    let (preset, exe, adapter, title) = match app {
        0 => (Some(Preset::Codex), None, None, "Codex".into()),
        1 => (Some(Preset::Claude), None, None, "Claude".into()),
        _ => {
            let path = text("应用 EXE 完整路径（空白返回）：", None, foreground).await?;
            let path = PathBuf::from(
                path.strip_prefix('"')
                    .and_then(|p| p.strip_suffix('"'))
                    .unwrap_or(&path),
            );
            let selected = fixed(
                "应用类型",
                &[
                    "Chromium / Electron（不支持分身）",
                    "Codex",
                    "Claude",
                    "其他（环境变量代理，不支持分身）",
                ],
                None,
                foreground,
            )
            .await?
            .ok_or_else(returned)?;
            let adapter = [
                Adapter::Chromium,
                Adapter::Codex,
                Adapter::Claude,
                Adapter::Environment,
            ][selected];
            let title = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("应用")
                .to_owned();
            (None, Some(path), Some(adapter), title)
        }
    };
    let data_options: &[&str] = if matches!(adapter, Some(Adapter::Environment | Adapter::Chromium))
    {
        &["原版（默认数据）"]
    } else {
        &["原版（默认数据）", "空白分身（独立数据，不复制登录）"]
    };
    let isolated = fixed("实例数据", data_options, Some(0), foreground)
        .await?
        .ok_or_else(returned)?
        == 1;
    let network = choose_network(root, foreground).await?;
    let snapshot = catalog(root).await?;
    let name = automatic_instance_name(&snapshot, &title, isolated);
    println!(
        "\n添加实例：{}\n   数据：{}\n   {}",
        display(&name),
        if isolated { "空白分身" } else { "原版" },
        network_field(&snapshot, network)
    );
    let guarded = matches!(network, NetworkBinding::Profile { .. })
        && (preset.is_some() || matches!(adapter, Some(Adapter::Codex | Adapter::Claude)));
    if guarded {
        println!("默认开启保护；授权后会检查并纠正不符合代理要求的误启动主进程。");
        if !isolated {
            println!("此记录将管理原版；Guard 在启动后检查，误启动可能被关闭并通过代理重启。");
        } else {
            println!("只管理这个分身，不接管未登记的原版。");
        }
    }
    confirm("保存此实例？", foreground).await?;
    prepare_network(root, network, foreground).await?;
    let (_, receipt, _) = instance_cli::save(
        root,
        instance_cli::Command::Create {
            preset,
            exe,
            adapter,
            name: Some(name),
            data: if isolated {
                Data::Isolated
            } else {
                Data::Original
            },
            network: network_input(network),
        },
        false,
        Some(snapshot.revision),
        foreground,
    )
    .await?;
    finish_instance(root, receipt.entity_id, receipt.revision, foreground).await
}
async fn finish_instance(
    root: &Path,
    id: Uuid,
    saved_revision: u64,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    foreground.check()?;
    let snapshot = catalog(root).await?;
    if snapshot.revision != saved_revision {
        return Err(fail(4, "实例已保存，但配置随后发生变化；请重新选择实例。"));
    }
    let instance = snapshot
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| fail(4, "实例已变化。"))?;
    println!("已保存：{}。", display(&instance.name));
    // Publish the entry before Guard authorization or the launch prompt, so
    // returning from either still leaves an entry for the saved instance.
    if crate::shortcut_cli::change(
        root,
        id,
        snapshot.revision,
        app_proxy_windows::shortcuts::journal::Action::Create,
        None,
        foreground,
    )
    .await
    .is_err()
    {
        foreground.check()?;
        println!("实例已保留，桌面入口尚未就绪；可在“管理实例 → 桌面快捷方式”中重试。");
    }
    foreground.check()?;
    // Creating the link commits a new manifest revision. Guard must use that
    // revision, rather than the receipt from saving the instance itself.
    let snapshot = catalog(root).await?;
    let instance = snapshot
        .instances
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| fail(4, "实例已变化。"))?;
    let protection = if instance.guard == Desired::Enabled {
        guard_cli::run_with_foreground(
            root.into(),
            guard_cli::Command::Enable { id },
            false,
            foreground,
            Some(snapshot.revision),
        )
        .await?
    } else {
        None
    };
    foreground.check()?;
    let current = catalog(root).await?;
    if let Some(status) = protection
        && let Some(message) = post_guard_launch_message(&status, id, current.revision)?
    {
        println!("{message}");
        return Ok(());
    }
    launch_confirmed(root, &current, id, foreground).await
}

// Reuse the observation already obtained by guard enable. Do not add a process
// scan to the normal launch path or interpret an active listener as a running app.
fn post_guard_launch_message(
    status: &GuardStatus,
    id: Uuid,
    revision: u64,
) -> Result<Option<&'static str>, Failure> {
    if status.instance_id != id || status.revision != revision {
        return Err(fail(4, "实例已保存，但配置随后发生变化；请重新选择实例。"));
    }
    let scan = status.scan.as_ref().filter(|scan| {
        scan.instance_id == id && scan.revision == revision && status.desired == Desired::Enabled
    });
    Ok(match scan.map(|scan| &scan.observation) {
        Some(GuardObservation::Session { .. } | GuardObservation::Compliant { .. }) => {
            Some("实例已在运行，返回主菜单。")
        }
        Some(GuardObservation::Pending { .. }) => Some("实例正在启动，无需重复启动，返回主菜单。"),
        Some(GuardObservation::Correction { .. }) => {
            Some("保护正在处理代理纠正，无需手动启动，返回主菜单。")
        }
        Some(GuardObservation::Absent {}) => None,
        _ => Some("实例已保存，运行状态暂未确认；可在管理实例中查看。"),
    })
}

async fn manage_instance(root: &Path, foreground: &mut Foreground) -> Result<(), Failure> {
    let (snapshot, id) = select_instance(root, foreground, true).await?;
    let instance = snapshot.instances.iter().find(|i| i.id == id).unwrap();
    let action = fixed(
        "管理实例",
        &[
            "详情与保护状态",
            "启动",
            "复制配置创建空白分身",
            "更换网络绑定",
            "改名",
            "启用或修复保护",
            "关闭保护",
            "移除登记（保留数据）",
            "桌面快捷方式",
            "高级设置（下次启动生效）",
        ],
        None,
        foreground,
    )
    .await?
    .ok_or_else(returned)?;
    match action {
        0 => crate::instance_status::inspect(root, id, false).await,
        1 => launch_confirmed(root, &snapshot, id, foreground).await,
        2 => {
            let application = snapshot
                .applications
                .iter()
                .find(|a| a.id == instance.application_id)
                .ok_or_else(|| fail(4, "应用记录不可用，请重新选择。"))?;
            if !application.template_ref.supports_isolation() {
                return Err(fail(2, "此应用类型不支持分身。"));
            }
            let name = automatic_instance_name(&snapshot, &application.name, true);
            println!(
                "创建：{}。继承应用、参数和网络：{}；使用独立空白数据，不复制登录。",
                display(&name),
                network_label(&snapshot, instance.network)
            );
            if matches!(application.template_ref, Template::Codex | Template::Claude)
                && matches!(instance.network, NetworkBinding::Profile { .. })
            {
                println!(
                    "新分身默认开启保护，不继承源实例的关闭状态；已有授权时，保存后即会检查并纠正误启动主进程。只管理此分身，不接管未登记的原版。"
                );
            }
            confirm("创建空白分身？", foreground).await?;
            prepare_network(root, instance.network, foreground).await?;
            let (_, receipt, _) = instance_cli::save(
                root,
                instance_cli::Command::Clone {
                    id,
                    name,
                    direct: false,
                    proxy: None,
                },
                false,
                Some(snapshot.revision),
                foreground,
            )
            .await?;
            finish_instance(root, receipt.entity_id, receipt.revision, foreground).await
        }
        3 => {
            let network = choose_network(root, foreground).await?;
            let current = catalog(root).await?;
            let target = current
                .instances
                .iter()
                .find(|i| i.id == id)
                .ok_or_else(|| fail(4, "实例已变化，请重新选择。"))?;
            println!("实例：{}", instance_label(&current, target));
            println!(
                "下次启动生效：{}。当前应用进程不会被本次编辑重启。",
                network_label(&current, network)
            );
            confirm("修改网络绑定？", foreground).await?;
            let (_, receipt, _) = instance_cli::save(
                root,
                instance_cli::Command::Bind {
                    id,
                    network: network_input(network),
                },
                false,
                Some(current.revision),
                foreground,
            )
            .await?;
            println!("网络绑定已保存：{}。", receipt.entity_id);
            let current = catalog(root).await?;
            if current.revision != receipt.revision {
                return Err(fail(
                    4,
                    "网络绑定已保存，但配置随后发生变化；请重新选择实例。",
                ));
            }
            if current
                .instances
                .iter()
                .any(|i| i.id == id && i.guard == Desired::Enabled)
            {
                guard_cli::run_with_foreground(
                    root.into(),
                    guard_cli::Command::Enable { id },
                    false,
                    foreground,
                    Some(current.revision),
                )
                .await?;
            }
            Ok(())
        }
        4 => {
            let name = text("新名称（空白返回）：", None, foreground).await?;
            confirm(&format!("改名为 {}？", display(&name)), foreground).await?;
            instance_cli::save(
                root,
                instance_cli::Command::Rename { id, name },
                false,
                Some(snapshot.revision),
                foreground,
            )
            .await?;
            println!("名称已保存。");
            Ok(())
        }
        5 | 6 => {
            confirm(
                if action == 5 {
                    "启用或修复此实例保护？可能需要 Windows 授权。"
                } else {
                    "关闭此实例保护？"
                },
                foreground,
            )
            .await?;
            guard_cli::run_with_foreground(
                root.into(),
                if action == 5 {
                    guard_cli::Command::Enable { id }
                } else {
                    guard_cli::Command::Disable { id }
                },
                false,
                foreground,
                Some(snapshot.revision),
            )
            .await
            .map(|_| ())
        }
        7 => {
            confirm(
                "移除此实例登记及其桌面快捷方式？保留应用及数据，不关闭应用。",
                foreground,
            )
            .await?;
            remove_instance(root, id, snapshot.revision, foreground).await
        }
        8 => manage_shortcut(root, id, foreground).await,
        9 => advanced_settings(root, id, foreground).await,
        _ => unreachable!(),
    }
}

async fn remove_instance(
    root: &Path,
    id: Uuid,
    revision: u64,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    use app_proxy_windows::shortcuts::journal::Action;
    foreground.check()?;
    let view = coordinator::shortcut_status(root.into(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    if view.revision != revision {
        return Err(fail(4, "配置已变化，请重新确认移除。"));
    }
    if let Some(entry) = view.integration {
        let result = match entry.request.action {
            Action::Create => {
                crate::shortcut_cli::change(
                    root,
                    id,
                    view.revision,
                    Action::Remove,
                    Some(entry.request.id),
                    foreground,
                )
                .await
            }
            Action::Remove => {
                // Continue an interrupted removal with its original request.
                crate::shortcut_cli::resume(root, entry.request.id, foreground).await
            }
            Action::Repair => {
                return Err(fail(
                    4,
                    "实例尚未移除：桌面入口有未完成的恢复操作，请先在“桌面快捷方式”中处理。",
                ));
            }
        };
        foreground.check()?;
        if result.is_err() {
            return Err(fail(
                4,
                "实例尚未移除：桌面入口清理未完成，请按上方原因处理后重试。",
            ));
        }
    }
    // Shortcut removal can commit a new revision. Re-read its registration
    // before removing the instance, keeping the core's ownership guard intact.
    let current = coordinator::shortcut_status(root.into(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    if current.integration.is_some() {
        return Err(fail(4, "实例尚未移除：桌面入口登记已变化，请重新选择。"));
    }
    instance_cli::save(
        root,
        instance_cli::Command::Remove { id },
        false,
        Some(current.revision),
        foreground,
    )
    .await?;
    println!("实例登记已移除；应用及数据已保留。");
    Ok(())
}

async fn advanced_settings(
    root: &Path,
    id: Uuid,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    use app_proxy_core::{
        model::WorkingDirectory,
        registry::{EnvironmentAssignment, EnvironmentEdit, InstanceEdit},
    };
    let summary = coordinator::instance_settings(root.into(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    crate::instance_settings::display(&summary);
    let action = fixed(
        "高级设置",
        &[
            "替换启动参数",
            "修改工作目录",
            "设置环境变量",
            "从子进程移除环境变量",
            "恢复环境变量继承",
        ],
        None,
        foreground,
    )
    .await?
    .ok_or_else(returned)?;
    let mut edit = InstanceEdit::default();
    match action {
        0 => {
            prompt("输入参数 JSON 数组（不回显；[] 清空；EOF 返回）：")?;
            let input = foreground
                .read_secret_line(128 * 1024)
                .await?
                .ok_or_else(returned)?;
            let args: Vec<String> =
                serde_json::from_str(&input).map_err(|_| fail(2, "参数须为 JSON 字符串数组。"))?;
            println!("将替换为 {} 项启动参数。", args.len());
            edit.args = Some(args);
        }
        1 => {
            let choice = fixed("工作目录", &["应用所在目录", "指定目录"], None, foreground)
                .await?
                .ok_or_else(returned)?;
            edit.cwd = Some(if choice == 0 {
                WorkingDirectory::Application {}
            } else {
                let path =
                    text("绝对路径或已支持的路径变量（空白返回）：", None, foreground).await?;
                WorkingDirectory::Explicit { path: path.into() }
            });
        }
        2..=4 => {
            let name = text("环境变量名称（空白返回）：", None, foreground).await?;
            let mut env = EnvironmentEdit::default();
            match action {
                2 => {
                    prompt("变量值（不回显；回车为空值，EOF 返回）：")?;
                    let value = foreground
                        .read_secret_line(65536)
                        .await?
                        .ok_or_else(returned)?;
                    env.set.push(EnvironmentAssignment {
                        name: name.clone(),
                        value,
                        secret_id: Uuid::new_v4(),
                    });
                    println!("设置变量 {}。", display(&name));
                }
                3 => {
                    println!("从子进程环境中移除 {}。", display(&name));
                    env.unset.push(name);
                }
                _ => {
                    println!("恢复 {} 的启动环境继承值。", display(&name));
                    env.inherit.push(name);
                }
            }
            edit.env = Some(env);
        }
        _ => unreachable!(),
    }
    confirm("保存高级设置？下次启动生效，当前应用保持运行。", foreground).await?;
    crate::instance_settings::save(root, id, summary.revision, edit, false, foreground).await
}

async fn manage_shortcut(
    root: &Path,
    id: Uuid,
    foreground: &mut Foreground,
) -> Result<(), Failure> {
    use app_proxy_windows::shortcuts::journal::{Action, Status};
    let view = coordinator::shortcut_status(root.into(), id)
        .await
        .map_err(|e| fail(3, e.to_string()))?;
    crate::shortcut_cli::print_registration(&view);
    match view.integration {
        None => {
            fixed("在桌面创建此实例的快捷方式", &["创建"], None, foreground)
                .await?
                .ok_or_else(returned)?;
            crate::shortcut_cli::change(root, id, view.revision, Action::Create, None, foreground)
                .await
        }
        Some(entry) => {
            if let Status::Pending { action, .. } = entry.status {
                let options = match action {
                    Action::Create => vec!["继续处理（重试创建）", "取消创建"],
                    Action::Remove => vec!["继续处理（重试移除）"],
                    Action::Repair => vec!["继续处理（重试恢复）"],
                };
                let choice = fixed("未完成的快捷方式操作", &options, None, foreground)
                    .await?
                    .ok_or_else(returned)?;
                if choice == 0 {
                    return crate::shortcut_cli::resume(root, entry.request.id, foreground).await;
                }
                confirm("取消刚显示的创建请求？用户修改过的文件会保留。", foreground).await?;
            } else {
                let check = coordinator::shortcut_check(root.into(), id)
                    .await
                    .map_err(|e| fail(3, e.to_string()))?;
                crate::shortcut_cli::print_check(&check);
                let action = fixed(
                    "桌面入口维护",
                    &["恢复丢失的快捷方式", "移除桌面入口"],
                    None,
                    foreground,
                )
                .await?
                .ok_or_else(returned)?;
                if action == 0 {
                    if check.revision != view.revision || check.request_id != Some(entry.request.id)
                    {
                        return Err(fail(4, "入口登记已变化，请重新选择。"));
                    }
                    confirm(
                        "在原位置恢复入口？已有链接须保持原样，启动器和图标须仍可用。",
                        foreground,
                    )
                    .await?;
                    return crate::shortcut_cli::change(
                        root,
                        id,
                        view.revision,
                        Action::Repair,
                        Some(entry.request.id),
                        foreground,
                    )
                    .await;
                }
                confirm("移除此实例的桌面入口？保留应用及数据。", foreground).await?;
            }
            crate::shortcut_cli::change(
                root,
                id,
                view.revision,
                Action::Remove,
                Some(entry.request.id),
                foreground,
            )
            .await
        }
    }
}

async fn manual_input(foreground: &mut Foreground) -> Result<ManualProxyInput, Failure> {
    let protocol = if fixed("手动上游协议", &["HTTP", "SOCKS5"], Some(0), foreground)
        .await?
        .ok_or_else(returned)?
        == 0
    {
        ManualProtocol::Http
    } else {
        ManualProtocol::Socks5
    };
    let host = text("上游服务器（空白返回）：", None, foreground).await?;
    let port = text("上游端口（空白返回）：", None, foreground)
        .await?
        .parse::<u16>()
        .ok()
        .filter(|p| *p > 0)
        .ok_or_else(|| fail(2, "端口须为 1–65535。"))?;
    let authenticated = fixed(
        "认证方式（明确选择）",
        &["无需认证", "输入用户名和密码"],
        None,
        foreground,
    )
    .await?
    .ok_or_else(returned)?
        == 1;
    let credentials = if authenticated {
        let username = text("用户名（空白返回）：", None, foreground).await?;
        prompt("密码（不回显；回车表示空密码，EOF 返回）：")?;
        let password = foreground
            .read_secret_line(32768)
            .await?
            .ok_or_else(returned)?;
        Some(ProxyCredentialInput { username, password })
    } else {
        None
    };
    println!(
        "上游：{} {}:{}\n认证：{}",
        match protocol {
            ManualProtocol::Http => "HTTP",
            ManualProtocol::Socks5 => "SOCKS5",
        },
        display(&host),
        port,
        if authenticated {
            "使用认证"
        } else {
            "无认证"
        }
    );
    Ok(ManualProxyInput {
        protocol,
        host,
        port,
        credentials,
    })
}
async fn add_proxy(root: &Path, foreground: &mut Foreground) -> Result<Uuid, Failure> {
    subscription_cli::run_with_foreground(
        root.into(),
        proxy_cli::Command::Import {
            name: None,
            url_stdin: false,
            node: vec![],
            via: None,
            apply_to_running: false,
        },
        false,
        foreground,
    )
    .await?
    .ok_or_else(|| fail(6, "订阅保存结果未确认。"))
}
async fn add_manual_proxy(root: &Path, foreground: &mut Foreground) -> Result<Uuid, Failure> {
    let snapshot = catalog(root).await?;
    let name = text("代理名称（空白返回）：", None, foreground).await?;
    let node = manual_input(foreground).await?;
    confirm("保存此代理配置？", foreground).await?;
    proxy_cli::save_manual(
        root.into(),
        snapshot,
        proxy_cli::ManualEdit::Create { name },
        node,
        false,
        false,
        foreground,
    )
    .await
}
async fn proxies(root: &Path, foreground: &mut Foreground) -> Result<(), Failure> {
    let action = fixed(
        "代理配置",
        &["添加代理（订阅）", "管理已有代理", "手动添加 HTTP / SOCKS5"],
        None,
        foreground,
    )
    .await?
    .ok_or_else(returned)?;
    if action == 0 {
        add_proxy(root, foreground).await?;
        return Ok(());
    }
    if action == 2 {
        add_manual_proxy(root, foreground).await?;
        return Ok(());
    }
    let snapshot = catalog(root).await?;
    if snapshot.profiles.is_empty() {
        return Err(fail(2, "尚无代理配置。"));
    }
    let options = snapshot
        .profiles
        .iter()
        .map(|p| format!("{}\n   上游：{}", display(&p.name), p.upstream_label()))
        .collect::<Vec<_>>();
    let selected = choose("选择代理", &options, None, foreground)
        .await?
        .ok_or_else(returned)?;
    let profile = &snapshot.profiles[selected];
    let id = profile.id;
    let subscription = matches!(profile.protocol, ProfileProtocol::Subscription(_));
    let labels = if subscription {
        vec![
            "节点列表",
            "选择节点",
            "刷新订阅",
            "改名",
            "移除配置（保留凭据）",
        ]
    } else {
        vec!["查看", "替换上游及认证", "改名", "移除配置（保留凭据）"]
    };
    let selected = fixed("管理代理", &labels, None, foreground)
        .await?
        .ok_or_else(returned)?;
    if subscription && selected <= 2 {
        let command = match selected {
            0 => proxy_cli::Command::Nodes { id },
            1 => proxy_cli::Command::Select {
                id,
                node: vec![],
                apply_to_running: false,
            },
            _ => proxy_cli::Command::Refresh {
                id,
                via: None,
                apply_to_running: false,
            },
        };
        subscription_cli::run_with_foreground(root.into(), command, false, foreground).await?;
        return Ok(());
    }
    if !subscription && selected == 0 {
        println!(
            "代理：{}\n代理编号：{id}\n本地入口：{}:{}\n认证：{}",
            display(&profile.name),
            profile.endpoint.host,
            profile.endpoint.port,
            if profile.authenticated {
                "已配置认证"
            } else {
                "无认证"
            }
        );
        return Ok(());
    }
    if !subscription && selected == 1 {
        println!("本次替换完整上游和认证；请重新明确选择认证方式。");
        let node = manual_input(foreground).await?;
        confirm("替换此代理？", foreground).await?;
        proxy_cli::save_manual(
            root.into(),
            snapshot,
            proxy_cli::ManualEdit::Update { id },
            node,
            false,
            false,
            foreground,
        )
        .await?;
        return Ok(());
    }
    let action = if selected == labels.len() - 2 {
        let name = text("新名称（空白返回）：", None, foreground).await?;
        confirm(&format!("改名为 {}？", display(&name)), foreground).await?;
        ConfigAction::RenameProfile {
            profile_id: id,
            name,
        }
    } else {
        confirm(
            "移除此代理配置？仍被实例或下载设置引用时将拒绝；运行中的入口会另行预览切换影响。",
            foreground,
        )
        .await?;
        return proxy_cli::remove(root.into(), snapshot.revision, id, false, false, foreground)
            .await;
    };
    foreground.check()?;
    instance_cli::submit(root, snapshot.revision, action, false).await?;
    println!("代理配置已保存。");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observed(observation: GuardObservation) -> GuardStatus {
        let id = Uuid::new_v4();
        GuardStatus {
            instance_id: id,
            revision: 7,
            desired: Desired::Enabled,
            phase: GuardPhase::Active,
            listener: crate::guard_control::ComponentState::ActiveEtw,
            diagnostic: None,
            scan: Some(crate::launch_engine::GuardScan {
                instance_id: id,
                revision: 7,
                observation,
            }),
        }
    }

    #[test]
    fn finished_guard_launch_skips_confirmation_but_absent_application_keeps_it() {
        let process = app_proxy_windows::identity::current().unwrap();
        for observation in [
            GuardObservation::Session {
                process: process.clone(),
                network: app_proxy_core::launch::LaunchNetwork::Direct {},
            },
            GuardObservation::Compliant { process },
        ] {
            let status = observed(observation);
            assert_eq!(
                post_guard_launch_message(&status, status.instance_id, 7).unwrap(),
                Some("实例已在运行，返回主菜单。")
            );
        }
        let absent = observed(GuardObservation::Absent {});
        assert!(
            post_guard_launch_message(&absent, absent.instance_id, 7)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pending_guard_work_never_offers_a_second_launch() {
        let correction = serde_json::from_value(serde_json::json!({
            "state":"correction",
            "target":{
                "process": app_proxy_windows::identity::current().unwrap(),
                "endpoint":{"host":"127.0.0.1", "port":18099}
            }
        }))
        .unwrap();
        for observation in [
            GuardObservation::Pending {
                attempt_id: Uuid::new_v4(),
            },
            correction,
        ] {
            let status = observed(observation);
            assert!(
                post_guard_launch_message(&status, status.instance_id, 7)
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn listener_readiness_and_stale_observations_are_not_running_evidence() {
        let mut status = observed(GuardObservation::Absent {});
        assert!(post_guard_launch_message(&status, Uuid::new_v4(), 7).is_err());
        assert!(post_guard_launch_message(&status, status.instance_id, 8).is_err());
        status.scan.as_mut().unwrap().revision = 6;
        let unknown = "实例已保存，运行状态暂未确认；可在管理实例中查看。";
        assert_eq!(
            post_guard_launch_message(&status, status.instance_id, 7).unwrap(),
            Some(unknown)
        );
        status.scan = None;
        assert_eq!(
            post_guard_launch_message(&status, status.instance_id, 7).unwrap(),
            Some(unknown)
        );
    }

    #[test]
    fn defaults_require_a_line_and_never_convert_eof_into_a_choice() {
        assert_eq!(selection(None, 2, Some(1)), Ok(None));
        assert_eq!(selection(Some("\n"), 2, Some(0)), Ok(Some(0)));
        assert_eq!(selection(Some("0"), 2, Some(0)), Ok(None));
        assert_eq!(selection(Some("2"), 2, None), Ok(Some(1)));
        assert_eq!(selection(Some("3"), 2, Some(0)), Err(()));
        assert_eq!(selection(Some("18446744073709551616"), 2, None), Err(()));
    }
}
