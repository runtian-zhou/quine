mod harness;
mod report;
mod types;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "quine-eval", about = "Eval harness for Quine CLI")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Run tests from a test suite file
    Run {
        /// Path to the test suite JSON file
        suite: PathBuf,

        /// Model to use
        #[arg(long, default_value = "claude-sonnet-4-20250514")]
        model: String,

        /// Output directory for results
        #[arg(short, long, default_value = ".quine/eval")]
        output: PathBuf,
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
    dotenvy::dotenv().ok();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            suite,
            model,
            output,
        } => {
            harness::run_suite(&suite, &model, &output).await?;
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
