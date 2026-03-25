use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::channel::CoreInput;
use crate::session::SessionSignal;

/// Tool for sending a signal to an agent session.
pub(crate) struct SignalTool;

#[async_trait]
impl Tool for SignalTool {
    fn name(&self) -> &str {
        "signal"
    }

    fn description(&self) -> &str {
        "Send a signal to an agent session: term (graceful stop), kill (immediate), \
         stop (pause), or continue (resume)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "session_id": {
                    "type": "string",
                    "description": "The session ID to signal."
                },
                "signal": {
                    "type": "string",
                    "enum": ["term", "kill", "stop", "continue"],
                    "description": "The signal to send."
                }
            },
            "required": ["session_id", "signal"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let _session_id_str = arguments
            .get("session_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: session_id".into(),
            })?;

        let signal_str = arguments
            .get("signal")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: signal".into(),
            })?;

        let signal = match signal_str {
            "term" => SessionSignal::Term,
            "kill" => SessionSignal::Kill,
            "stop" => SessionSignal::Stop,
            "continue" => SessionSignal::Continue,
            other => {
                return Err(ToolError::InvalidArguments {
                    message: format!("invalid signal: {other}"),
                })
            }
        };

        let target_id =
            crate::tool::wait_child::parse_session_id(_session_id_str).ok_or_else(|| {
                ToolError::InvalidArguments {
                    message: format!("invalid session_id: {_session_id_str}"),
                }
            })?;

        let core_input = context
            .core_input
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no core_input channel available".into(),
            })?;

        core_input
            .send(CoreInput::Signal {
                session_id: target_id,
                signal,
            })
            .await
            .map_err(|_| ToolError::Internal {
                message: "core_input channel closed".into(),
            })?;

        Ok(ToolOutput::success(format!(
            "signal {signal_str} sent to {_session_id_str}"
        )))
    }
}
