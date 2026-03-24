use std::collections::HashMap;

use crate::channel::{CoreHandle, CoreInput, CoreOutput};
use crate::error::CoreError;
use crate::session::{SessionId, SessionState};

/// Per-session context held by the core event loop.
struct SessionContext {
    state: SessionState,
    #[allow(dead_code)] // Will be used when LLM integration is added.
    system_prompt: Option<String>,
}

impl SessionContext {
    fn new(system_prompt: Option<String>) -> Self {
        Self {
            state: SessionState::Idle,
            system_prompt,
        }
    }
}

/// Run the core event loop, processing inputs and emitting outputs.
///
/// This is a skeleton — it handles session lifecycle but does not yet
/// integrate with an LLM provider.
pub async fn run_core_loop(mut handle: CoreHandle) {
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();

    while let Some(input) = handle.input.recv().await {
        match input {
            CoreInput::CreateSession {
                session_id,
                system_prompt,
                reply,
            } => {
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
                content: _,
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

                    // TODO: call LLM provider, stream response, handle tool calls
                    // For now, emit a placeholder completion.
                    let _ = handle
                        .output
                        .send(CoreOutput::TextComplete {
                            session_id,
                            full_text: String::new(),
                        })
                        .await;

                    session.state = SessionState::Idle;
                    let _ = handle
                        .output
                        .send(CoreOutput::TurnComplete { session_id })
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
                result: _,
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
                        // TODO: feed tool result back into conversation
                        let _ = tool_use_id;
                        session.state = SessionState::Idle;
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
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn create_session_and_shutdown() {
        let (harness, core) = create_channels(ChannelConfig::default());

        let loop_handle = tokio::spawn(run_core_loop(core));

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

        let loop_handle = tokio::spawn(run_core_loop(core));

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

        let loop_handle = tokio::spawn(run_core_loop(core));

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
}
