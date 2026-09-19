#![forbid(unsafe_code)]

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(version, about = "App Proxy Rust — M0 平台验证；尚未提供日常启动菜单")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// 显示当前进程身份，确认普通权限运行环境
    Doctor,
    /// 只读识别已安装的桌面应用
    Discover {
        #[arg(value_enum)]
        app: App,
    },
    /// 运行隔离的开发验证，不注册 IFEO、不启动真实应用
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

#[derive(Subcommand)]
enum Probes {
    Process {
        #[arg(long)]
        debug_detach: bool,
    },
    Package {
        #[arg(value_enum)]
        app: App,
    },
}

fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

fn print(value: &impl serde::Serialize) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    use app_proxy_windows::{identity, package};
    identity::assert_ordinary_user()?;
    match Cli::parse().command {
        Commands::Doctor => print(&identity::current()?),
        Commands::Discover { app } => print(&package::discover(app.name())?),
        Commands::Probe {
            command: Probes::Process { debug_detach },
        } => print(&app_proxy_app::probe::process_probe(debug_detach)?),
        Commands::Probe {
            command: Probes::Package { app },
        } => print(&app_proxy_app::probe::package_probe(app.name())?),
    }
}
