use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct BashTool {
    working_dir: PathBuf,
}

impl BashTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "Bash"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the working directory. Use for system commands, running tests, installing packages, and other terminal operations that require shell execution."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default: 120). Note: timeout is not yet enforced."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let command = arguments["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("command is required"))?;

        let start = std::time::Instant::now();
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.working_dir)
            .env("TERM", "dumb")
            .output();
        let elapsed = start.elapsed();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("STDERR:\n");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = format!("(no output, exit code {})", exit_code);
                }

                // Truncate very long output
                let max_len = 100_000;
                if result.len() > max_len {
                    let mut end = max_len;
                    while end > 0 && !result.is_char_boundary(end) {
                        end -= 1;
                    }
                    let truncated = &result[..end];
                    result = format!(
                        "{}\n\n... output truncated ({} bytes total)",
                        truncated,
                        result.len()
                    );
                }

                let time_suffix = format_elapsed(elapsed);
                Ok(ToolOutput {
                    success: output.status.success(),
                    output: if output.status.success() {
                        format!("{}\n{}", result, time_suffix)
                    } else {
                        format!("Exit code {}\n{}\n{}", exit_code, result, time_suffix)
                    },
                })
            }
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!(
                    "Failed to execute command: {}\n{}",
                    e,
                    format_elapsed(elapsed)
                ),
            }),
        }
    }
}

fn format_elapsed(elapsed: std::time::Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 1.0 {
        format!("(completed in {:.0}ms)", secs * 1000.0)
    } else {
        format!("(completed in {:.1}s)", secs)
    }
}
