use std::time::Duration;

use async_trait::async_trait;
use tokio::process::Command;
use tokio::select;

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
         the session working directory. Use this for inspection, build, and test commands, not \
         file modifications. Stdout and stderr are captured and returned. Commands time out after \
         120 seconds by default."
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

        let mut command_builder = Command::new("/bin/sh");
        command_builder
            .arg("-c")
            .arg(command)
            .current_dir(&context.working_directory)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);

        let child = command_builder.spawn().map_err(|e| ToolError::Internal {
            message: format!("failed to spawn process: {e}"),
        })?;
        let child_pid = child.id().ok_or_else(|| ToolError::Internal {
            message: "failed to determine child process id".into(),
        })? as i32;

        let wait_result = tokio::time::timeout(timeout, async {
            select! {
                output = child.wait_with_output() => output.map_err(|e| ToolError::Internal {
                    message: format!("failed to run process: {e}"),
                }),
                _ = context.cancellation.cancelled() => {
                    unsafe {
                        libc::kill(-child_pid, libc::SIGKILL);
                    }
                    Err(ToolError::Cancelled)
                },
            }
        })
        .await;

        match wait_result {
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
            Ok(Err(tool_error)) => Err(tool_error),
            Err(_) => {
                unsafe {
                    libc::kill(-child_pid, libc::SIGKILL);
                }
                Err(ToolError::Timeout {
                    seconds: timeout_secs,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::session::SessionId;
    use std::path::Path;
    use std::sync::Arc;
    use tempfile::TempDir;

    #[tokio::test]
    async fn bash_cancellation_kills_background_process_group() {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let marker = base.path().join("marker.txt");
        let command = format!(
            "sh -c 'sleep 5; python -c \"from pathlib import Path; Path(r#\"{}\"#).write_text(\"done\")\"' & wait",
            marker.display()
        );
        let (cancel_tx, cancellation) = crate::tool::CancellationChannel::new_pair();
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
            cancellation,
        };
        let tool = BashTool;

        let handle = tokio::spawn(async move {
            tool.execute(serde_json::json!({"command": command, "timeout": 30}), &ctx)
                .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        let _ = cancel_tx.send(true);
        let result = handle.await.unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));

        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !Path::new(&marker).exists(),
            "background process survived cancellation and wrote marker"
        );
    }

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
            cancellation: crate::tool::CancellationChannel::never(),
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

        assert!(
            matches!(result, Err(ToolError::Timeout { .. })),
            "expected timeout, got: {result:?}"
        );
    }

    #[tokio::test]
    async fn bash_cancels_immediately() {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let (cancel_tx, cancellation) = crate::tool::CancellationChannel::new_pair();
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
            cancellation,
        };
        let tool = BashTool;

        let handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({"command": "sleep 10", "timeout": 30}),
                &ctx,
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel_tx.send(true).unwrap();

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
    }

    #[tokio::test]
    async fn bash_allows_redirection_to_reach_execution() {
        let (base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(
                serde_json::json!({"command": "echo hello > test.txt"}),
                &ctx,
            )
            .await;

        assert!(result.is_ok());
        assert!(base.path().join("test.txt").exists());
    }
}
