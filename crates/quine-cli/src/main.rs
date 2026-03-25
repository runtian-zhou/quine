mod agent_ctl;
mod chat;
mod client;
mod log;
mod render;
mod run;
mod test_runner;
mod tui;

use std::io::IsTerminal;

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
        /// Skills to load for this session (can be repeated).
        #[arg(long, short = 's')]
        skill: Vec<String>,
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
        /// Skills to load for this session (can be repeated).
        #[arg(long, short = 's')]
        skill: Vec<String>,
    },
    /// Respond to an interaction request (e.g., ask_user prompt) on an existing session.
    Respond {
        /// The session ID to respond to.
        #[arg(long)]
        session: String,
        /// The response text.
        response: String,
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
    /// List active agent sessions.
    Ps {
        /// Show all sessions including completed ones.
        #[arg(long, short = 'a')]
        all: bool,
        /// Display sessions as a tree hierarchy.
        #[arg(long, short = 't')]
        tree: bool,
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Spawn a new child agent session.
    Spawn {
        /// The task description for the new session.
        task: String,
        /// Parent session ID to spawn under.
        #[arg(long)]
        parent: Option<String>,
        /// System prompt for the new session.
        #[arg(long)]
        system_prompt: Option<String>,
        /// Inherit conversation history from parent.
        #[arg(long)]
        inherit_history: bool,
        /// Output as JSON instead of plain text.
        #[arg(long)]
        json: bool,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Send a signal to an agent session.
    Signal {
        /// The session ID to signal.
        session_id: String,
        /// The signal to send (term, kill, stop, continue).
        signal: String,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Send an IPC message to a target session.
    Send {
        /// The target session ID.
        target: String,
        /// The message content. If omitted, reads from stdin.
        message: Option<String>,
        /// Optional key for keyed messaging.
        #[arg(long)]
        key: Option<String>,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Receive an IPC message from a source session.
    Recv {
        /// The source session ID to receive from.
        source: String,
        /// Return immediately if no message is available.
        #[arg(long)]
        non_blocking: bool,
        /// Optional key for keyed messaging.
        #[arg(long)]
        key: Option<String>,
        /// Output as JSON instead of plain text.
        #[arg(long)]
        json: bool,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Manage and inspect skills.
    Skills {
        #[command(subcommand)]
        command: SkillsCommands,
    },
    /// Run a scripted test scenario against the daemon.
    Test {
        /// Path to a test scenario TOML file, or a directory of scenarios.
        scenario: String,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
        /// Output results as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Print version information.
    Version,
}

#[derive(Subcommand)]
enum SkillsCommands {
    /// List all available skills.
    List {
        /// Output as JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
    /// Show details of a specific skill.
    Show {
        /// Skill name.
        name: String,
        /// Socket path to connect to the harness daemon.
        #[arg(long)]
        socket: Option<String>,
    },
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
        Commands::Chat { socket, skill } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                tui::run_tui_chat(&socket_path, &skill).await?;
            } else {
                chat::run_chat(&socket_path, &skill).await?;
            }
        }
        Commands::Run {
            message,
            session,
            json,
            socket,
            skill,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            run::run_oneshot(&socket_path, &message, session.as_deref(), json, &skill).await?;
        }
        Commands::Respond {
            session,
            response,
            json,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            run::run_respond(&socket_path, &session, &response, json).await?;
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
                let checker = quine_harness::create_default_permission_checker();
                let harness =
                    std::sync::Arc::new(quine_harness::LocalHarness::new(provider, Some(checker)));
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
        Commands::Ps {
            all,
            tree,
            json,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            agent_ctl::handle_ps(&socket_path, all, tree, json).await?;
        }
        Commands::Spawn {
            task,
            parent,
            system_prompt,
            inherit_history: _,
            json,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            agent_ctl::handle_spawn(
                &socket_path,
                &task,
                parent.as_deref(),
                system_prompt.as_deref(),
                json,
            )
            .await?;
        }
        Commands::Signal {
            session_id,
            signal,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            agent_ctl::handle_signal(&socket_path, &session_id, &signal).await?;
        }
        Commands::Send {
            target,
            message,
            key: _,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            agent_ctl::handle_send(&socket_path, &target, message.as_deref()).await?;
        }
        Commands::Recv {
            source,
            non_blocking,
            key: _,
            json: _,
            socket,
        } => {
            let socket_path = socket
                .map(std::path::PathBuf::from)
                .unwrap_or_else(default_socket_path);
            agent_ctl::handle_recv(&socket_path, &source, non_blocking).await?;
        }
        Commands::Skills { command } => match command {
            SkillsCommands::List { json, socket } => {
                let socket_path = socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(default_socket_path);
                run::run_skills_list(&socket_path, json).await?;
            }
            SkillsCommands::Show { name, socket } => {
                let socket_path = socket
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(default_socket_path);
                run::run_skills_show(&socket_path, &name).await?;
            }
        },
        Commands::Test {
            scenario,
            socket,
            json,
        } => {
            test_runner::run_test(&scenario, socket, json).await?;
        }
        Commands::Version => {
            println!("quine {}", env!("CARGO_PKG_VERSION"));
        }
    }

    Ok(())
}
