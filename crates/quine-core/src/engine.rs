use std::collections::HashMap;

use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};

use crate::channel::{CoreHandle, CoreInput, CoreOutput, ToolOutcome};
use crate::error::CoreError;
use crate::session::{SessionId, SessionState};

/// Per-session context held by the core event loop.
struct SessionContext {
    state: SessionState,
    #[allow(dead_code)] // May be used for session management features later.
    system_prompt: Option<String>,
    /// Conversation history for this session.
    history: Vec<Message>,
    /// Tool definitions available to the LLM.
    tools: Vec<ToolDefinition>,
}

impl SessionContext {
    fn new(system_prompt: Option<String>) -> Self {
        let mut history = Vec::new();
        if let Some(prompt) = &system_prompt {
            history.push(Message::system(prompt.clone()));
        }
        Self {
            state: SessionState::Idle,
            system_prompt,
            history,
            tools: Vec::new(),
        }
    }
}

/// Send the current conversation to the LLM and stream the response.
///
/// Returns the accumulated assistant text if the LLM responded with text,
/// or a list of tool calls if it requested tools.
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

/// Run the core event loop, processing inputs and emitting outputs.
///
/// The `provider` is used to send conversation history to the LLM and
/// stream back responses.
pub async fn run_core_loop(mut handle: CoreHandle, provider: Box<dyn LlmProvider>) {
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();

    while let Some(input) = handle.input.recv().await {
        match input {
            CoreInput::CreateSession {
                session_id,
                system_prompt,
                reply,
            } => {
                if sessions.contains_key(&session_id) {
                    let _ = reply.send(Err("session already exists".into()));
                    continue;
                }
                let ctx = SessionContext::new(system_prompt);
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

                    // Append user message to history
                    session.history.push(Message::user(&content));

                    // Call LLM and process the response
                    match call_llm(&*provider, session, session_id, &handle.output).await {
                        Ok(LlmTurnResult::Text(full_text)) => {
                            // Append assistant response to history
                            session.history.push(Message::assistant(&full_text));

                            let _ = handle
                                .output
                                .send(CoreOutput::TextComplete {
                                    session_id,
                                    full_text,
                                })
                                .await;

                            session.state = SessionState::Idle;
                            let _ = handle
                                .output
                                .send(CoreOutput::TurnComplete { session_id })
                                .await;
                        }
                        Ok(LlmTurnResult::ToolCalls { text_before, calls }) => {
                            // If there was text before tool calls, add it to history
                            if let Some(text) = &text_before {
                                session.history.push(Message::assistant(text));
                            }

                            // Emit tool requests and transition to awaiting
                            for call in &calls {
                                let _ = handle
                                    .output
                                    .send(CoreOutput::ToolRequest {
                                        session_id,
                                        tool_use_id: call.tool_use_id.clone(),
                                        tool_name: call.tool_name.clone(),
                                        arguments: call.arguments.clone(),
                                    })
                                    .await;
                            }

                            session.state = SessionState::AwaitingToolResult;
                            let _ = handle
                                .output
                                .send(CoreOutput::SessionStateChanged {
                                    session_id,
                                    state: SessionState::AwaitingToolResult,
                                })
                                .await;
                        }
                        Err(error) => {
                            session.state = SessionState::Idle;
                            let _ = handle
                                .output
                                .send(CoreOutput::SessionError { session_id, error })
                                .await;
                        }
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

            CoreInput::ToolResult {
                session_id,
                tool_use_id,
                result,
            } => {
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
                        // Append tool result to history
                        let (output, is_error) = match &result {
                            ToolOutcome::Success { output } => (output.clone(), false),
                            ToolOutcome::Error { message } => (message.clone(), true),
                            ToolOutcome::Cancelled => {
                                ("Tool execution was cancelled".to_string(), true)
                            }
                        };
                        session
                            .history
                            .push(Message::tool_result(&tool_use_id, &output, is_error));

                        // Transition back to streaming and call LLM again
                        session.state = SessionState::Streaming;
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;

                        match call_llm(&*provider, session, session_id, &handle.output).await {
                            Ok(LlmTurnResult::Text(full_text)) => {
                                session.history.push(Message::assistant(&full_text));

                                let _ = handle
                                    .output
                                    .send(CoreOutput::TextComplete {
                                        session_id,
                                        full_text,
                                    })
                                    .await;

                                session.state = SessionState::Idle;
                                let _ = handle
                                    .output
                                    .send(CoreOutput::TurnComplete { session_id })
                                    .await;
                            }
                            Ok(LlmTurnResult::ToolCalls { text_before, calls }) => {
                                if let Some(text) = &text_before {
                                    session.history.push(Message::assistant(text));
                                }

                                for call in &calls {
                                    let _ = handle
                                        .output
                                        .send(CoreOutput::ToolRequest {
                                            session_id,
                                            tool_use_id: call.tool_use_id.clone(),
                                            tool_name: call.tool_name.clone(),
                                            arguments: call.arguments.clone(),
                                        })
                                        .await;
                                }

                                session.state = SessionState::AwaitingToolResult;
                                let _ = handle
                                    .output
                                    .send(CoreOutput::SessionStateChanged {
                                        session_id,
                                        state: SessionState::AwaitingToolResult,
                                    })
                                    .await;
                            }
                            Err(error) => {
                                session.state = SessionState::Idle;
                                let _ = handle
                                    .output
                                    .send(CoreOutput::SessionError { session_id, error })
                                    .await;
                            }
                        }
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

            CoreInput::Cancel { session_id } => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
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

        // Create a session
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

        // Shutdown
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

        // Should receive a SessionError with SessionNotFound
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

        // Create session
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain the SessionStateChanged from creation
        let _ = output.recv().await.unwrap();

        // Send a user message
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

        // First creation succeeds
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_ok());

        // Second creation with same ID fails
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
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

        // Create session
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain the SessionStateChanged from creation
        let _ = output.recv().await.unwrap();

        // Send a user message
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        // Expect: SessionStateChanged(Streaming), StreamDelta, TextComplete, TurnComplete
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
    async fn tool_call_and_result_flow() {
        // Provider that returns a tool call on first send, then text on second
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
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_1".into(),
                            tool_name: "read_file".into(),
                            arguments: serde_json::json!({"path": "/tmp/test"}),
                        }),
                        Ok(LlmEvent::Done),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "File contents are: hello".into(),
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

        // Create session
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap(); // Drain SessionStateChanged

        // Send user message
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "read /tmp/test".into(),
            })
            .await
            .unwrap();

        // Expect: Streaming state, ToolRequest, AwaitingToolResult state
        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        match &event {
            CoreOutput::ToolRequest {
                tool_use_id,
                tool_name,
                ..
            } => {
                assert_eq!(tool_use_id, "tc_1");
                assert_eq!(tool_name, "read_file");
            }
            other => panic!("expected ToolRequest, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::AwaitingToolResult,
                ..
            }
        ));

        // Send tool result
        harness
            .input
            .send(CoreInput::ToolResult {
                session_id,
                tool_use_id: "tc_1".into(),
                result: ToolOutcome::Success {
                    output: "hello".into(),
                },
            })
            .await
            .unwrap();

        // Expect: Streaming state, StreamDelta, TextComplete, TurnComplete
        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::StreamDelta { .. }));

        let event = output.recv().await.unwrap();
        match &event {
            CoreOutput::TextComplete { full_text, .. } => {
                assert_eq!(full_text, "File contents are: hello");
            }
            other => panic!("expected TextComplete, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }
}
