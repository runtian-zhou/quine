use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Tool for receiving a message from the session's mailbox.
///
/// Note: The actual mailbox receive is handled by the engine, which
/// injects messages into the session's conversation. This tool is a
/// placeholder that will be connected when the engine supports mailboxes.
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
                }
            },
            "required": ["source"]
        })
    }

    async fn execute(
        &self,
        _arguments: serde_json::Value,
        _context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        // The recv_message tool's blocking behavior requires engine-level
        // integration to wait on the mailbox channel. For now, return a
        // placeholder indicating no messages are available.
        Ok(ToolOutput::success("null"))
    }
}
