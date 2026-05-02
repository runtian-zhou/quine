use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::oneshot;

const SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(30);

fn debug_enabled() -> bool {
    std::env::var("QUINE_DEBUG").is_ok()
}

fn debug_log_spawn(session_id: SessionId, child_id: SessionId, message: impl AsRef<str>) {
    if debug_enabled() {
        eprintln!(
            "[core][session={session_id:?}][spawn child={child_id:?}] {}",
            message.as_ref()
        );
    }
}

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::CoreInput;
use crate::permission::PermissionPromptBehavior;
use crate::session::{InheritanceFlags, SessionId};

/// Tool for spawning a child agent session.
pub(crate) struct SpawnTool;

#[async_trait]
impl Tool for SpawnTool {
    fn name(&self) -> &str {
        "spawn"
    }

    fn description(&self) -> &str {
        "Spawn a child agent session with a task. Returns the child session ID. \
         Use wait_child to collect the result, or signal to control it."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "The task for the child agent to execute."
                },
                "system_prompt": {
                    "type": "string",
                    "description": "Optional system prompt override for the child."
                },
                "inherit_history": {
                    "type": "boolean",
                    "description": "Copy parent conversation history to child. Default false."
                },
                "inherit_filesystem": {
                    "type": "boolean",
                    "description": "Share parent's filesystem with child. Default true."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: task".into(),
            })?;

        let system_prompt = arguments
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        let inherit_history = arguments
            .get("inherit_history")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let inherit_filesystem = arguments
            .get("inherit_filesystem")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let core_input = context
            .core_input
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no core_input channel available".into(),
            })?;

        let child_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        debug_log_spawn(
            context.session_id,
            child_id,
            format!(
                "dispatching SpawnSession (task_len={}, inherit_history={}, inherit_filesystem={}, has_system_prompt={})",
                task.len(),
                inherit_history,
                inherit_filesystem,
                system_prompt.is_some()
            ),
        );

        core_input
            .send(CoreInput::SpawnSession {
                parent_id: context.session_id,
                child_id,
                task: task.to_string(),
                system_prompt,
                prompt_behavior: context
                    .permission_runtime
                    .as_ref()
                    .map(|snapshot| snapshot.prompt_behavior)
                    .unwrap_or(PermissionPromptBehavior::Interactive),
                permission_rules: context
                    .permission_runtime
                    .as_ref()
                    .map(|snapshot| snapshot.rules.clone())
                    .unwrap_or_default(),
                permission_runtime: context.permission_runtime.clone(),
                inheritance: InheritanceFlags {
                    history: inherit_history,
                    filesystem: inherit_filesystem,
                    ..Default::default()
                },
                reply: reply_tx,
            })
            .await
            .map_err(|_| ToolError::Internal {
                message: "core_input channel closed".into(),
            })?;

        debug_log_spawn(
            context.session_id,
            child_id,
            "SpawnSession dispatched; awaiting reply",
        );

        tokio::select! {
            _ = context.cancellation.cancelled() => {
                debug_log_spawn(
                    context.session_id,
                    child_id,
                    "spawn cancelled while awaiting acknowledgement",
                );
                Err(ToolError::Cancelled)
            }
            reply = tokio::time::timeout(SPAWN_ACK_TIMEOUT, reply_rx) => {
                match reply {
                    Ok(Ok(Ok(()))) => {
                        debug_log_spawn(
                            context.session_id,
                            child_id,
                            "spawn acknowledged successfully",
                        );
                        Ok(ToolOutput::success(format!("{child_id:?}")))
                    }
                    Ok(Ok(Err(e))) => {
                        debug_log_spawn(
                            context.session_id,
                            child_id,
                            format!("spawn acknowledged with error: {e}"),
                        );
                        Ok(ToolOutput::error(format!("failed to spawn: {e}")))
                    }
                    Ok(Err(_)) => {
                        debug_log_spawn(
                            context.session_id,
                            child_id,
                            "spawn reply channel dropped before acknowledgement",
                        );
                        Ok(ToolOutput::error("spawn reply channel dropped"))
                    }
                    Err(_) => {
                        debug_log_spawn(
                            context.session_id,
                            child_id,
                            format!(
                                "spawn acknowledgement timed out after {}s",
                                SPAWN_ACK_TIMEOUT.as_secs()
                            ),
                        );
                        Ok(ToolOutput::error(format!(
                            "spawn timed out waiting for core acknowledgement after {}s",
                            SPAWN_ACK_TIMEOUT.as_secs()
                        )))
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::permission::{
        PermissionMode, PermissionPromptBehavior, PermissionRule, PermissionRuleEffect,
        PermissionRuleSet, PermissionRuntimeSnapshot, PermissionTarget,
    };
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    fn inherited_permission_runtime() -> PermissionRuntimeSnapshot {
        let mut rules = PermissionRuleSet::default();
        rules.session.push(PermissionRule {
            effect: PermissionRuleEffect::Deny,
            scope: crate::permission::RuleScope::Workspace,
            request_scope: None,
            target: PermissionTarget::Tool {
                name: "apply_patch".into(),
            },
            source_path: None,
        });
        PermissionRuntimeSnapshot {
            mode: PermissionMode::AcceptEdits,
            pre_plan_mode: Some(PermissionMode::Default),
            rules,
            workspace_root: std::path::PathBuf::from("/workspace"),
            additional_allowed_roots: vec![std::path::PathBuf::from("/tmp/extra")],
            prompt_behavior: PermissionPromptBehavior::Headless,
            last_decision: None,
            pending_approval: None,
        }
    }

    async fn make_context_with_core_input() -> (
        TempDir,
        TempDir,
        mpsc::Receiver<CoreInput>,
        ExecutionContext,
    ) {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let (core_input_tx, core_input_rx) = mpsc::channel(1);
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: Some(core_input_tx),
            permission_runtime: Some(inherited_permission_runtime()),
            cancellation: crate::tool::CancellationChannel::never(),
        };
        (base, session_dir, core_input_rx, ctx)
    }

    #[tokio::test]
    async fn spawn_returns_child_session_id_when_core_input_available() {
        let tool = SpawnTool;
        let (_base, _session, mut core_input_rx, ctx) = make_context_with_core_input().await;
        let session_id = ctx.session_id;

        let exec = tokio::spawn(async move {
            tool.execute(serde_json::json!({"task": "delegate this"}), &ctx)
                .await
                .unwrap()
        });

        let (child_id, reply_tx) = match core_input_rx.recv().await.unwrap() {
            CoreInput::SpawnSession {
                parent_id,
                child_id,
                task,
                system_prompt,
                prompt_behavior,
                permission_runtime,
                inheritance,
                reply,
                ..
            } => {
                assert_eq!(parent_id, session_id);
                assert_eq!(task, "delegate this");
                assert!(system_prompt.is_none());
                assert_eq!(prompt_behavior, PermissionPromptBehavior::Headless);
                let permission_runtime = permission_runtime.expect("permission runtime should propagate");
                assert_eq!(permission_runtime.mode, PermissionMode::AcceptEdits);
                assert_eq!(permission_runtime.pre_plan_mode, Some(PermissionMode::Default));
                assert_eq!(permission_runtime.prompt_behavior, PermissionPromptBehavior::Headless);
                assert_eq!(permission_runtime.additional_allowed_roots, vec![std::path::PathBuf::from("/tmp/extra")]);
                assert_eq!(permission_runtime.rules.session.len(), 1);
                assert!(!inheritance.history);
                assert!(inheritance.filesystem);
                (child_id, reply)
            }
            other => panic!("expected SpawnSession, got {other:?}"),
        };
        reply_tx.send(Ok(())).unwrap();

        let output = exec.await.unwrap();
        assert!(!output.is_error);
        assert_eq!(output.content, format!("{child_id:?}"));
    }

    #[tokio::test]
    async fn spawn_errors_without_core_input_channel() {
        let tool = SpawnTool;
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
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            permission_runtime: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        let err = tool
            .execute(serde_json::json!({"task": "delegate this"}), &ctx)
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Internal { .. }));
        assert!(err.to_string().contains("no core_input channel available"));
    }

    #[tokio::test]
    async fn spawn_returns_timeout_error_when_core_never_acknowledges() {
        let tool = SpawnTool;
        let (_base, _session, _core_input_rx, ctx) = make_context_with_core_input().await;

        let started = std::time::Instant::now();
        let output = tool
            .execute(serde_json::json!({"task": "delegate this"}), &ctx)
            .await
            .unwrap();

        assert!(output.is_error);
        assert!(output
            .content
            .contains("spawn timed out waiting for core acknowledgement"));
        assert!(started.elapsed() >= SPAWN_ACK_TIMEOUT);
    }
}
