use async_trait::async_trait;

use super::{
    ExecutionContext, InteractionKind, InteractionRequest, SelectOption, Tool, ToolError,
    ToolOutput,
};

/// Tool for asking the user a question.
///
/// Supports free-form text questions, single-select from options, and
/// multi-select from options. This is an interactive tool that sends a
/// prompt to the user and waits for their response via the `InteractionChannel`.
pub(crate) struct AskUserTool;

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. Supports free-form text input, \
         single-select from a list of options, and multi-select from options."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user."
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of options for the user to choose from."
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "If true, user can select multiple options. Defaults to false."
                },
                "allow_freeform": {
                    "type": "boolean",
                    "description": "If true, user can type a custom answer in addition to options. Defaults to true."
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

        let options: Vec<SelectOption> = arguments
            .get("options")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| {
                        v.as_str().map(|s| SelectOption {
                            label: s.to_string(),
                            description: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let multi_select = arguments
            .get("multi_select")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let allow_freeform = arguments
            .get("allow_freeform")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let kind = if options.is_empty() {
            InteractionKind::Question
        } else if multi_select {
            InteractionKind::MultiSelect
        } else {
            InteractionKind::SingleSelect
        };

        let channel = context
            .interaction_channel
            .as_ref()
            .ok_or_else(|| ToolError::Internal {
                message: "no interaction channel available for interactive tool".into(),
            })?;

        let response = channel
            .ask(
                InteractionRequest {
                    prompt: question.to_string(),
                    kind,
                    options,
                    allow_freeform,
                    source_label: None,
                },
                &context.cancellation,
            )
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

    async fn make_test_context() -> (
        TempDir,
        TempDir,
        ExecutionContext,
        mpsc::Receiver<(InteractionRequest, oneshot::Sender<InteractionResponse>)>,
    ) {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();

        let (tx, rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

        let channel = super::super::InteractionChannel { request_tx: tx };

        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: Some(channel),
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            permission_runtime: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        (base, session_dir, ctx, rx)
    }

    #[tokio::test]
    async fn ask_user_sends_and_receives() {
        let (_base, _session, ctx, mut rx) = make_test_context().await;
        let tool = AskUserTool;

        let handle = tokio::spawn(async move {
            tool.execute(serde_json::json!({"question": "What is your name?"}), &ctx)
                .await
        });

        let (req, reply_tx) = rx.recv().await.unwrap();
        assert_eq!(req.prompt, "What is your name?");
        assert_eq!(req.kind, InteractionKind::Question);
        assert!(req.options.is_empty());
        reply_tx
            .send(InteractionResponse {
                response: "Alice".into(),
                selected_indices: Vec::new(),
            })
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.content, "Alice");
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn ask_user_single_select() {
        let (_base, _session, ctx, mut rx) = make_test_context().await;
        let tool = AskUserTool;

        let handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({
                    "question": "Pick a color",
                    "options": ["red", "green", "blue"]
                }),
                &ctx,
            )
            .await
        });

        let (req, reply_tx) = rx.recv().await.unwrap();
        assert_eq!(req.kind, InteractionKind::SingleSelect);
        assert_eq!(req.options.len(), 3);
        assert_eq!(req.options[0].label, "red");
        reply_tx
            .send(InteractionResponse {
                response: "green".into(),
                selected_indices: vec![1],
            })
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.content, "green");
    }

    #[tokio::test]
    async fn ask_user_multi_select() {
        let (_base, _session, ctx, mut rx) = make_test_context().await;
        let tool = AskUserTool;

        let handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({
                    "question": "Pick features",
                    "options": ["A", "B", "C"],
                    "multi_select": true
                }),
                &ctx,
            )
            .await
        });

        let (req, reply_tx) = rx.recv().await.unwrap();
        assert_eq!(req.kind, InteractionKind::MultiSelect);
        reply_tx
            .send(InteractionResponse {
                response: "A, C".into(),
                selected_indices: vec![0, 2],
            })
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert_eq!(result.content, "A, C");
    }

    #[tokio::test]
    async fn ask_user_cancels_while_waiting_for_response() {
        let base = TempDir::new().unwrap();
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();

        let (tx, mut rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);
        let channel = super::super::InteractionChannel { request_tx: tx };
        let (cancel_tx, cancellation) = crate::tool::CancellationChannel::new_pair();

        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: Some(channel),
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            permission_runtime: None,
            cancellation,
        };

        let tool = AskUserTool;
        let handle = tokio::spawn(async move {
            tool.execute(serde_json::json!({"question": "Wait?"}), &ctx)
                .await
        });

        let (_req, _reply_tx) = rx.recv().await.unwrap();
        cancel_tx.send(true).unwrap();

        let result = handle.await.unwrap();
        assert!(matches!(result, Err(ToolError::Cancelled)));
    }
}
