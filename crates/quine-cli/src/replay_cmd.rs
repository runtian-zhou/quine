use std::path::Path;

use quine_core::log::ConversationLog;
use quine_core::replay::{ReplayEngine, ReplayOptions};
use quine_core::tool::ToolRegistry;

pub async fn run_replay(log_file: &Path, strict: bool, dry_run: bool) -> anyhow::Result<()> {
    println!("\x1b[1;36mQuine Replay\x1b[0m");
    println!("\x1b[90mLog: {}\x1b[0m", log_file.display());

    let log = ConversationLog::load(log_file)?;
    let working_dir = std::env::current_dir()?;
    let registry = ToolRegistry::register_defaults(&working_dir);

    let options = ReplayOptions { strict, dry_run };

    println!(
        "\x1b[90mModel: {} | Provider: {} | Entries: {}\x1b[0m",
        log.model,
        log.provider,
        log.entries.len()
    );
    if dry_run {
        println!("\x1b[33m[DRY RUN MODE]\x1b[0m");
    }
    if strict {
        println!("\x1b[31m[STRICT MODE]\x1b[0m");
    }
    println!();

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().await?;

    println!("\n\x1b[1;36m--- Replay Summary ---\x1b[0m");
    println!("  Entries processed: {}", result.entries_processed);
    println!("  Tools executed:    {}", result.tools_executed);
    println!("  Drift warnings:    {}", result.drift_warnings.len());

    if !result.drift_warnings.is_empty() {
        println!("\n\x1b[1;33mDrift Details:\x1b[0m");
        for warning in &result.drift_warnings {
            println!(
                "  Entry {}: {} - output differs",
                warning.entry_index, warning.tool_name
            );
        }
    }

    Ok(())
}
