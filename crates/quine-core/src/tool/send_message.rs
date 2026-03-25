use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::CoreInput;

/// Tool for sending a message to another agent session.
pub(crate) struct SendMessageTool;

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a message to another agent session's mailbox."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "target": {
                    "type": "string",
                    "description": "The session ID to send the message to."
                },
                "content": {
                    "type": "string",
                    "description": "The message content."
                }
            },
            "required": ["target", "content"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let target_str = arguments
            .get("target")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: target".into(),
            })?;

        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: content".into(),
            })?;

        let target_id = crate::tool::wait_child::parse_session_id(target_str).ok_or_else(|| {
            ToolError::InvalidArguments {
                message: format!("invalid target session_id: {target_str}"),
            }
        })?;

        let core_input = context
            .core_input
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no core_input channel available".into(),
            })?;

        core_input
            .send(CoreInput::SendMessage {
                from: context.session_id,
                to: target_id,
                content: content.to_string(),
            })
            .await
            .map_err(|_| ToolError::Internal {
                message: "core_input channel closed".into(),
            })?;

        Ok(ToolOutput::success("message sent"))
    }
}
