use async_trait::async_trait;
use quine_core::{
    create_channels, ChannelConfig, CoreInput, CoreOutput, HarnessHandle, InteractionResponse,
    PermissionChecker, SessionId,
};
use quine_llm::LlmProvider;
use tokio::sync::{broadcast, oneshot, Mutex};

use crate::config::SessionConfig;
use crate::error::HarnessError;
use crate::service::HarnessService;

/// Local in-process harness implementation.
///
/// Spawns the core event loop in a background task and fans out events to
/// subscribers via a broadcast channel. Tools are now executed directly
/// within the core, so this harness simply forwards events.
pub struct LocalHarness {
    harness_input: tokio::sync::mpsc::Sender<CoreInput>,
    event_tx: broadcast::Sender<CoreOutput>,
    /// Handle for the core event loop task.
    _core_task: tokio::task::JoinHandle<()>,
    /// Handle for the event fan-out task.
    _fanout_task: tokio::task::JoinHandle<()>,
}

impl LocalHarness {
    /// Create a new `LocalHarness` that spawns the core event loop with the
    /// given LLM provider and optional permission checker.
    pub fn new(
        provider: Box<dyn LlmProvider>,
        permission_checker: Option<Box<dyn PermissionChecker>>,
    ) -> Self {
        let (harness_handle, core_handle) = create_channels(ChannelConfig::default());

        let HarnessHandle { input, output } = harness_handle;
        let harness_input = input.clone();

        // Broadcast channel for fanning out core events.
        let (event_tx, _) = broadcast::channel::<CoreOutput>(256);
        let event_tx_clone = event_tx.clone();

        // Spawn the core event loop.
        let core_task = tokio::spawn(quine_core::run_core_loop(
            core_handle,
            provider,
            permission_checker,
        ));

        // Spawn a fan-out task that reads from the core output channel and
        // broadcasts events. The core now handles tool execution directly,
        // so no stub tool results are needed.
        let fanout_input = input;
        let fanout_task = tokio::spawn(Self::fanout_loop(
            Mutex::new(output),
            event_tx_clone,
            fanout_input,
        ));

        Self {
            harness_input,
            event_tx,
            _core_task: core_task,
            _fanout_task: fanout_task,
        }
    }

    /// Fan-out loop: reads events from the core and broadcasts them.
    ///
    /// InteractionNeeded events are broadcast to subscribers (e.g., the CLI)
    /// which are responsible for collecting the user's response and sending
    /// it back via `submit_interaction_response`.
    async fn fanout_loop(
        output: Mutex<tokio::sync::mpsc::Receiver<CoreOutput>>,
        event_tx: broadcast::Sender<CoreOutput>,
        _input: tokio::sync::mpsc::Sender<CoreInput>,
    ) {
        let mut output = output.into_inner();
        while let Some(event) = output.recv().await {
            // Broadcast to all subscribers (ignore errors if no receivers).
            let _ = event_tx.send(event);
        }
    }
}

#[async_trait]
impl HarnessService for LocalHarness {
    async fn create_session(&self, config: SessionConfig) -> Result<SessionId, HarnessError> {
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.harness_input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: config.system_prompt,
                working_directory: config.working_directory,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;

        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|reason| HarnessError::SessionCreationFailed { reason })?;

        Ok(session_id)
    }

    async fn send_message(
        &self,
        session_id: SessionId,
        content: String,
    ) -> Result<(), HarnessError> {
        self.harness_input
            .send(CoreInput::UserMessage {
                session_id,
                content,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn submit_tool_result(
        &self,
        session_id: SessionId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    ) -> Result<(), HarnessError> {
        let result = if is_error {
            quine_core::ToolOutcome::Error { message: output }
        } else {
            quine_core::ToolOutcome::Success { output }
        };

        self.harness_input
            .send(CoreInput::ToolResult {
                session_id,
                tool_use_id,
                result,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn submit_interaction_response(
        &self,
        session_id: SessionId,
        response: InteractionResponse,
    ) -> Result<(), HarnessError> {
        self.harness_input
            .send(CoreInput::InteractionResponse {
                session_id,
                response,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn cancel(&self, session_id: SessionId) -> Result<(), HarnessError> {
        self.harness_input
            .send(CoreInput::Cancel { session_id })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn shutdown(&self) -> Result<(), HarnessError> {
        self.harness_input
            .send(CoreInput::Shutdown)
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    fn subscribe(&self) -> broadcast::Receiver<CoreOutput> {
        self.event_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_core::CoreOutput;
    use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
    use std::pin::Pin;

    /// A mock provider that returns a fixed text response.
    struct MockProvider;

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let events = vec![
                Ok(LlmEvent::TextDelta {
                    text: "Hello!".into(),
                }),
                Ok(LlmEvent::Done),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn local_harness_create_session_and_message() {
        let harness = LocalHarness::new(Box::new(MockProvider), None);
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        harness.send_message(session_id, "hi".into()).await.unwrap();

        // Collect events until TurnComplete
        let mut got_delta = false;
        let mut got_complete = false;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(event)) => match event {
                    CoreOutput::StreamDelta { .. } => got_delta = true,
                    CoreOutput::TurnComplete { .. } => {
                        got_complete = true;
                        break;
                    }
                    _ => {}
                },
                Ok(Err(_)) => break,
                Err(_) => panic!("timeout waiting for events"),
            }
        }

        assert!(got_delta);
        assert!(got_complete);

        harness.shutdown().await.unwrap();
    }
}
