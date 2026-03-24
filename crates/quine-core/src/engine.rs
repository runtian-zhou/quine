use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
use tokio::sync::{mpsc, oneshot};

use crate::channel::{CoreHandle, CoreInput, CoreOutput, ToolOutcome};
use crate::error::CoreError;
use crate::filesystem::OverlayFilesystem;
use crate::session::{SessionId, SessionState};
use crate::tool::{
    ask_user::AskUserTool, bash::BashTool, read::ReadTool, write::WriteTool, ExecutionContext,
    InteractionChannel, InteractionRequest, InteractionResponse, ToolRegistry,
};

/// Per-session context held by the core event loop.
struct SessionContext {
    state: SessionState,
    #[allow(dead_code)]
    system_prompt: Option<String>,
    /// Conversation history for this session.
    history: Vec<Message>,
    /// Tool definitions available to the LLM.
    tools: Vec<ToolDefinition>,
    /// Registry of tool implementations.
    tool_registry: ToolRegistry,
    /// Session filesystem.
    filesystem: Arc<dyn crate::filesystem::SessionFilesystem>,
    /// Working directory for this session.
    working_directory: PathBuf,
    /// Sender for pending interaction responses (tool_use_id -> sender).
    pending_interaction: Option<oneshot::Sender<InteractionResponse>>,
}

impl SessionContext {
    async fn new(
        system_prompt: Option<String>,
        working_directory: PathBuf,
    ) -> Result<Self, CoreError> {
        let session_dir = std::env::temp_dir()
            .join("quine-sessions")
            .join(uuid::Uuid::new_v4().to_string());

        let filesystem = Arc::new(
            OverlayFilesystem::new(working_directory.clone(), session_dir)
                .await
                .map_err(|e| CoreError::Internal {
                    message: format!("failed to create overlay filesystem: {e}"),
                })?,
        );

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Arc::new(ReadTool));
        tool_registry.register(Arc::new(WriteTool));
        tool_registry.register(Arc::new(BashTool));
        tool_registry.register(Arc::new(AskUserTool));

        let tools = tool_registry.tool_definitions();

        let mut history = Vec::new();
        if let Some(prompt) = &system_prompt {
            history.push(Message::system(prompt.clone()));
        }

        Ok(Self {
            state: SessionState::Idle,
            system_prompt,
            history,
            tools,
            tool_registry,
            filesystem,
            working_directory,
            pending_interaction: None,
        })
    }
}

/// Send the current conversation to the LLM and stream the response.
async fn call_llm(
    provider: &dyn LlmProvider,
    session: &SessionContext,
    session_id: SessionId,
    output: &tokio::sync::mpsc::Sender<CoreOutput>,
) -> Result<LlmTurnResult, CoreError> {
    let stream_result = provider
        .send(&session.history, &session.tools)
        .await
        .map_err(|e| CoreError::LlmError {
            message: e.to_string(),
        })?;

    let mut stream = stream_result;
    let mut full_text = String::new();
    let mut tool_calls = Vec::new();

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(LlmEvent::TextDelta { text }) => {
                full_text.push_str(&text);
                let _ = output
                    .send(CoreOutput::StreamDelta {
                        session_id,
                        delta: text,
                    })
                    .await;
            }
            Ok(LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            }) => {
                tool_calls.push(PendingToolCall {
                    tool_use_id,
                    tool_name,
                    arguments,
                });
            }
            Ok(LlmEvent::Done) => break,
            Err(e) => {
                return Err(CoreError::LlmError {
                    message: e.to_string(),
                });
            }
        }
    }

    if tool_calls.is_empty() {
        Ok(LlmTurnResult::Text(full_text))
    } else {
        Ok(LlmTurnResult::ToolCalls {
            text_before: if full_text.is_empty() {
                None
            } else {
                Some(full_text)
            },
            calls: tool_calls,
        })
    }
}

struct PendingToolCall {
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

enum LlmTurnResult {
    Text(String),
    ToolCalls {
        #[allow(dead_code)]
        text_before: Option<String>,
        calls: Vec<PendingToolCall>,
    },
}

/// Execute a tool call directly within the core.
///
/// For interactive tools, sets up a channel and emits `InteractionNeeded`.
/// Returns the tool result as a `ToolOutcome`.
async fn execute_tool_call(
    call: &PendingToolCall,
    session: &mut SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    input: &mut mpsc::Receiver<CoreInput>,
) -> ToolOutcome {
    let tool = match session.tool_registry.get(&call.tool_name) {
        Some(t) => Arc::clone(t),
        None => {
            return ToolOutcome::Error {
                message: format!("unknown tool: {}", call.tool_name),
            };
        }
    };

    if tool.is_interactive() {
        // Create an interaction channel for this tool.
        // The tool is spawned in a separate task so we can simultaneously
        // poll for interaction requests and tool completion.
        let (req_tx, mut req_rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

        let channel = InteractionChannel { request_tx: req_tx };

        let ctx = ExecutionContext {
            session_id,
            filesystem: Arc::clone(&session.filesystem),
            working_directory: session.working_directory.clone(),
            interaction_channel: Some(channel),
        };

        let args = call.arguments.clone();
        let mut tool_handle = tokio::spawn(async move { tool.execute(args, &ctx).await });

        let output_clone = output.clone();
        let sid = session_id;

        // Poll: either the tool finishes or it needs interaction
        loop {
            tokio::select! {
                result = &mut tool_handle => {
                    return match result {
                        Ok(Ok(tool_output)) => {
                            if tool_output.is_error {
                                ToolOutcome::Error { message: tool_output.content }
                            } else {
                                ToolOutcome::Success { output: tool_output.content }
                            }
                        }
                        Ok(Err(tool_err)) => {
                            ToolOutcome::Error { message: tool_err.to_string() }
                        }
                        Err(join_err) => {
                            ToolOutcome::Error {
                                message: format!("tool task panicked: {join_err}"),
                            }
                        }
                    };
                }
                interaction = req_rx.recv() => {
                    if let Some((request, reply_tx)) = interaction {
                        // Emit InteractionNeeded
                        let _ = output_clone.send(CoreOutput::InteractionNeeded {
                            session_id: sid,
                            request,
                        }).await;

                        // Wait for CoreInput::InteractionResponse from the harness
                        loop {
                            match input.recv().await {
                                Some(CoreInput::InteractionResponse { session_id: resp_sid, response }) if resp_sid == sid => {
                                    let _ = reply_tx.send(response);
                                    break;
                                }
                                Some(CoreInput::Cancel { session_id: cancel_sid }) if cancel_sid == sid => {
                                    // Drop the reply sender to cancel the tool
                                    drop(reply_tx);
                                    return ToolOutcome::Cancelled;
                                }
                                Some(_) => {
                                    // Ignore other messages while waiting for interaction response
                                    continue;
                                }
                                None => {
                                    return ToolOutcome::Error {
                                        message: "input channel closed".into(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        // Non-interactive tool: execute directly
        let ctx = ExecutionContext {
            session_id,
            filesystem: Arc::clone(&session.filesystem),
            working_directory: session.working_directory.clone(),
            interaction_channel: None,
        };

        match tool.execute(call.arguments.clone(), &ctx).await {
            Ok(tool_output) => {
                if tool_output.is_error {
                    ToolOutcome::Error {
                        message: tool_output.content,
                    }
                } else {
                    ToolOutcome::Success {
                        output: tool_output.content,
                    }
                }
            }
            Err(tool_err) => ToolOutcome::Error {
                message: tool_err.to_string(),
            },
        }
    }
}

/// Handle the result of an LLM turn: either complete with text or process tool calls.
///
/// This function handles the tool execution loop: when the LLM requests tools,
/// it executes them and calls the LLM again until the LLM produces text.
async fn handle_llm_turn(
    provider: &dyn LlmProvider,
    session: &mut SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    input: &mut mpsc::Receiver<CoreInput>,
) {
    loop {
        match call_llm(provider, session, session_id, output).await {
            Ok(LlmTurnResult::Text(full_text)) => {
                session.history.push(Message::assistant(&full_text));

                let _ = output
                    .send(CoreOutput::TextComplete {
                        session_id,
                        full_text,
                    })
                    .await;

                session.state = SessionState::Idle;
                let _ = output.send(CoreOutput::TurnComplete { session_id }).await;
                break;
            }
            Ok(LlmTurnResult::ToolCalls { text_before, calls }) => {
                if let Some(text) = &text_before {
                    session.history.push(Message::assistant(text));
                }

                // Execute each tool call directly
                for call in &calls {
                    // Emit ToolRequest for informational purposes
                    let _ = output
                        .send(CoreOutput::ToolRequest {
                            session_id,
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            arguments: call.arguments.clone(),
                        })
                        .await;

                    let result = execute_tool_call(call, session, session_id, output, input).await;

                    // Append tool result to history
                    let (tool_output, is_error) = match &result {
                        ToolOutcome::Success { output } => (output.clone(), false),
                        ToolOutcome::Error { message } => (message.clone(), true),
                        ToolOutcome::Cancelled => {
                            ("Tool execution was cancelled".to_string(), true)
                        }
                    };
                    session.history.push(Message::tool_result(
                        &call.tool_use_id,
                        &tool_output,
                        is_error,
                    ));
                }

                // Call LLM again with tool results
                continue;
            }
            Err(error) => {
                session.state = SessionState::Idle;
                let _ = output
                    .send(CoreOutput::SessionError { session_id, error })
                    .await;
                break;
            }
        }
    }
}

/// Run the core event loop, processing inputs and emitting outputs.
///
/// The `provider` is used to send conversation history to the LLM and
/// stream back responses. Tools are executed directly within the core.
pub async fn run_core_loop(mut handle: CoreHandle, provider: Box<dyn LlmProvider>) {
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();

    while let Some(input) = handle.input.recv().await {
        match input {
            CoreInput::CreateSession {
                session_id,
                system_prompt,
                working_directory,
                reply,
            } => {
                if sessions.contains_key(&session_id) {
                    let _ = reply.send(Err("session already exists".into()));
                    continue;
                }

                let work_dir = working_directory
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                match SessionContext::new(system_prompt, work_dir).await {
                    Ok(ctx) => {
                        sessions.insert(session_id, ctx);
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Idle,
                            })
                            .await;
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        let _ = reply.send(Err(format!("failed to create session: {e}")));
                    }
                }
            }

            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Streaming;
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Streaming,
                        })
                        .await;

                    session.history.push(Message::user(&content));

                    handle_llm_turn(
                        &*provider,
                        session,
                        session_id,
                        &handle.output,
                        &mut handle.input,
                    )
                    .await;
                } else {
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionError {
                            session_id,
                            error: CoreError::SessionNotFound,
                        })
                        .await;
                }
            }

            CoreInput::ToolResult {
                session_id,
                tool_use_id,
                result,
            } => {
                // Legacy path: the harness sent a tool result.
                // With the new architecture, tools are executed in-core,
                // but we still accept external tool results for backward compat.
                if let Some(session) = sessions.get_mut(&session_id) {
                    if session.state != SessionState::AwaitingToolResult {
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::InvalidState {
                                    expected: SessionState::AwaitingToolResult,
                                    actual: session.state,
                                },
                            })
                            .await;
                    } else {
                        let (output_text, is_error) = match &result {
                            ToolOutcome::Success { output } => (output.clone(), false),
                            ToolOutcome::Error { message } => (message.clone(), true),
                            ToolOutcome::Cancelled => {
                                ("Tool execution was cancelled".to_string(), true)
                            }
                        };
                        session.history.push(Message::tool_result(
                            &tool_use_id,
                            &output_text,
                            is_error,
                        ));

                        session.state = SessionState::Streaming;
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;

                        handle_llm_turn(
                            &*provider,
                            session,
                            session_id,
                            &handle.output,
                            &mut handle.input,
                        )
                        .await;
                    }
                } else {
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionError {
                            session_id,
                            error: CoreError::SessionNotFound,
                        })
                        .await;
                }
            }

            CoreInput::InteractionResponse { .. } => {
                // Interaction responses are handled within execute_tool_call.
                // If we get one here, it means no tool is waiting for it.
            }

            CoreInput::Cancel { session_id } => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
                    // Drop any pending interaction
                    session.pending_interaction.take();
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Idle,
                        })
                        .await;
                }
            }

            CoreInput::Shutdown => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{create_channels, ChannelConfig};
    use std::pin::Pin;
    use tokio::sync::oneshot;

    /// A mock LLM provider that returns a fixed text response.
    struct MockProvider {
        response_text: String,
    }

    impl MockProvider {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
            }
        }

        fn empty() -> Self {
            Self::new("")
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = self.response_text.clone();
            let events = if text.is_empty() {
                vec![Ok(LlmEvent::Done)]
            } else {
                vec![Ok(LlmEvent::TextDelta { text }), Ok(LlmEvent::Done)]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn create_session_and_shutdown() {
        let (harness, core) = create_channels(ChannelConfig::default());

        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(MockProvider::empty())));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_to_unknown_session_errors() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(MockProvider::empty())));

        let session_id = SessionId::new();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::SessionError { error, .. } => match error {
                CoreError::SessionNotFound => {}
                other => panic!("expected SessionNotFound, got {other:?}"),
            },
            other => panic!("expected SessionError, got {other:?}"),
        }

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_produces_turn_complete() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(MockProvider::empty())));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain the SessionStateChanged from creation
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        // Expect: SessionStateChanged(Streaming), TextComplete, TurnComplete
        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TextComplete { .. }));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_session_id_returns_error() {
        let (harness, core) = create_channels(ChannelConfig::default());

        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(MockProvider::empty())));

        let session_id = SessionId::new();

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_ok());

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_err());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_streams_llm_response() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = MockProvider::new("Hello from the LLM!");
        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(provider)));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::StreamDelta { delta, .. } => {
                assert_eq!(delta, "Hello from the LLM!");
            }
            other => panic!("expected StreamDelta, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::TextComplete { full_text, .. } => {
                assert_eq!(full_text, "Hello from the LLM!");
            }
            other => panic!("expected TextComplete, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tool_call_executed_in_core() {
        // Provider that returns a tool call for read_file on first send, then text on second
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
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
                    // Ask to read a file that we'll create in the test
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_1".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "echo hello_world"}),
                        }),
                        Ok(LlmEvent::Done),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Done!".into(),
                        }),
                        Ok(LlmEvent::Done),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        };
        let loop_handle = tokio::spawn(run_core_loop(core, Box::new(provider)));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap(); // SessionStateChanged(Idle)

        // Send user message
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "run echo".into(),
            })
            .await
            .unwrap();

        // Collect events until TurnComplete
        let mut got_tool_request = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(event)) => match event {
                    CoreOutput::ToolRequest { tool_name, .. } => {
                        assert_eq!(tool_name, "bash");
                        got_tool_request = true;
                    }
                    CoreOutput::TextComplete { full_text, .. } => {
                        assert_eq!(full_text, "Done!");
                        got_text_complete = true;
                    }
                    CoreOutput::TurnComplete { .. } => {
                        got_turn_complete = true;
                        break;
                    }
                    _ => {} // SessionStateChanged, StreamDelta, etc.
                },
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for events"),
            }
        }

        assert!(got_tool_request, "should have received ToolRequest");
        assert!(got_text_complete, "should have received TextComplete");
        assert!(got_turn_complete, "should have received TurnComplete");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }
}
