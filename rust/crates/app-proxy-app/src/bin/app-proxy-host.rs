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
    ProbeChild {
        #[arg(long)]
        request: PathBuf,
        #[arg(last = true)]
        args: Vec<OsString>,
    },
}

fn main() {
    let Commands::ProbeChild { request, args } = Cli::parse().command;
    if app_proxy_app::probe::child(&request, args).is_err() {
        std::process::exit(1);
    }
}
