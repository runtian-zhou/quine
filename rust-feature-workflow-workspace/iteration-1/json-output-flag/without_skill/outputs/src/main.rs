use clap::Parser;

mod cli;
mod commands;
mod output;

use cli::{Cli, Commands};
use output::OutputFormat;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let format = if cli.json {
        OutputFormat::Json
    } else {
        OutputFormat::Human
    };

    match &cli.command {
        Commands::List { path } => commands::list::run(path, &format),
        Commands::Info { name } => commands::info::run(name, &format),
        Commands::Search { query, limit } => commands::search::run(query, *limit, &format),
    }
}
