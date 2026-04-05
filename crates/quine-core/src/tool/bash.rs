use std::time::{Duration, Instant};

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
         the session working directory. Use this for general shell commands such as inspection, \
         builds, tests, git operations, or other command-line workflows allowed by the current \
         sandbox and permission policy. Stdout and stderr are captured and returned. Commands \
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
        let started_at = Instant::now();

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

                append_execution_time(&mut text, started_at.elapsed());

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

fn append_execution_time(text: &mut String, elapsed: Duration) {
    let millis = elapsed.as_millis();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&format!("Execution time: {millis}ms"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::permission::{analyze_command, CommandRisk};
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

        assert!(result.content.contains("hello"));
        assert!(result.content.contains("Execution time:"));
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
        assert!(result.content.contains("Execution time:"));
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
    async fn bash_uses_default_timeout_when_omitted() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(serde_json::json!({"command": "echo timeout-default"}), &ctx)
            .await
            .unwrap();

        assert!(result.content.contains("timeout-default"));
        assert!(result.content.contains("Execution time:"));
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
    async fn bash_allows_file_edit_commands() {
        let (_base, ctx) = make_context().await;
        let tool = BashTool;

        let result = tool
            .execute(
                serde_json::json!({"command": "echo hello > test.txt"}),
                &ctx,
            )
            .await;

        let output = result.unwrap();
        assert!(!output.is_error);
        let file_contents = tokio::fs::read_to_string(ctx.working_directory.join("test.txt"))
            .await
            .unwrap();
        assert_eq!(file_contents.trim(), "hello");
    }

    #[test]
    fn bash_classifies_read_oriented_commands() {
        let pwd = analyze_command("pwd");
        let ls = analyze_command("ls -la");
        let find = analyze_command("find . -maxdepth 1 -type f");

        assert_eq!(pwd.risk, CommandRisk::ReadOnly);
        assert_eq!(ls.risk, CommandRisk::ReadOnly);
        assert_eq!(find.risk, CommandRisk::ReadOnly);
    }

    #[test]
    fn bash_classifies_write_capable_commands() {
        let redirected_echo = analyze_command("echo hello > test.txt");
        let touch = analyze_command("touch test.txt");
        let mkdir = analyze_command("mkdir scratch");
        let remove = analyze_command("rm -f test.txt");

        assert_eq!(redirected_echo.risk, CommandRisk::Mutating);
        assert_eq!(touch.risk, CommandRisk::Mutating);
        assert_eq!(mkdir.risk, CommandRisk::Mutating);
        assert_eq!(remove.risk, CommandRisk::Mutating);
    }

    #[test]
    fn bash_classifies_nested_shell_wrappers_conservatively() {
        let sh_c = analyze_command("sh -c 'pwd'");
        let bash_lc = analyze_command("bash -lc 'ls -la'");
        let env_shell = analyze_command("env sh -c 'echo hello'");

        assert_eq!(sh_c.risk, CommandRisk::NestedShell);
        assert_eq!(bash_lc.risk, CommandRisk::NestedShell);
        assert_eq!(env_shell.risk, CommandRisk::NestedShell);
    }

    #[test]
    fn bash_classifies_inline_interpreters_conservatively() {
        let python_read = analyze_command("python -c 'print(1)'");
        let python_write =
            analyze_command("python -c 'from pathlib import Path; Path(\"x\").write_text(\"y\")'");
        let perl = analyze_command("perl -e 'print qq(hi)'");

        assert_eq!(python_read.risk, CommandRisk::Interpreter);
        assert_eq!(python_write.risk, CommandRisk::Interpreter);
        assert_eq!(perl.risk, CommandRisk::Interpreter);
    }
}
