#![forbid(unsafe_code)]

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version, about = "AppProxy — 应用实例与代理；无参数打开中文菜单")]
struct Cli {
    /// 数据目录，默认使用当前用户的 AppProxy 目录
    #[arg(long, global = true)]
    home: Option<std::path::PathBuf>,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    #[command(hide = true)]
    SetupPrepare,
    #[command(hide = true)]
    SetupVerify,
    /// 管理桌面快捷方式及中断操作
    Shortcut {
        #[command(subcommand)]
        command: app_proxy_app::shortcut_cli::Command,
        #[arg(long, global = true)]
        json: bool,
    },
    /// 打开中文交互菜单
    Menu,
    /// 启动已登记实例，或查询/取消原启动请求
    Launch(app_proxy_app::launch_cli::Command),
    /// 查看或配置实例保护；授权组件未完成时会明确提示
    Guard {
        #[command(subcommand)]
        command: app_proxy_app::guard_cli::Command,
        #[arg(long, global = true)]
        json: bool,
    },
    /// 管理手动代理和订阅，导入、刷新及选择节点
    Proxy {
        #[command(subcommand)]
        command: app_proxy_app::proxy_cli::Command,
        #[arg(long, global = true)]
        json: bool,
    },
    /// 管理本工具拥有的共享 sing-box 进程
    Core {
        #[command(subcommand)]
        command: app_proxy_app::core_cli::Command,
        #[arg(long, global = true)]
        json: bool,
    },
    /// 管理实例登记；目前不启动应用或安装保护组件
    Instance {
        #[command(subcommand)]
        command: app_proxy_app::instance_cli::Command,
        #[arg(long, global = true)]
        json: bool,
    },
    /// 查询协调进程状态；首次运行创建数据目录
    Status {
        #[arg(long)]
        json: bool,
    },
    /// 显示当前进程身份，确认普通权限运行环境
    Doctor,
    /// 识别已安装的桌面应用或探测 sing-box 程序文件
    Discover {
        #[arg(value_enum)]
        app: Discovery,
    },
    /// 运行隔离的开发验证，不启动真实应用
    Probe {
        #[command(subcommand)]
        command: Probes,
    },
}

#[derive(Clone, ValueEnum)]
enum App {
    Claude,
    Codex,
}
impl App {
    fn name(&self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Clone, ValueEnum)]
enum Discovery {
    Claude,
    Codex,
    SingBox,
}

#[derive(Subcommand)]
enum Probes {
    Process {},
    Package {
        #[arg(value_enum)]
        app: App,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(
            error
                .downcast_ref::<app_proxy_app::exit::Failure>()
                .map_or(1, |e| e.exit_code),
        );
    }
}

fn print(value: &impl serde::Serialize) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Every command that talks to the coordinator runs on the same small runtime.
fn block_on<T>(future: impl std::future::Future<Output = T>) -> std::io::Result<T> {
    Ok(tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(future))
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    use app_proxy_windows::{identity, package};
    identity::assert_ordinary_user()?;
    let cli = Cli::parse();
    if matches!(cli.command, Some(Commands::SetupPrepare)) {
        let root = cli
            .home
            .map(Ok)
            .unwrap_or_else(app_proxy_app::coordinator::default_home)?;
        app_proxy_app::setup::prepare(root)?;
        return Ok(());
    }
    app_proxy_windows::setup::ensure_available()?;
    // Resolved only by the commands that need it: the default location is
    // created on first use, and read-only commands must not create it.
    let explicit_home = cli.home;
    let home = move || {
        explicit_home
            .map(Ok)
            .unwrap_or_else(app_proxy_app::coordinator::default_home)
    };
    match cli.command.unwrap_or(Commands::Menu) {
        Commands::SetupPrepare => unreachable!(),
        Commands::SetupVerify => {
            let root = home()?;
            block_on(app_proxy_app::setup::verify(root))??;
            Ok(())
        }
        Commands::Shortcut { command, json } => {
            let root = home()?;
            block_on(app_proxy_app::shortcut_cli::run(root, command, json))??;
            Ok(())
        }
        Commands::Menu => {
            let root = home()?;
            block_on(app_proxy_app::menu::run(root))??;
            Ok(())
        }
        Commands::Guard { command, json } => {
            let root = home()?;
            block_on(app_proxy_app::guard_cli::run(root, command, json))??;
            Ok(())
        }
        Commands::Launch(command) => {
            let root = home()?;
            block_on(app_proxy_app::launch_cli::run(root, command))??;
            Ok(())
        }
        Commands::Proxy { command, json } => {
            let root = home()?;
            block_on(app_proxy_app::proxy_cli::run(root, command, json))??;
            Ok(())
        }
        Commands::Core { command, json } => {
            let root = home()?;
            block_on(app_proxy_app::core_cli::run(root, command, json))??;
            Ok(())
        }
        Commands::Instance { command, json } => {
            let root = home()?;
            block_on(app_proxy_app::instance_cli::run(root, command, json))??;
            Ok(())
        }
        Commands::Status { json } => {
            let status = block_on(app_proxy_app::coordinator::status(home()?))??;
            if json {
                print(&status)
            } else {
                println!(
                    "协调进程已连接；配置版本 {}，应用 {}，实例 {}，代理 {}。\n无参数可打开菜单，launch 启动已登记实例，guard status 查看保护状态。",
                    status.revision, status.applications, status.instances, status.profiles
                );
                Ok(())
            }
        }
        Commands::Doctor => print(&identity::current()?),
        Commands::Discover {
            app: Discovery::SingBox,
        } => {
            let root = home()?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            let binary = runtime.block_on(app_proxy_windows::singbox_binary::discover(&root))?;
            match binary {
                Some(binary) => print(&serde_json::json!({
                    "found": true, "version": binary.version(), "source": binary.source(),
                    "executable": binary.executable(), "configuration_checked": false,
                    "message": "已找到程序；具体代理配置仍须检查，尚未启动代理进程。"
                })),
                None => print(&serde_json::json!({"found": false,
                    "message": "未找到可用的 sing-box；代理准备流程会提示安装，也可运行 core install。"})),
            }
        }
        Commands::Discover {
            app: Discovery::Claude,
        } => print(&package::discover("claude")?),
        Commands::Discover {
            app: Discovery::Codex,
        } => print(&package::discover("codex")?),
        Commands::Probe {
            command: Probes::Process {},
        } => print(&app_proxy_app::probe::process_probe()?),
        Commands::Probe {
            command: Probes::Package { app },
        } => print(&app_proxy_app::probe::package_probe(app.name())?),
    }
}
