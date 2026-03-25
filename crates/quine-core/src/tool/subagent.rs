use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message};

use super::{ExecutionContext, Tool, ToolError, ToolOutput, ToolRegistry};
use crate::filesystem::SessionFilesystem;
use crate::permission::{PermissionChecker, PermissionContext, PermissionDecision};
use crate::session::SessionId;
use crate::tool::{bash::BashTool, plan::PlanTool, read::ReadTool, write::WriteTool};

/// Default timeout for subagent execution (5 minutes).
const DEFAULT_TIMEOUT_SECS: u64 = 300;

/// Tool that spawns a child agent session, waits for completion, and returns
/// the result. This is the primary mechanism for delegating subtasks.
pub(crate) struct SubagentTool {
    provider: Arc<dyn LlmProvider>,
    permission_checker: Option<Arc<dyn PermissionChecker>>,
}

impl SubagentTool {
    pub(crate) fn new(
        provider: Arc<dyn LlmProvider>,
        permission_checker: Option<Arc<dyn PermissionChecker>>,
    ) -> Self {
        Self {
            provider,
            permission_checker,
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

        match run_subagent(
            &*self.provider,
            self.permission_checker.as_deref(),
            task,
            system_prompt,
            Arc::clone(&context.filesystem),
            context.working_directory.clone(),
            timeout,
        )
        .await
        {
            Ok(output) => Ok(ToolOutput::success(output)),
            Err(err) => Ok(ToolOutput::error(err)),
        }
    }
}

/// Run an autonomous agent loop: send task to LLM, execute tool calls,
/// repeat until the LLM returns text without tool calls (or timeout).
async fn run_subagent(
    provider: &dyn LlmProvider,
    permission_checker: Option<&dyn PermissionChecker>,
    task: &str,
    system_prompt: Option<&str>,
    filesystem: Arc<dyn SessionFilesystem>,
    working_directory: PathBuf,
    timeout: Duration,
) -> Result<String, String> {
    let result = tokio::time::timeout(
        timeout,
        run_subagent_inner(
            provider,
            permission_checker,
            task,
            system_prompt,
            filesystem,
            working_directory,
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
    permission_checker: Option<&dyn PermissionChecker>,
    task: &str,
    system_prompt: Option<&str>,
    filesystem: Arc<dyn SessionFilesystem>,
    working_directory: PathBuf,
) -> Result<String, String> {
    let session_id = SessionId::new();
    let plan_store = crate::tool::plan::new_plan_store();

    // Build tool registry for the child (no AskUser — no interaction channel).
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Arc::new(ReadTool));
    tool_registry.register(Arc::new(WriteTool));
    tool_registry.register(Arc::new(BashTool));
    tool_registry.register(Arc::new(PlanTool::new(plan_store.clone())));
    // Note: no SubagentTool registered here to avoid unbounded recursion.

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
                Ok(LlmEvent::TextDelta { text }) => full_text.push_str(&text),
                Ok(LlmEvent::ToolCall {
                    tool_use_id,
                    tool_name,
                    arguments,
                }) => {
                    tool_calls.push((tool_use_id, tool_name, arguments));
                }
                Ok(LlmEvent::Done) => break,
                Err(e) => return Err(format!("LLM stream error: {e}")),
            }
        }

        if tool_calls.is_empty() {
            // LLM produced text without tool calls — done.
            return Ok(full_text);
        }

        // Record any text before tool calls.
        if !full_text.is_empty() {
            history.push(Message::assistant(&full_text));
        }

        // Execute each tool call.
        for (tool_use_id, tool_name, arguments) in &tool_calls {
            // Permission check (auto-allow RequiresConfirmation in subagent context).
            if let Some(checker) = permission_checker {
                let perm_ctx = PermissionContext {
                    session_id,
                    working_directory: working_directory.clone(),
                };
                match checker.check(tool_name, arguments, &perm_ctx).await {
                    Ok(PermissionDecision::Allow) => {}
                    Ok(PermissionDecision::RequiresConfirmation { .. }) => {
                        // Auto-allow in subagent — no user interaction available.
                    }
                    Ok(PermissionDecision::Deny { reason }) => {
                        history.push(Message::tool_result(
                            tool_use_id,
                            format!("permission denied: {reason}"),
                            true,
                        ));
                        continue;
                    }
                    Err(_) => {
                        // On checker error, auto-allow.
                    }
                }
            }

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
                interaction_channel: None,
                plan_store: plan_store.clone(),
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
    use quine_llm::ToolDefinition;
    use std::pin::Pin;
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
            let events = vec![Ok(LlmEvent::TextDelta { text }), Ok(LlmEvent::Done)];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Mock provider that issues a bash tool call on first send, then returns text.
    struct ToolThenTextProvider {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait]
    impl LlmProvider for ToolThenTextProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let count = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let events = if count == 0 {
                vec![
                    Ok(LlmEvent::ToolCall {
                        tool_use_id: "tc_sub_1".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo SUBAGENT_OUTPUT_42"}),
                    }),
                    Ok(LlmEvent::Done),
                ]
            } else {
                vec![
                    Ok(LlmEvent::TextDelta {
                        text: "The command output was: SUBAGENT_OUTPUT_42".into(),
                    }),
                    Ok(LlmEvent::Done),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn subagent_simple_task() {
        let provider: Arc<dyn LlmProvider> = Arc::new(TextProvider::new("SUBAGENT_RESULT_777"));
        let tool = SubagentTool::new(provider, None);
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
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let tool = SubagentTool::new(provider, None);
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
                    Ok(LlmEvent::Done),
                ];
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(InfiniteToolProvider);
        let tool = SubagentTool::new(provider, None);
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
        let tool = SubagentTool::new(provider, None);
        let (_base, _session, ctx) = make_context().await;

        let result = tool
            .execute(serde_json::json!({"task": "Do something"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.content.contains("LLM error"));
    }
}
