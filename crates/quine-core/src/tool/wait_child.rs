use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio::time::Duration;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::CoreInput;
use crate::session::SessionId;

/// Tool for waiting on a child session to complete.
pub(crate) struct WaitChildTool;

#[async_trait]
impl Tool for WaitChildTool {
    fn name(&self) -> &str {
        "wait_child"
    }

    fn description(&self) -> &str {
        "Wait for a child agent session to complete and collect its exit status. \
         Use non_blocking=true to check without waiting."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "child_id": {
                    "type": "string",
                    "description": "The session ID of the child to wait for."
                },
                "non_blocking": {
                    "type": "boolean",
                    "description": "If true, return immediately with null if child is still running. Default false."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional timeout in milliseconds for blocking waits. Returns an error if the deadline expires."
                }
            },
            "required": ["child_id"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let child_id_str = arguments
            .get("child_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: child_id".into(),
            })?;

        let non_blocking = arguments
            .get("non_blocking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout = arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis);

        let core_input = context
            .core_input
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no core_input channel available".into(),
            })?;

        // Parse the child_id from the debug format "SessionId(uuid)"
        let child_id =
            parse_session_id(child_id_str).ok_or_else(|| ToolError::InvalidArguments {
                message: format!("invalid child_id: {child_id_str}"),
            })?;

        let (reply_tx, reply_rx) = oneshot::channel();

        core_input
            .send(CoreInput::WaitSession {
                parent_id: context.session_id,
                child_id,
                reply: reply_tx,
                non_blocking,
                timeout,
            })
            .await
            .map_err(|_| ToolError::Internal {
                message: "core_input channel closed".into(),
            })?;

        match reply_rx.await {
            Ok(Some(status)) => {
                let json = serde_json::to_string(&status).unwrap_or_else(|_| "unknown".into());
                Ok(ToolOutput::success(json))
            }
            Ok(None) => Ok(ToolOutput::success("null")),
            Err(_) => Ok(ToolOutput::error("wait reply channel dropped")),
        }
    }
}

/// Parse a SessionId from its Debug format "SessionId(uuid)".
pub(crate) fn parse_session_id(s: &str) -> Option<SessionId> {
    let s = s.trim();
    // Try parsing as raw UUID first
    if let Ok(uuid) = s.parse::<uuid::Uuid>() {
        return Some(SessionId::from_uuid(uuid));
    }
    // Try SessionId(uuid) format
    let inner = s.strip_prefix("SessionId(")?.strip_suffix(')')?;
    let uuid = inner.parse::<uuid::Uuid>().ok()?;
    Some(SessionId::from_uuid(uuid))
}
