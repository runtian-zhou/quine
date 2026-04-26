use std::path::PathBuf;
use std::sync::Arc;

use quine_llm::{LlmProvider, Message};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::error::CoreError;
use crate::permission::{PermissionPromptBehavior, PermissionRuleSet};
use crate::persistence::CoreCheckpoint;
use crate::session::{ExitStatus, InheritanceFlags, SessionId, SessionSignal, SessionState};
use crate::skill::Skill;
use crate::status_report::SessionStatusReport;
use crate::tool;

#[derive(Clone)]
pub struct SessionLlmConfig {
    pub provider: Arc<dyn LlmProvider>,
    pub max_context_window: Option<u64>,
    pub model_profile: Option<String>,
}

impl std::fmt::Debug for SessionLlmConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionLlmConfig")
            .field("max_context_window", &self.max_context_window)
            .field("model_profile", &self.model_profile)
            .finish()
    }
}

/// Operations the harness sends into the core event loop.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
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
        /// How permission prompts should behave for this session.
        prompt_behavior: PermissionPromptBehavior,
        /// Trusted persisted permission rules loaded by the harness.
        permission_rules: PermissionRuleSet,
        /// Seed the new session with these messages after the system prompt.
        initial_messages: Vec<Message>,
        /// Optional custom-agent durable memory key.
        agent_key: Option<String>,
        /// Optional team durable memory key.
        team_key: Option<String>,
        /// Optional shared python session-group key.
        session_group: Option<String>,
        /// Session memory scope and policy configuration.
        memory_policy: crate::memory::MemoryPolicyConfig,
        /// Selected LLM provider/runtime config for this session.
        session_llm: SessionLlmConfig,
        /// Auto-compaction threshold as a percentage of the model context window.
        auto_compact_threshold_percent: u8,
        /// Minimum number of tool rounds before status reporting begins.
        status_report_min_tool_rounds: u32,
        /// Acknowledges session creation.
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Send a user message into an existing session.
    UserMessage {
        session_id: SessionId,
        content: String,
    },

    /// Leave read-only plan mode for an existing session.
    ExitPlanMode {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Update the active LLM provider for an existing session.
    UpdateSessionLlm {
        session_id: SessionId,
        session_llm: SessionLlmConfig,
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Schedule a user message for future delivery.
    ScheduleUserMessage {
        session_id: SessionId,
        content: String,
        delay: Duration,
        cadence: Option<Duration>,
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
        prompt_behavior: PermissionPromptBehavior,
        permission_rules: PermissionRuleSet,
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
        reply: oneshot::Sender<Result<Option<ExitStatus>, String>>,
        non_blocking: bool,
        timeout: Option<Duration>,
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
        timeout: Option<Duration>,
        reply: oneshot::Sender<Option<MailboxMessage>>,
    },

    /// Send a message through the harness-facing IPC mailbox.
    SendHarnessIpcMessage {
        target: String,
        content: String,
        reply: oneshot::Sender<Result<(), String>>,
    },

    /// Receive a message from the harness-facing IPC mailbox.
    RecvHarnessIpcMessage {
        source: String,
        non_blocking: bool,
        reply: oneshot::Sender<Option<String>>,
    },

    /// Persist and acknowledge a fresh checkpoint of the current core state.
    RequestCheckpoint { reply: oneshot::Sender<()> },

    /// Internal signal that a background session-memory refresh has finished.
    SessionMemoryRefreshFinished {
        session_id: SessionId,
        last_summarized_message_index: Option<usize>,
        refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
        listing_summary: Option<String>,
    },

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

    /// Updated status report for a multi-turn tool loop.
    SessionStatusReport {
        session_id: SessionId,
        report: Option<SessionStatusReport>,
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
        cache_usage: Option<quine_llm::PromptCacheUsage>,
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
    use quine_llm::{LlmEvent, ToolDefinition};
    use std::pin::Pin;
    use std::sync::Arc;

    struct TestProvider;

    #[async_trait::async_trait]
    impl LlmProvider for TestProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            Ok(Box::pin(futures::stream::empty()))
        }
    }

    fn test_session_llm_config() -> SessionLlmConfig {
        SessionLlmConfig {
            provider: Arc::new(TestProvider),
            max_context_window: None,
            model_profile: None,
        }
    }

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
                prompt_behavior: crate::permission::PermissionPromptBehavior::Interactive,
                permission_rules: crate::permission::PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: crate::memory::MemoryPolicyConfig::default(),
                session_llm: test_session_llm_config(),
                auto_compact_threshold_percent:
                    crate::compaction::DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: crate::default_status_report_min_tool_rounds(),
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
