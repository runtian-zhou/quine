use async_trait::async_trait;
use tokio::sync::oneshot;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::CoreInput;
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

        core_input
            .send(CoreInput::SpawnSession {
                parent_id: context.session_id,
                child_id,
                task: task.to_string(),
                system_prompt,
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

        match reply_rx.await {
            Ok(Ok(())) => Ok(ToolOutput::success(format!("{child_id:?}"))),
            Ok(Err(e)) => Ok(ToolOutput::error(format!("failed to spawn: {e}"))),
            Err(_) => Ok(ToolOutput::error("spawn reply channel dropped")),
        }
    }
}
