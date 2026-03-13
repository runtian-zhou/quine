mod commands;
mod interactive;
mod permissions;
mod render;
mod replay_cmd;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

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
            provider,
            model,
            base_url,
            r#continue,
            no_stream,
            print,
            working_dir,
            output_format,
        } => {
            if let Some(prompt) = print {
                interactive::run_print(
                    &provider,
                    &model,
                    base_url.as_deref(),
                    &prompt,
                    working_dir,
                    &output_format,
                )
                .await?;
            } else {
                interactive::run_chat(
                    &provider,
                    &model,
                    base_url.as_deref(),
                    r#continue,
                    !no_stream,
                )
                .await?;
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
