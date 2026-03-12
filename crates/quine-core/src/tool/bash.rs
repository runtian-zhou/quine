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

        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(&self.working_dir)
            .env("TERM", "dumb")
            .output();

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
                    let truncated = &result[..max_len];
                    result = format!(
                        "{}\n\n... output truncated ({} bytes total)",
                        truncated,
                        result.len()
                    );
                }

                Ok(ToolOutput {
                    success: output.status.success(),
                    output: if output.status.success() {
                        result
                    } else {
                        format!("Exit code {}\n{}", exit_code, result)
                    },
                })
            }
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Failed to execute command: {}", e),
            }),
        }
    }
}
