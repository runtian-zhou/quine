use std::sync::Arc;

use clap::{Parser, Subcommand};
use quine_harness::{
    create_default_permission_checker, create_provider_from_env, HarnessConfig, LocalHarness,
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
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Start { socket } => {
            let config = HarnessConfig {
                socket_path: socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(quine_harness::default_socket_path),
            };

            let provider = create_provider_from_env();
            let checker = create_default_permission_checker();
            let harness = Arc::new(LocalHarness::new(provider, Some(checker)));

            quine_harness::server::run_ipc_server(&config.socket_path, harness).await?;
        }
    }

    Ok(())
}
