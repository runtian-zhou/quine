//! Entry point for the CLI.
//!
//! Parses arguments, determines the output format, and dispatches to the
//! appropriate command handler.

mod cli;
mod commands;
mod output;

use clap::Parser;
use std::process;

use cli::{Cli, Command};
use output::OutputWriter;

fn main() {
    let cli = Cli::parse();
    let writer = OutputWriter::new(cli.output_format());

    let result = match &cli.command {
        Command::Status => commands::run_status(&writer),
        Command::List { filter } => commands::run_list(&writer, filter.as_deref()),
        Command::Show { name } => commands::run_show(&writer, name),
    };

    if let Err(e) = result {
        let exit_code = writer
            .write_error("internal", &e.to_string())
            .unwrap_or(1);
        process::exit(exit_code);
    }
}
