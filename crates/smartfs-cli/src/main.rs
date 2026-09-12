//! `smartfs-cli` main executable entrypoint.

use clap::Parser;
use smartfs_cli::{run_cli, Cli};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run_cli(cli).await {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}
