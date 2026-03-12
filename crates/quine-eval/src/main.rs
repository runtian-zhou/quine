mod harness;
mod judge;
mod report;
mod types;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "quine-eval", about = "A/B tester: compare Claude Code vs Quine CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run A/B tests from a test suite file
    Run {
        /// Path to the test suite JSON file
        suite: PathBuf,

        /// Model to use for Quine
        #[arg(long, default_value = "claude-sonnet-4-20250514")]
        model: String,

        /// Model to use for Claude Code (default: same as --model)
        #[arg(long)]
        claude_model: Option<String>,

        /// Output directory for results
        #[arg(short, long, default_value = ".quine/eval")]
        output: PathBuf,

        /// Model used for LLM-as-judge evaluation
        #[arg(long, default_value = "claude-sonnet-4-20250514")]
        judge_model: String,

        /// Skip the LLM judge evaluation step
        #[arg(long)]
        skip_judge: bool,
    },
    /// Create an example test suite file
    Init {
        /// Output path
        #[arg(default_value = "eval_suite.json")]
        path: PathBuf,
    },
    /// View results from a previous run
    Show {
        /// Path to the results JSON file
        results: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            suite,
            model,
            claude_model,
            output,
            judge_model,
            skip_judge,
        } => {
            let claude_model = claude_model.unwrap_or_else(|| model.clone());
            harness::run_suite(&suite, &model, &claude_model, &output, &judge_model, skip_judge)
                .await?;
        }
        Commands::Init { path } => {
            let example = types::TestSuite::example();
            let json = serde_json::to_string_pretty(&example)?;
            std::fs::write(&path, json)?;
            println!("Example test suite written to: {}", path.display());
        }
        Commands::Show { results } => {
            report::show_results(&results)?;
        }
    }

    Ok(())
}
