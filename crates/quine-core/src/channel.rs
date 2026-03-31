use std::path::PathBuf;

use quine_llm::Message;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::error::CoreError;
use crate::persistence::CoreCheckpoint;
use crate::session::{ExitStatus, InheritanceFlags, SessionId, SessionSignal, SessionState};
use crate::skill::Skill;
use crate::tool;

/// Operations the harness sends into the core event loop.
#[derive(Debug)]
pub enum CoreInput {
    /// Start a new agent session.
    CreateSession {
        session_id: SessionId,
        /// Optional system prompt override.
        system_prompt: Option<String>,
        /// The working directory for this session's filesystem.
        working_directory: Option<PathBuf>,
        /// Skills to load for this session.
        skills: Vec<Skill>,
        /// Whether this session operates in read-only plan mode.
        plan_mode: bool,
        /// Whether bash permission prompts should be auto-approved.
        auto_approve_permissions: bool,
        /// Seed the new session with these messages after the system prompt.
        initial_messages: Vec<Message>,
        /// Acknowledges session creation.
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Send a user message into an existing session.
    UserMessage {
        session_id: SessionId,
        content: String,
    },

    /// Compact a session's stored context without sending a new user message.
    CompactSession {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Return the result of a tool invocation the core previously requested.
    ToolResult {
        session_id: SessionId,
        /// Correlates to the `tool_use_id` from the corresponding `ToolRequest`.
        tool_use_id: String,
        result: ToolOutcome,
    },

    /// Provide the user's response to an interaction request.
    InteractionResponse {
        session_id: SessionId,
        response: tool::InteractionResponse,
    },

    /// Cancel any in-flight work for a session.
    Cancel { session_id: SessionId },

    /// Spawn a child session under an existing parent session.
    SpawnSession {
        parent_id: SessionId,
        child_id: SessionId,
        task: String,
        system_prompt: Option<String>,
        inheritance: InheritanceFlags,
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Send a signal to a session.
    Signal {
        session_id: SessionId,
        signal: SessionSignal,
    },

    /// Wait for a child session to exit.
    WaitSession {
        parent_id: SessionId,
        child_id: SessionId,
        reply: oneshot::Sender<Option<ExitStatus>>,
        non_blocking: bool,
    },

    /// Send an inter-session message.
    SendMessage {
        from: SessionId,
        to: SessionId,
        content: String,
    },

    /// Receive an inter-session message.
    RecvMessage {
        session_id: SessionId,
        source: MessageSource,
        non_blocking: bool,
        reply: oneshot::Sender<Option<MailboxMessage>>,
    },

    /// Persist and acknowledge a fresh checkpoint of the current core state.
    RequestCheckpoint { reply: oneshot::Sender<()> },

    /// Graceful shutdown of the entire core event loop.
    Shutdown,
}

/// Source filter for mailbox receive requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MessageSource {
    Any,
    Session(SessionId),
}

/// A delivered inter-session mailbox message.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MailboxMessage {
    pub from: SessionId,
    pub content: String,
}

/// The outcome of a tool execution performed by the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ToolOutcome {
    /// Tool succeeded with this output.
    Success { output: String },
    /// Tool failed with this error description.
    Error { message: String },
    /// Tool execution was cancelled (e.g., user denied permission).
    Cancelled,
}

/// Events the core sends out to the harness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CoreOutput {
    /// A partial reasoning token from the LLM stream.
    ReasoningDelta {
        session_id: SessionId,
        delta: String,
    },

    /// A partial text token from the LLM stream.
    StreamDelta {
        session_id: SessionId,
        delta: String,
    },

    /// The LLM has finished generating text for this turn.
    TextComplete {
        session_id: SessionId,
        full_text: String,
    },

    /// The LLM is requesting a tool invocation.
    ToolRequest {
        session_id: SessionId,
        tool_use_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },

    /// A session's state changed.
    SessionStateChanged {
        session_id: SessionId,
        state: SessionState,
    },

    /// An error occurred within a session.
    SessionError {
        session_id: SessionId,
        error: CoreError,
    },

    /// A tool needs user interaction before it can proceed.
    InteractionNeeded {
        session_id: SessionId,
        request: tool::InteractionRequest,
    },

    /// Progress update for an action plan.
    PlanProgress {
        session_id: SessionId,
        plan_id: String,
        action_id: String,
        status: String,
        remaining: usize,
        total: usize,
    },

    /// A child session was successfully spawned.
    ChildSpawned {
        parent_id: SessionId,
        child_id: SessionId,
    },

    /// A child session has exited.
    ChildExited {
        parent_id: SessionId,
        child_id: SessionId,
        status: ExitStatus,
    },

    /// An inter-session message was received.
    MessageReceived {
        session_id: SessionId,
        from: SessionId,
        content: String,
    },

    /// A tool execution completed.
    ToolResult {
        session_id: SessionId,
        tool_use_id: String,
        tool_name: String,
        content: String,
        is_error: bool,
        duration_us: u64,
    },

    /// The agent turn is fully complete.
    TurnComplete {
        session_id: SessionId,
        duration_us: u64,
        usage: Option<quine_llm::TokenUsage>,
    },

    /// A stable checkpoint was requested after a committed state transition.
    CheckpointRequested { checkpoint: CoreCheckpoint },
}

/// Configuration for channel buffer sizes.
pub struct ChannelConfig {
    /// Buffer size for the input channel (harness -> core).
    pub input_buffer: usize,
    /// Buffer size for the output channel (core -> harness).
    pub output_buffer: usize,
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            input_buffer: 64,
            output_buffer: 256,
        }
    }
}

/// The channel endpoints the harness holds.
pub struct HarnessHandle {
    /// Send operations into the core.
    pub input: mpsc::Sender<CoreInput>,
    /// Receive events from the core.
    pub output: mpsc::Receiver<CoreOutput>,
}

/// The channel endpoints the core event loop holds.
pub struct CoreHandle {
    /// Send operations back into the core event loop.
    pub input_tx: mpsc::Sender<CoreInput>,
    /// Receive operations from the harness.
    pub input: mpsc::Receiver<CoreInput>,
    /// Send events to the harness.
    pub output: mpsc::Sender<CoreOutput>,
}

/// Create a paired set of channels connecting harness and core.
pub fn create_channels(config: ChannelConfig) -> (HarnessHandle, CoreHandle) {
    let (input_tx, input_rx) = mpsc::channel(config.input_buffer);
    let (output_tx, output_rx) = mpsc::channel(config.output_buffer);

    let harness = HarnessHandle {
        input: input_tx.clone(),
        output: output_rx,
    };

    let core = CoreHandle {
        input_tx: input_tx.clone(),
        input: input_rx,
        output: output_tx,
    };

    (harness, core)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn channels_send_and_receive() {
        let (mut harness, mut core) = create_channels(ChannelConfig::default());

        let session_id = SessionId::new();

        // Harness sends a user message
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        // Core receives it
        let msg = core.input.recv().await.unwrap();
        match msg {
            CoreInput::UserMessage { content, .. } => assert_eq!(content, "hello"),
            _ => panic!("expected UserMessage"),
        }

        // Core sends a stream delta back
        core.output
            .send(CoreOutput::StreamDelta {
                session_id,
                delta: "hi".into(),
            })
            .await
            .unwrap();

        // Harness receives it
        let event = harness.output.recv().await.unwrap();
        match event {
            CoreOutput::StreamDelta { delta, .. } => assert_eq!(delta, "hi"),
            _ => panic!("expected StreamDelta"),
        }
    }

    #[tokio::test]
    async fn create_session_with_oneshot_reply() {
        let (harness, mut core) = create_channels(ChannelConfig::default());

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
                reply: reply_tx,
            })
            .await
            .unwrap();

        // Core receives and acknowledges
        let msg = core.input.recv().await.unwrap();
        match msg {
            CoreInput::CreateSession { reply, .. } => {
                reply.send(Ok(())).unwrap();
            }
            _ => panic!("expected CreateSession"),
        }

        // Harness gets the ack
        assert!(reply_rx.await.unwrap().is_ok());
    }
}
