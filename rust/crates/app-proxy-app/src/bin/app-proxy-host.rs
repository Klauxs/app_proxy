#![forbid(unsafe_code)]
#![windows_subsystem = "windows"]

use clap::{Parser, Subcommand};
use std::ffi::OsString;
use std::path::PathBuf;

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}
#[derive(Subcommand)]
enum Commands {
    Launch {
        instance: uuid::Uuid,
        #[arg(long)]
        home: PathBuf,
        /// 需要用户选择时显示前台流程，失败时保留通知
        #[arg(long)]
        notify: bool,
    },
    GuardInstall {
        #[arg(long)]
        ticket: String,
    },
    EventListen {
        #[arg(long)]
        store: uuid::Uuid,
        #[arg(long)]
        generation: uuid::Uuid,
    },
    PackageChild {
        #[arg(long)]
        request: PathBuf,
    },
    Serve {
        #[arg(long)]
        home: PathBuf,
        #[arg(long)]
        expected_store: Option<uuid::Uuid>,
    },
    ProbeChild {
        #[arg(long)]
        request: PathBuf,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

fn main() {
    let result = match Cli::parse().command {
        Commands::Launch {
            instance,
            home,
            notify,
        } => {
            if app_proxy_windows::setup::ensure_available().is_err() {
                if notify {
                    let _ = app_proxy_windows::console::notify_launch_failure(
                        "AppProxy 正在升级，请安装完成后再打开。",
                    );
                }
                std::process::exit(5);
            }
            let result = match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime
                    .block_on(app_proxy_app::launch_cli::from_shortcut(
                        home, instance, notify,
                    ))
                    .map_err(|e| (e.exit_code, e.to_string())),
                Err(error) => Err((1, error.to_string())),
            };
            if let Err((code, message)) = result {
                if notify {
                    let _ = app_proxy_windows::console::notify_launch_failure(&message);
                } else {
                    eprintln!("{message}");
                }
                std::process::exit(code);
            }
            Ok(())
        }
        Commands::GuardInstall { ticket } => {
            app_proxy_windows::guard_install::elevated(&ticket).map_err(Into::into)
        }
        Commands::EventListen { store, generation } => {
            match tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime
                    .block_on(app_proxy_windows::guard_listener::run(store, generation))
                    .map_err(Into::into),
                Err(error) => Err(error.into()),
            }
        }
        Commands::PackageChild { request } => {
            app_proxy_windows::package_launch::run_helper(&request).map_err(Into::into)
        }
        Commands::ProbeChild { request, args } => app_proxy_app::probe::child(&request, args),
        Commands::Serve {
            home,
            expected_store,
        } => match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime
                .block_on(app_proxy_app::coordinator::serve_expected(
                    home,
                    expected_store,
                ))
                .map_err(Into::into),
            Err(error) => Err(error.into()),
        },
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
