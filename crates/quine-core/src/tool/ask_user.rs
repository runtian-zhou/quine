use async_trait::async_trait;

use super::{ExecutionContext, InteractionKind, InteractionRequest, Tool, ToolError, ToolOutput};

/// Tool for asking the user a question.
///
/// This is an interactive tool that sends a prompt to the user and waits
/// for their response via the `InteractionChannel`.
pub(crate) struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. Use this when you need \
         clarification or confirmation from the user before proceeding."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user."
                }
            },
            "required": ["question"]
        })
    }

    fn is_interactive(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let question = arguments
            .get("question")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: question".into(),
            })?;

        let channel = context
            .interaction_channel
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no interaction channel available for interactive tool".into(),
            })?;

        let response = channel
            .ask(InteractionRequest {
                prompt: question.to_string(),
                kind: InteractionKind::Question,
            })
            .await?;

        Ok(ToolOutput::success(response.response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::session::SessionId;
    use crate::tool::InteractionResponse;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::{mpsc, oneshot};

    #[tokio::test]
    async fn ask_user_sends_and_receives() {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();

        let (tx, mut rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

        let channel = super::super::InteractionChannel { request_tx: tx };

        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: Some(channel),
            plan_store: crate::tool::plan::new_plan_store(),
        };

        let tool = AskUserTool;

        // Spawn the tool execution
        let handle = tokio::spawn(async move {
            tool.execute(serde_json::json!({"question": "What is your name?"}), &ctx)
                .await
        });

        // Receive the interaction request and respond
        let (req, reply_tx) = rx.recv().await.unwrap();
        assert_eq!(req.prompt, "What is your name?");
        reply_tx
            .send(InteractionResponse {
                response: "Alice".into(),
            })
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.content, "Alice");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn ask_user_no_channel_errors() {
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
        };

        let tool = AskUserTool;
        let result = tool
            .execute(serde_json::json!({"question": "hello?"}), &ctx)
            .await;

        assert!(matches!(result, Err(ToolError::Internal { .. })));
    }
}
