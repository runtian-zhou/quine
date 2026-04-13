use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message, WebProvider};

use super::{ExecutionContext, InteractionChannel, Tool, ToolError, ToolOutput, ToolRegistry};
use crate::filesystem::SessionFilesystem;
use crate::session::SessionId;
use crate::tool::{
    ask_user::AskUserTool, bash::BashTool, plan::PlanTool, read::ReadTool, web_open::WebOpenTool,
    web_search::WebSearchTool, write::WriteTool,
};

/// Default timeout for subagent execution (5 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Tool that spawns a child agent session, waits for completion, and returns
/// the result. This is the primary mechanism for delegating subtasks.
pub(crate) struct SubagentTool {
    provider: Arc<dyn LlmProvider>,
    web_provider: Arc<dyn WebProvider>,
}

impl SubagentTool {
    pub(crate) fn new(provider: Arc<dyn LlmProvider>, web_provider: Arc<dyn WebProvider>) -> Self {
        Self {
            provider,
            web_provider,
        }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "subagent"
    }

    fn description(&self) -> &str {
        "Spawn a child agent to execute a task and return the result. The child runs \
         autonomously (LLM calls + tool execution) until it produces a final text response. \
         Use this for delegating subtasks like research, implementation, or exploration."
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
                    "description": "Optional system prompt override for the child agent."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds. Defaults to 300 (5 minutes)."
                }
            },
            "required": ["task"]
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
        let task = arguments
            .get("task")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: task".into(),
            })?;

        let system_prompt = arguments.get("system_prompt").and_then(|v| v.as_str());

        let timeout_secs = arguments
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let timeout = Duration::from_secs(timeout_secs);

        let channel = context.interaction_channel.clone();

        match run_subagent(
            &*self.provider,
            Arc::clone(&self.web_provider),
            task,
            system_prompt,
            Arc::clone(&context.filesystem),
            context.working_directory.clone(),
            timeout,
            channel,
        )
        .await
        {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Ok(ToolOutput::error(err)),
        }
    }
}

/// Truncate a string to `max_len` characters, appending "..." if truncated.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        let mut end = max_len;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Create a wrapper `InteractionChannel` that annotates all forwarded requests
/// with the given `source_label`.
fn wrap_channel_with_label(parent: &InteractionChannel, label: String) -> InteractionChannel {
    use super::{InteractionRequest, InteractionResponse};
    use tokio::sync::{mpsc, oneshot};

    let parent_tx = parent.request_tx.clone();
    let (child_tx, mut child_rx) =
        mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

    tokio::spawn(async move {
        while let Some((mut req, reply)) = child_rx.recv().await {
            req.source_label = Some(label.clone());
            let _ = parent_tx.send((req, reply)).await;
        }
    });

    InteractionChannel {
        request_tx: child_tx,
    }
}

/// Run an autonomous agent loop: send task to LLM, execute tool calls,
/// repeat until the LLM returns text without tool calls (or timeout).
#[allow(clippy::too_many_arguments)]
async fn run_subagent(
    provider: &dyn LlmProvider,
    web_provider: Arc<dyn WebProvider>,
    task: &str,
    system_prompt: Option<&str>,
    filesystem: Arc<dyn SessionFilesystem>,
    working_directory: PathBuf,
    timeout: Duration,
    parent_channel: Option<InteractionChannel>,
) -> Result<String, String> {
    let result = tokio::time::timeout(
        timeout,
        run_subagent_inner(
            provider,
            web_provider,
            task,
            system_prompt,
            filesystem,
            working_directory,
            parent_channel,
        ),
    )
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => Err(format!("subagent timed out after {}s", timeout.as_secs())),
    }
}

async fn run_subagent_inner(
    provider: &dyn LlmProvider,
    web_provider: Arc<dyn WebProvider>,
    task: &str,
    system_prompt: Option<&str>,
    filesystem: Arc<dyn SessionFilesystem>,
    working_directory: PathBuf,
    parent_channel: Option<InteractionChannel>,
) -> Result<String, String> {
    let session_id = SessionId::new();
    let plan_store = crate::tool::plan::new_plan_store();

    // Build tool registry for the child.
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Arc::new(ReadTool));
    tool_registry.register(Arc::new(WriteTool));
    tool_registry.register(Arc::new(BashTool));
    tool_registry.register(Arc::new(PlanTool::new(plan_store.clone())));
    tool_registry.register(Arc::new(WebSearchTool::new(Arc::clone(&web_provider))));
    tool_registry.register(Arc::new(WebOpenTool::new(Arc::clone(&web_provider))));
    // Register AskUserTool so subagent can ask the user questions.
    tool_registry.register(Arc::new(AskUserTool));
    // Note: no SubagentTool registered here to avoid unbounded recursion.

    // Build a wrapped interaction channel that annotates requests with source_label.
    let label = format!("subagent: {}", truncate(task, 60));
    let child_channel = parent_channel
        .as_ref()
        .map(|ch| wrap_channel_with_label(ch, label));

    let tools = tool_registry.tool_definitions();

    // Build conversation history.
    let mut history: Vec<Message> = Vec::new();
    if let Some(prompt) = system_prompt {
        history.push(Message::system(prompt.to_string()));
    }
    history.push(Message::user(task));

    // Agent loop: call LLM, execute tools, repeat.
    loop {
        let stream_result = provider
            .send(&history, &tools)
            .await
            .map_err(|e| format!("LLM error: {e}"))?;

        let mut stream = stream_result;
        let mut full_text = String::new();
        let mut tool_calls: Vec<(String, String, serde_json::Value)> = Vec::new();

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(LlmEvent::ReasoningDelta { .. }) => {}
                Ok(LlmEvent::TextDelta { text }) => full_text.push_str(&text),
                Ok(LlmEvent::ToolCall {
                    tool_use_id,
                    tool_name,
                    arguments,
                }) => {
                    tool_calls.push((tool_use_id, tool_name, arguments));
                }
                Ok(LlmEvent::Done { .. }) => break,
                Err(e) => return Err(format!("LLM stream error: {e}")),
            }
        }

        if tool_calls.is_empty() {
            // LLM produced text without tool calls — done.
            return Ok(full_text);
        }

        // Record the assistant tool-use message before executing tools so the
        // next LLM request includes the required tool-call IDs.
        let tool_use_requests: Vec<quine_llm::ToolUseRequest> = tool_calls
            .iter()
            .map(
                |(tool_use_id, tool_name, arguments)| quine_llm::ToolUseRequest {
                    tool_use_id: tool_use_id.clone(),
                    tool_name: tool_name.clone(),
                    arguments: arguments.clone(),
                },
            )
            .collect();
        history.push(Message::assistant_tool_use(
            if full_text.is_empty() {
                None
            } else {
                Some(full_text.clone())
            },
            tool_use_requests,
        ));

        // Execute each tool call.
        for (tool_use_id, tool_name, arguments) in &tool_calls {
            let tool = match tool_registry.get(tool_name) {
                Some(t) => Arc::clone(t),
                None => {
                    history.push(Message::tool_result(
                        tool_use_id,
                        format!("unknown tool: {tool_name}"),
                        true,
                    ));
                    continue;
                }
            };

            let ctx = ExecutionContext {
                session_id,
                filesystem: Arc::clone(&filesystem),
                working_directory: working_directory.clone(),
                interaction_channel: child_channel.clone(),
                plan_store: plan_store.clone(),
                session_group: session_id.to_string(),
                python_runtime: crate::python::PythonRuntime::new(),
                core_input: None,
                cancellation: crate::tool::CancellationChannel::never(),
            };

            match tool.execute(arguments.clone(), &ctx).await {
                Ok(tool_output) => {
                    history.push(Message::tool_result(
                        tool_use_id,
                        &tool_output.content,
                        tool_output.is_error,
                    ));
                }
                Err(tool_err) => {
                    history.push(Message::tool_result(
                        tool_use_id,
                        tool_err.to_string(),
                        true,
                    ));
                }
            }
        }

        // Continue loop — call LLM again with tool results.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use quine_llm::{NoopWebProvider, ToolDefinition};
    use std::pin::Pin;
    use std::sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    };
    use tempfile::TempDir;

    async fn make_context() -> (TempDir, TempDir, ExecutionContext) {
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
            cancellation: crate::tool::CancellationChannel::never(),
        };
        (base, session_dir, ctx)
    }

    /// Mock provider that returns a fixed text response (no tool calls).
    struct TextProvider {
        text: String,
    }

    impl TextProvider {
        fn new(text: &str) -> Self {
            Self {
                text: text.to_string(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for TextProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = self.text.clone();
            let events = vec![
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Mock provider that issues a bash tool call on first send, then returns text.
    struct ToolThenTextProvider {
        call_count: AtomicU32,
    }

    #[async_trait]
    impl LlmProvider for ToolThenTextProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);

            let events = if count == 0 {
                vec![
                    Ok(LlmEvent::ToolCall {
                        tool_use_id: "tc_sub_1".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo SUBAGENT_OUTPUT_42"}),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            } else {
                vec![
                    Ok(LlmEvent::TextDelta {
                        text: "The command output was: SUBAGENT_OUTPUT_42".into(),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn subagent_simple_task() {
        let provider: Arc<dyn LlmProvider> = Arc::new(TextProvider::new("SUBAGENT_RESULT_777"));
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(serde_json::json!({"task": "Say SUBAGENT_RESULT_777"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("SUBAGENT_RESULT_777"));
    }

    #[tokio::test]
    async fn subagent_with_tool_use() {
        let provider: Arc<dyn LlmProvider> = Arc::new(ToolThenTextProvider {
            call_count: AtomicU32::new(0),
        });
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(
                serde_json::json!({"task": "Run echo and report output"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("SUBAGENT_OUTPUT_42"));
    }

    #[tokio::test]
    async fn subagent_timeout() {
        /// Provider that always returns a tool call (infinite loop).
        struct InfiniteToolProvider;

        #[async_trait]
        impl LlmProvider for InfiniteToolProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let events = vec![
                    Ok(LlmEvent::ToolCall {
                        tool_use_id: format!("tc_{}", uuid::Uuid::new_v4()),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo loop"}),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ];
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(InfiniteToolProvider);
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(
                serde_json::json!({"task": "Do something", "timeout": 2}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("timed out"));
    }

    #[tokio::test]
    async fn subagent_llm_error() {
        /// Provider that always returns an error.
        struct ErrorProvider;

        #[async_trait]
        impl LlmProvider for ErrorProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                Err(anyhow::anyhow!("LLM service unavailable"))
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(ErrorProvider);
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(serde_json::json!({"task": "Do something"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("LLM error"));
    }

    #[tokio::test]
    async fn subagent_ask_user_bubbles_to_parent() {
        use crate::tool::{InteractionRequest, InteractionResponse};
        use tokio::sync::{mpsc, oneshot};

        /// Mock provider that calls ask_user on first send, then returns the answer.
        struct AskUserProvider {
            call_count: AtomicU32,
        }

        #[async_trait]
        impl LlmProvider for AskUserProvider {
            async fn send(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);

                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_ask_1".into(),
                            tool_name: "ask_user".into(),
                            arguments: serde_json::json!({"question": "What is your favorite color?"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    // Extract the tool result from messages to include in the response.
                    let last = messages
                        .last()
                        .map(|m| format!("{:?}", m))
                        .unwrap_or_default();
                    let text = if last.contains("blue") {
                        "The user said: blue".to_string()
                    } else {
                        "The user responded".to_string()
                    };
                    vec![
                        Ok(LlmEvent::TextDelta { text }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (tx, mut rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

        let parent_channel = InteractionChannel { request_tx: tx };

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
            interaction_channel: Some(parent_channel),
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };

        let provider: Arc<dyn LlmProvider> = Arc::new(AskUserProvider {
            call_count: AtomicU32::new(0),
        });
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));

        let handle = tokio::spawn(async move {
            tool.execute(
                serde_json::json!({"task": "Ask the user their favorite color"}),
                &ctx,
            )
            .await
        });

        // Receive the interaction request from the subagent.
        let (req, reply_tx) = rx.recv().await.expect("should receive interaction request");
        assert_eq!(req.prompt, "What is your favorite color?");
        assert!(
            req.source_label.is_some(),
            "source_label should be set by subagent wrapper"
        );
        assert!(
            req.source_label.as_ref().unwrap().contains("subagent:"),
            "source_label should contain 'subagent:'"
        );

        // Send a response back.
        reply_tx
            .send(InteractionResponse {
                response: "blue".into(),
                selected_indices: Vec::new(),
            })
            .unwrap();

        let result = handle.await.unwrap().unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("blue"),
            "subagent should include user's answer in output"
        );
    }

    #[tokio::test]
    async fn subagent_without_interaction_channel_still_works() {
        let provider: Arc<dyn LlmProvider> = Arc::new(TextProvider::new("NO_CHANNEL_RESULT"));
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;
        // ctx.interaction_channel is None (from make_context)

        let result = tool
            .execute(
                serde_json::json!({"task": "Do something without interaction"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("NO_CHANNEL_RESULT"));
    }

    #[tokio::test]
    async fn subagent_records_assistant_tool_use_before_tool_result() {
        struct ProtocolCheckingProvider {
            call_count: AtomicU32,
        }

        #[async_trait]
        impl LlmProvider for ProtocolCheckingProvider {
            async fn send(
                &self,
                messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self.call_count.fetch_add(1, Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_protocol_1".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "echo protocol_ok"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    let assistant_tool_use_index = messages.iter().position(|message| {
                        matches!(
                            &message.content,
                            quine_llm::MessageContent::ToolUse { tool_calls, .. }
                                if message.role == quine_llm::Role::Assistant
                                    && tool_calls.iter().any(|call| call.tool_use_id == "tc_protocol_1")
                        )
                    });
                    let tool_result_index = messages.iter().position(|message| {
                        matches!(
                            &message.content,
                            quine_llm::MessageContent::ToolResult { tool_use_id, .. }
                                if message.role == quine_llm::Role::Tool && tool_use_id == "tc_protocol_1"
                        )
                    });

                    assert_eq!(assistant_tool_use_index, Some(messages.len() - 2));
                    assert_eq!(tool_result_index, Some(messages.len() - 1));

                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "protocol preserved".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(ProtocolCheckingProvider {
            call_count: AtomicU32::new(0),
        });
        let tool = SubagentTool::new(provider, Arc::new(NoopWebProvider));
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(serde_json::json!({"task": "delegate with bash"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(result.content, "protocol preserved");
    }

    #[tokio::test]
    async fn wrap_channel_with_label_sets_source() {
        use crate::tool::{InteractionRequest, InteractionResponse};
        use tokio::sync::{mpsc, oneshot};

        let (parent_tx, mut parent_rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);
        let parent_channel = InteractionChannel {
            request_tx: parent_tx,
        };

        let wrapped = wrap_channel_with_label(&parent_channel, "test-label".to_string());

        // Send a request through the wrapped channel.
        let (reply_tx, _reply_rx) = oneshot::channel();
        wrapped
            .request_tx
            .send((
                crate::tool::InteractionRequest {
                    prompt: "hello?".into(),
                    kind: crate::tool::InteractionKind::Question,
                    options: Vec::new(),
                    allow_freeform: false,
                    source_label: None,
                },
                reply_tx,
            ))
            .await
            .unwrap();

        // Receive on the parent side and verify source_label was set.
        let (req, _reply) = parent_rx
            .recv()
            .await
            .expect("should receive forwarded request");
        assert_eq!(req.prompt, "hello?");
        assert_eq!(
            req.source_label,
            Some("test-label".to_string()),
            "wrap_channel_with_label should set source_label to 'test-label'"
        );
    }
}
