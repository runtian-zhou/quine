mod agent;
mod commands;
mod dispatcher;
mod event;
mod interactive;
mod permissions;
mod render;
mod replay_cmd;

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use quine_core::config::Config;
use quine_core::log::ConversationLog;
use quine_core::prompt::build_system_prompt;

#[derive(Parser)]
#[command(name = "quine", about = "A self-bootstrapping CLI assistant")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start an interactive chat session
    Chat {
        /// LLM provider to use (anthropic or openai)
        #[arg(long, default_value = "anthropic")]
        provider: String,

        /// Model to use
        #[arg(long, default_value = "claude-sonnet-4-20250514")]
        model: String,

        /// Override the base URL for the LLM provider API
        #[arg(long)]
        base_url: Option<String>,

        /// Continue from a previous log file
        #[arg(long, value_name = "LOG_FILE")]
        r#continue: Option<PathBuf>,

        /// Disable streaming responses
        #[arg(long)]
        no_stream: bool,

        /// Non-interactive: run a single prompt and print the result
        #[arg(short, long)]
        print: Option<String>,

        /// Working directory for tool execution
        #[arg(long, value_name = "DIR")]
        working_dir: Option<PathBuf>,

        /// Output format: text (default) or json
        #[arg(long, default_value = "text")]
        output_format: String,
    },
    /// Replay a recorded conversation log
    Replay {
        /// Path to the conversation log file
        log_file: PathBuf,

        /// Abort on any drift between recorded and actual tool output
        #[arg(long)]
        strict: bool,

        /// Print what would be done without executing tools
        #[arg(long)]
        dry_run: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    match cli.command {
        Commands::Chat {
            provider: provider_name,
            model,
            base_url,
            r#continue,
            no_stream,
            print,
            working_dir,
            output_format,
        } => {
            let config = if let Some(ref wd) = working_dir {
                let mut c = Config::new(&provider_name, &model);
                c.working_dir = wd.clone();
                c.log_dir = wd.join(".quine").join("logs");
                c
            } else {
                Config::new(&provider_name, &model)
            };
            let llm_provider = interactive::create_provider(&provider_name, base_url.as_deref())?;
            let system_prompt = build_system_prompt(&config.working_dir);

            let (conv_log, messages) = if let Some(ref log_path) = r#continue {
                println!("Continuing from: {}", log_path.display());
                let log = ConversationLog::load(log_path)?;
                let msgs = interactive::entries_to_messages(&log.entries);
                (log, msgs)
            } else {
                let log = ConversationLog::new(
                    model.clone(),
                    provider_name.clone(),
                    system_prompt.clone(),
                );
                (log, vec![])
            };

            let mut disp = dispatcher::Dispatcher::new(
                llm_provider,
                config,
                model,
                system_prompt,
                conv_log,
                messages,
                !no_stream,
            );

            if let Some(prompt) = print {
                disp.run_oneshot(&prompt, &output_format).await?;
            } else {
                disp.run().await?;
            }
        }
        Commands::Replay {
            log_file,
            strict,
            dry_run,
        } => {
            replay_cmd::run_replay(&log_file, strict, dry_run).await?;
        }
    }

    Ok(())
}
