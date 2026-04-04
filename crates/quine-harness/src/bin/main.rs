use std::sync::Arc;

use clap::{Parser, Subcommand};
use quine_harness::{
    create_provider_from_env, default_memory_dir_from_state_dir, HarnessConfig, LocalHarness,
};

#[derive(Parser)]
#[command(name = "quine-harness", about = "Quine harness daemon")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the harness daemon.
    Start {
        /// Socket path override.
        #[arg(long)]
        socket: Option<String>,
        /// State directory override.
        #[arg(long)]
        state_dir: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { socket, state_dir } => {
            let state_dir = state_dir
                .map(std::path::PathBuf::from)
                .unwrap_or_else(quine_harness::default_state_dir);
            let config = HarnessConfig {
                socket_path: socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(quine_harness::default_socket_path),
                memory_dir: default_memory_dir_from_state_dir(&state_dir),
                state_dir,
            };

            let provider = create_provider_from_env();
            let harness = Arc::new(
                LocalHarness::with_archive_root(provider, Some(config.state_dir.clone())).await?,
            );

            quine_harness::server::run_ipc_server(&config.socket_path, harness).await?;
        }
    }

    Ok(())
}
