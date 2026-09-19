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
        Commands::Serve { home } => match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime
                .block_on(app_proxy_app::coordinator::serve(home))
                .map_err(Into::into),
            Err(error) => Err(error.into()),
        },
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
