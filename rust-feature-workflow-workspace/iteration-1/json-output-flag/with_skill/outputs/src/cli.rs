//! CLI argument definitions using `clap`.
//!
//! The `--json` flag is defined as a global argument on the top-level [`Cli`]
//! struct, making it available to every subcommand automatically.

use clap::{Parser, Subcommand};

use crate::output::OutputFormat;

/// A sample CLI tool with JSON output support.
#[derive(Parser, Debug)]
#[command(name = "quine", version, about = "A sample CLI tool")]
pub struct Cli {
    /// Output results as JSON instead of human-readable text.
    ///
    /// When this flag is set, all output (including errors) is emitted as
    /// structured JSON, suitable for piping to `jq` or other tools.
    #[arg(long, short = 'j', global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Returns the [`OutputFormat`] indicated by the CLI flags.
    pub fn output_format(&self) -> OutputFormat {
        if self.json {
            OutputFormat::Json
        } else {
            OutputFormat::Human
        }
    }
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Show the current status.
    Status,

    /// List available items.
    List {
        /// Optional filter pattern.
        #[arg(short, long)]
        filter: Option<String>,
    },

    /// Show details for a specific item.
    Show {
        /// The item name or ID.
        name: String,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_json_flag_default_is_false() {
        let cli = Cli::parse_from(["quine", "status"]);
        assert_eq!(cli.json, false, "--json must default to false");
        assert_eq!(
            cli.output_format(),
            OutputFormat::Human,
            "Default output format must be Human"
        );
    }

    #[test]
    fn test_json_long_flag() {
        let cli = Cli::parse_from(["quine", "--json", "status"]);
        assert_eq!(cli.json, true, "--json long flag must set json to true");
        assert_eq!(
            cli.output_format(),
            OutputFormat::Json,
            "Output format must be Json when --json is passed"
        );
    }

    #[test]
    fn test_json_short_flag() {
        let cli = Cli::parse_from(["quine", "-j", "status"]);
        assert_eq!(cli.json, true, "-j short flag must set json to true");
        assert_eq!(
            cli.output_format(),
            OutputFormat::Json,
            "Output format must be Json when -j is passed"
        );
    }

    #[test]
    fn test_json_flag_after_subcommand() {
        // Because --json is global, it should work after the subcommand too.
        let cli = Cli::parse_from(["quine", "status", "--json"]);
        assert_eq!(
            cli.json, true,
            "--json must work when placed after the subcommand (global flag)"
        );
    }

    #[test]
    fn test_list_subcommand_with_filter() {
        let cli = Cli::parse_from(["quine", "list", "--filter", "foo"]);
        match &cli.command {
            Command::List { filter } => {
                assert_eq!(
                    filter.as_deref(),
                    Some("foo"),
                    "Filter must be exactly 'foo'"
                );
            }
            _ => panic!("Expected List command"),
        }
    }

    #[test]
    fn test_show_subcommand() {
        let cli = Cli::parse_from(["quine", "show", "my-item"]);
        match &cli.command {
            Command::Show { name } => {
                assert_eq!(name, "my-item", "Name must be exactly 'my-item'");
            }
            _ => panic!("Expected Show command"),
        }
    }
}
