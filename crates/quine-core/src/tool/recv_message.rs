use async_trait::async_trait;
use tokio::sync::oneshot;
use tokio::time::Duration;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::{CoreInput, MailboxMessage, MessageSource};

/// Tool for receiving a message from the session's mailbox.
pub(crate) struct RecvMessageTool;

#[async_trait]
impl Tool for RecvMessageTool {
    fn name(&self) -> &str {
        "recv_message"
    }

    fn description(&self) -> &str {
        "Receive a message from another agent session. Returns the message content \
         or null if non_blocking=true and no message is available."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "source": {
                    "type": "string",
                    "description": "Source session ID, or 'any' for any sender."
                },
                "non_blocking": {
                    "type": "boolean",
                    "description": "If true, return immediately with null if no message. Default false."
                },
                "timeout_ms": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "Optional timeout in milliseconds for blocking waits. Returns an error if the deadline expires."
                }
            },
            "required": ["source"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let source_str = arguments
            .get("source")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: source".into(),
            })?;

        let non_blocking = arguments
            .get("non_blocking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let timeout = arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis);

        let source = if source_str == "any" {
            MessageSource::Any
        } else {
            let source_id =
                crate::tool::wait_child::parse_session_id(source_str).ok_or_else(|| {
                    ToolError::InvalidArguments {
                        message: format!("invalid source session_id: {source_str}"),
                    }
                })?;
            MessageSource::Session(source_id)
        };

        let core_input = context
            .core_input
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no core_input channel available".into(),
            })?;

        let (reply_tx, reply_rx) = oneshot::channel();
        core_input
            .send(CoreInput::RecvMessage {
                session_id: context.session_id,
                source,
                non_blocking,
                timeout,
                reply: reply_tx,
            })
            .await
            .map_err(|_| ToolError::Internal {
                message: "core_input channel closed".into(),
            })?;

        match reply_rx.await {
            Ok(Some(MailboxMessage { from, content })) => {
                let payload = serde_json::json!({
                    "from": from,
                    "content": content,
                });
                Ok(ToolOutput::success(payload.to_string()))
            }
            Ok(None) => Ok(ToolOutput::success("null")),
            Err(_) => Ok(ToolOutput::error("recv reply channel dropped")),
        }
    }
}
