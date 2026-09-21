#![forbid(unsafe_code)]
#![windows_subsystem = "windows"]

#[cfg(feature = "bundle")]
use app_proxy_setup::FilePayload;
use app_proxy_setup::{Installation, Payload};
use app_proxy_windows::{Error, Result, setup};
use std::{
    io::Write,
    os::windows::process::CommandExt,
    path::Path,
    process::{Command, Stdio},
};
include!(concat!(env!("OUT_DIR"), "/payload.rs"));

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let quiet = args.iter().any(|a| a == "--yes");
    let mut log = match tempfile::Builder::new()
        .prefix("AppProxy-setup-")
        .suffix(".log")
        .tempfile()
        .and_then(|f| f.keep().map_err(|e| e.error))
    {
        Ok((file, path)) => (file, path),
        Err(_) => {
            let _ = setup::dialog("无法创建安装日志，安装尚未开始。", false, true);
            std::process::exit(1);
        }
    };
    let result = run(&args, &mut log.0);
    if let Err(error) = result {
        let _ = writeln!(log.0, "failed: {error}");
        let _ = log.0.sync_all();
        if !quiet {
            let _ = setup::dialog(
                &format!(
                    "安装未完成：{error}\n\n已有配置与应用数据保留。关闭 AppProxy 菜单后，可重新运行同一个安装包继续。\n日志：{}",
                    log.1.display()
                ),
                false,
                true,
            );
        }
        std::process::exit(1);
    }
}

fn run(args: &[String], log: &mut std::fs::File) -> Result<()> {
    app_proxy_windows::identity::assert_ordinary_user()?;
    if args.iter().any(|a| a != "--yes" && a != "--no-launch") {
        return Err(Error::Invalid(
            "仅支持 --yes 和 --no-launch；安装目录由程序确定。",
        ));
    }
    let payload = PAYLOAD.as_ref().ok_or(Error::Invalid(
        "这是未嵌入发行文件的开发构建，请使用 scripts/package.ps1 生成安装包。",
    ))?;
    let root = setup::install_directory()?;
    let home = app_proxy_windows::instance_data::local_app_data()?.join("AppProxy");
    if home.try_exists()? && app_proxy_windows::store::describe(&home).is_err() {
        return Err(Error::Invalid(
            "数据目录 %LOCALAPPDATA%\\AppProxy 已有其他版本或无法核验的数据。未覆盖任何数据；请先备份并处理目录冲突，再重新安装。",
        ));
    }
    let quiet = args.iter().any(|a| a == "--yes");
    let action = if root.join(".app-proxy-install.json").exists() {
        "升级或修复"
    } else {
        "安装"
    };
    if !quiet
        && !setup::dialog(
            &format!(
                "{action} AppProxy\n版本：{}\n安装位置：{}\n\n请先退出 AppProxy 菜单。安装器会等待后台完成当前工作；已有应用和代理进程保留。\n更新监听组件时，Windows 会请求管理员授权。\n\n是否继续？",
                payload.build,
                root.display()
            ),
            true,
            false,
        )?
    {
        return Ok(());
    }
    writeln!(log, "build={} target={}", payload.build, root.display())?;
    let progress = if quiet {
        None
    } else {
        Some(setup::Progress::open()?)
    };
    if let Some(p) = &progress {
        p.stage("正在准备安装，等待后台退出；请关闭 AppProxy 菜单。")?;
    }
    let parent = root
        .parent()
        .ok_or(Error::Invalid("SETUP_PARENT_REQUIRED"))?;
    if !parent.try_exists()? {
        std::fs::create_dir(parent)?;
    }
    // Never adopt a preexisting arbitrary installation directory.
    if root.try_exists()?
        && !root.join(".app-proxy-install.json").try_exists()?
        && !root.join(".app-proxy-upgrade.json").try_exists()?
    {
        for entry in std::fs::read_dir(&root)? {
            let name = entry?.file_name();
            if name != ".app-proxy-update.lock" && name != ".app-proxy-setup.lock" {
                return Err(Error::Invalid("固定安装目录已有其他文件，未覆盖。"));
            }
        }
    }
    let _root_pin = setup::prepare_directory(&root)?;
    writeln!(log, "draining and publishing executable pair")?;
    log.sync_all()?;
    let mut transaction = Installation::begin(&root, payload)?;
    if let Some(p) = &progress {
        p.stage("程序已保存，正在配置监听组件；请留意 Windows 授权窗口。")?;
    }
    writeln!(
        log,
        "executables published; updating listener and login entry"
    )?;
    log.sync_all()?;
    run_frontend(&root, "setup-prepare", log)?;
    app_proxy_windows::shortcuts::install_menu_entry(&root.join("app-proxy.exe"))?;
    transaction.release_for_verification();
    if let Some(p) = &progress {
        p.stage("正在核验后台与事件监听状态…")?;
    }
    writeln!(log, "verifying coordinator and listener")?;
    run_frontend(&root, "setup-verify", log)?;
    transaction.complete()?;
    writeln!(log, "complete")?;
    log.sync_all()?;
    drop(progress);
    if !quiet {
        let launch = setup::dialog(
            &format!(
                "AppProxy 安装完成。\n位置：{}\n开始菜单入口：AppProxy\n\n打开 AppProxy？",
                root.display()
            ),
            true,
            false,
        )?;
        if launch && !args.iter().any(|a| a == "--no-launch") {
            setup::open_frontend(&root.join("app-proxy.exe"))?;
        }
    }
    Ok(())
}

fn run_frontend(root: &Path, operation: &str, log: &mut std::fs::File) -> Result<()> {
    let output = Command::new(root.join("app-proxy.exe"))
        .arg(operation)
        .current_dir(root)
        .stdin(Stdio::null())
        .creation_flags(0x08000000)
        .output()?;
    writeln!(log, "{operation}: {}", output.status)?;
    log.write_all(&output.stdout)?;
    log.write_all(&output.stderr)?;
    log.sync_all()?;
    if !output.status.success() {
        return Err(Error::Invalid(
            "程序已保存，但后台组件配置或核验未完成。请查看日志并重新运行此安装包。",
        ));
    }
    Ok(())
}
