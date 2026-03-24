mod chat;
mod client;
mod render;

use clap::{Parser, Subcommand};
use quine_harness::default_socket_path;

#[derive(Parser)]
#[command(name = "quine", about = "Quine AI coding agent CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start an interactive chat session with the agent.
    Chat {
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Manage the harness daemon.
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    /// Print version information.
    Version,
}

#[derive(Subcommand)]
enum DaemonCommands {
    /// Start the harness daemon.
    Start {
        /// Socket path override.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Stop the harness daemon.
    Stop {
        /// Socket path override.
        #[arg(long)]
        socket: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Chat { socket } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            chat::run_chat(&socket_path).await?;
        }
        Commands::Daemon { command } => match command {
            DaemonCommands::Start { socket } => {
                let socket_path = socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(default_socket_path);

                // Start the daemon in-process.
                let provider = quine_harness::create_provider_from_env();
                let harness = std::sync::Arc::new(quine_harness::LocalHarness::new(provider));
                quine_harness::server::run_ipc_server(&socket_path, harness).await?;
            }
            DaemonCommands::Stop { socket } => {
                let socket_path = socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(default_socket_path);
                let mut client = client::IpcClient::connect(&socket_path).await?;
                let result = client
                    .call(quine_harness::protocol::methods::SHUTDOWN, None)
                    .await?;
                match result {
                    Ok(_) => eprintln!("Daemon stopped."),
                    Err(e) => eprintln!("Failed to stop daemon: {e}"),
                }
            }
        },
        Commands::Version => {
            println!("quine {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}
