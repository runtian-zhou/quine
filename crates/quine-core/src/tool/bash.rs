use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Default timeout for bash command execution (120 seconds).
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Tool for executing shell commands.
///
/// Spawns `/bin/sh -c <command>`, captures stdout and stderr, and enforces
/// a configurable timeout. The working directory is set to the real session
/// working directory.
pub(crate) struct BashTool;

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "Execute a bash command. The command runs in /bin/sh with the working directory set to \
         the session working directory. Stdout and stderr are captured and returned. Commands \
         time out after 120 seconds by default."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds. Defaults to 120."
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let command = arguments
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: command".into(),
            })?;

        let timeout_secs = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let timeout = Duration::from_secs(timeout_secs);

        let result = tokio::time::timeout(timeout, async {
            Command::new("/bin/sh")
                .arg("-c")
                .arg(command)
                .current_dir(&context.working_directory)
                .output()
                .await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let exit_code = output.status.code().unwrap_or(-1);

                let mut text = String::new();
                if !stdout.is_empty() {
                    text.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str("STDERR:\n");
                    text.push_str(&stderr);
                }

                if exit_code != 0 {
                    text.push_str(&format!("\n(exit code: {exit_code})"));
                    Ok(ToolOutput::error(text))
                } else {
                    Ok(ToolOutput::success(text))
                }
            }
            Ok(Err(e)) => Err(ToolError::Internal {
                message: format!("failed to spawn process: {e}"),
            }),
            Err(_) => Err(ToolError::Timeout {
                seconds: timeout_secs,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::session::SessionId;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn make_context() -> (TempDir, ExecutionContext) {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
        };
        (base, ctx)
    }

    #[tokio::test]
    async fn bash_echo() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}), &ctx)
            .await
            .unwrap();

        assert_eq!(result.content.trim(), "hello");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn bash_nonzero_exit() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(serde_json::json!({"command": "exit 42"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("exit code: 42"));
    }

    #[tokio::test]
    async fn bash_captures_stderr() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(serde_json::json!({"command": "echo err >&2"}), &ctx)
            .await
            .unwrap();

        assert!(result.content.contains("STDERR:"));
        assert!(result.content.contains("err"));
    }

    #[tokio::test]
    async fn bash_missing_command() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }

    #[tokio::test]
    async fn bash_timeout() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(
                serde_json::json!({"command": "sleep 10", "timeout": 1}),
                &ctx,
            )
            .await;

        assert!(matches!(result, Err(ToolError::Timeout { .. })));
    }
}
