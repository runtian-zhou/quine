use crate::conversation::Entry;
use crate::log::ConversationLog;
use crate::tool::ToolRegistry;

#[derive(Debug, Clone, Copy)]
pub struct ReplayOptions {
    pub strict: bool,
    pub dry_run: bool,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            strict: false,
            dry_run: false,
        }
    }
}

pub struct ReplayEngine {
    log: ConversationLog,
    registry: ToolRegistry,
    options: ReplayOptions,
}

#[derive(Debug)]
pub struct DriftWarning {
    pub entry_index: usize,
    pub tool_name: String,
    pub expected_output: String,
    pub actual_output: String,
}

#[derive(Debug)]
pub struct ReplayResult {
    pub entries_processed: usize,
    pub tools_executed: usize,
    pub drift_warnings: Vec<DriftWarning>,
}

impl ReplayEngine {
    pub fn new(log: ConversationLog, registry: ToolRegistry, options: ReplayOptions) -> Self {
        Self {
            log,
            registry,
            options,
        }
    }

    pub fn run(&self) -> anyhow::Result<ReplayResult> {
        let mut tools_executed = 0;
        let mut drift_warnings = Vec::new();

        for (i, entry) in self.log.entries.iter().enumerate() {
            match entry {
                Entry::UserMessage { content } => {
                    println!("\n\x1b[1;34m> User:\x1b[0m {}", content);
                }
                Entry::AssistantMessage {
                    content,
                    tool_calls,
                } => {
                    println!("\n\x1b[1;32m> Assistant:\x1b[0m {}", content);
                    if !tool_calls.is_empty() {
                        println!(
                            "  (with {} tool call{})",
                            tool_calls.len(),
                            if tool_calls.len() == 1 { "" } else { "s" }
                        );
                    }
                }
                Entry::ToolExecution {
                    tool_name,
                    arguments,
                    result: expected_result,
                    ..
                } => {
                    println!("\n\x1b[1;33m> Tool: {}\x1b[0m", tool_name);

                    if self.options.dry_run {
                        println!("  [dry-run] Would execute with: {}", arguments);
                        tools_executed += 1;
                        continue;
                    }

                    match self.registry.get(tool_name) {
                        Some(tool) => {
                            let actual_result = tool.execute(arguments.clone())?;
                            tools_executed += 1;

                            if actual_result.output != expected_result.output {
                                let warning = DriftWarning {
                                    entry_index: i,
                                    tool_name: tool_name.clone(),
                                    expected_output: expected_result.output.clone(),
                                    actual_output: actual_result.output.clone(),
                                };

                                println!(
                                    "  \x1b[1;31m[DRIFT]\x1b[0m Output differs from recorded log"
                                );
                                println!(
                                    "  Expected: {}",
                                    truncate(&expected_result.output, 100)
                                );
                                println!(
                                    "  Actual:   {}",
                                    truncate(&actual_result.output, 100)
                                );

                                if self.options.strict {
                                    anyhow::bail!(
                                        "Drift detected at entry {} (strict mode). Tool '{}' output differs.",
                                        i,
                                        tool_name
                                    );
                                }

                                drift_warnings.push(warning);
                            } else {
                                println!("  \x1b[32m[OK]\x1b[0m");
                            }
                        }
                        None => {
                            println!("  \x1b[1;31m[ERROR]\x1b[0m Unknown tool: {}", tool_name);
                            if self.options.strict {
                                anyhow::bail!("Unknown tool '{}' at entry {} (strict mode)", tool_name, i);
                            }
                        }
                    }
                }
            }
        }

        Ok(ReplayResult {
            entries_processed: self.log.entries.len(),
            tools_executed,
            drift_warnings,
        })
    }
}

fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}
