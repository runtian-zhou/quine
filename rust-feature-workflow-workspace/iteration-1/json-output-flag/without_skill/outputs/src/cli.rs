use clap::{Parser, Subcommand};

/// quine-cli: a tool for inspecting project metadata
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub struct Cli {
    /// Output results as JSON (suitable for piping to jq)
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// List items in a given path
    List {
        /// Path to list
        #[arg(default_value = ".")]
        path: String,
    },
    /// Show detailed info about a named item
    Info {
        /// Name of the item to inspect
        name: String,
    },
    /// Search for items matching a query
    Search {
        /// Search query string
        query: String,

        /// Maximum number of results to return
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
}
