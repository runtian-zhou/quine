mod chat;
mod client;
mod log;
mod render;
mod run;

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
    /// Send a one-shot message to the agent and exit.
    Run {
        /// The message to send to the agent.
        message: String,
        /// Resume an existing session by ID.
        #[arg(long)]
        session: Option<String>,
        /// Output structured JSON instead of plain text.
        #[arg(long)]
        json: bool,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// View session logs.
    Log {
        /// The session ID to dump logs for. If omitted, use --list.
        session_id: Option<String>,
        /// List all sessions with timestamps.
        #[arg(long)]
        list: bool,
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
        Commands::Run {
            message,
            session,
            json,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            run::run_oneshot(&socket_path, &message, session.as_deref(), json).await?;
        }
        Commands::Log {
            session_id,
            list,
            socket,
        } => {
            let socket_path = socket.map(std::path::PathBuf::from);
            let default_path = default_socket_path();
            if list || session_id.is_none() {
                log::list_session_logs(socket_path.as_deref().or(Some(&default_path))).await?;
            } else if let Some(sid) = session_id {
                log::dump_session_log(&sid, socket_path.as_deref().or(Some(&default_path))).await?;
            }
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
