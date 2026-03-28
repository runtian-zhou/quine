use async_trait::async_trait;
use std::cmp::Ordering;
use std::collections::BinaryHeap;
use std::collections::HashMap;
use std::sync::Arc;

use quine_core::{
    create_channels, load_skills, ChannelConfig, CoreInput, CoreOutput, HarnessHandle,
    InheritanceFlags, InteractionResponse, PermissionChecker, SessionId, SessionSignal, Skill,
};
use quine_llm::LlmProvider;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::{Duration, Instant};

use crate::config::SessionConfig;
use crate::error::HarnessError;
use crate::service::HarnessService;

/// Local in-process harness implementation.
///
/// Spawns the core event loop in a background task and fans out events to
/// subscribers via a broadcast channel. Tools are now executed directly
/// within the core, so this harness simply forwards events.
pub struct LocalHarness {
    scheduler_tx: mpsc::Sender<ScheduledCommand>,
    event_tx: broadcast::Sender<CoreOutput>,
    /// Handle for the core event loop task.
    _core_task: tokio::task::JoinHandle<()>,
    /// Handle for the scheduler task that serializes core-loop work.
    _scheduler_task: tokio::task::JoinHandle<()>,
    /// Handle for the event fan-out task.
    _fanout_task: tokio::task::JoinHandle<()>,
}

impl LocalHarness {
    /// Create a new `LocalHarness` that spawns the core event loop with the
    /// given LLM provider and optional permission checker.
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        permission_checker: Option<Arc<dyn PermissionChecker>>,
    ) -> Self {
        let (harness_handle, core_handle) = create_channels(ChannelConfig::default());

        let HarnessHandle { input, output } = harness_handle;
        let scheduler_input = input.clone();

        // Broadcast channel for fanning out core events.
        let (event_tx, _) = broadcast::channel::<CoreOutput>(256);
        let event_tx_clone = event_tx.clone();
        let (scheduler_tx, scheduler_rx) = mpsc::channel(256);

        // Spawn the core event loop.
        let core_task = tokio::spawn(quine_core::run_core_loop(
            core_handle,
            provider,
            permission_checker,
        ));

        let scheduler_task = tokio::spawn(Self::scheduler_loop(scheduler_input, scheduler_rx));

        // Spawn a fan-out task that reads from the core output channel and
        // broadcasts events. The core now handles tool execution directly,
        // so no stub tool results are needed.
        let fanout_task = tokio::spawn(Self::fanout_loop(Mutex::new(output), event_tx_clone));

        Self {
            scheduler_tx,
            event_tx,
            _core_task: core_task,
            _scheduler_task: scheduler_task,
            _fanout_task: fanout_task,
        }
    }

    /// Queue a user message to be delivered after `delay`.
    pub async fn schedule_message(
        &self,
        session_id: SessionId,
        content: String,
        delay: Duration,
    ) -> Result<(), HarnessError> {
        self.enqueue(ScheduledCommand::new(
            ScheduledAction::SendMessage {
                session_id,
                content,
                reply: None,
            },
            delay,
        ))
        .await
    }

    /// Fan-out loop: reads events from the core and broadcasts them.
    ///
    /// InteractionNeeded events are broadcast to subscribers (e.g., the CLI)
    /// which are responsible for collecting the user's response and sending
    /// it back via `submit_interaction_response`.
    async fn fanout_loop(
        output: Mutex<tokio::sync::mpsc::Receiver<CoreOutput>>,
        event_tx: broadcast::Sender<CoreOutput>,
    ) {
        let mut output = output.into_inner();
        while let Some(event) = output.recv().await {
            // Broadcast to all subscribers (ignore errors if no receivers).
            let _ = event_tx.send(event);
        }
    }

    async fn enqueue(&self, command: ScheduledCommand) -> Result<(), HarnessError> {
        self.scheduler_tx
            .send(command)
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn scheduler_loop(
        harness_input: mpsc::Sender<CoreInput>,
        mut scheduler_rx: mpsc::Receiver<ScheduledCommand>,
    ) {
        let mut pending = BinaryHeap::new();
        let mut next_sequence = 0_u64;
        let mut channel_closed = false;
        let mut ipc_mailboxes: HashMap<String, Vec<String>> = HashMap::new();

        loop {
            let now = Instant::now();
            while pending
                .peek()
                .is_some_and(|command: &QueuedCommand| command.execute_at <= now)
            {
                let queued = pending.pop().expect("pending heap is not empty");
                if Self::dispatch_scheduled_action(
                    &harness_input,
                    &mut ipc_mailboxes,
                    queued.action,
                )
                .await
                {
                    return;
                }
            }

            if channel_closed && pending.is_empty() {
                return;
            }

            if pending.is_empty() {
                match scheduler_rx.recv().await {
                    Some(command) => {
                        pending.push(QueuedCommand::new(command, next_sequence));
                        next_sequence += 1;
                    }
                    None => channel_closed = true,
                }
                continue;
            }

            let next_deadline = pending
                .peek()
                .map(|command| command.execute_at)
                .expect("pending heap is not empty");

            if channel_closed {
                tokio::time::sleep_until(next_deadline).await;
                continue;
            }

            tokio::select! {
                maybe_command = scheduler_rx.recv() => {
                    match maybe_command {
                        Some(command) => {
                            pending.push(QueuedCommand::new(command, next_sequence));
                            next_sequence += 1;
                        }
                        None => channel_closed = true,
                    }
                }
                _ = tokio::time::sleep_until(next_deadline) => {}
            }
        }
    }

    async fn dispatch_scheduled_action(
        harness_input: &mpsc::Sender<CoreInput>,
        ipc_mailboxes: &mut HashMap<String, Vec<String>>,
        action: ScheduledAction,
    ) -> bool {
        match action {
            ScheduledAction::CreateSession {
                session_id,
                config,
                reply,
            } => {
                let skills = load_skills_from_config(&config.skills).await;
                let (core_reply_tx, core_reply_rx) = oneshot::channel();
                let result = match harness_input
                    .send(CoreInput::CreateSession {
                        session_id,
                        system_prompt: config.system_prompt,
                        working_directory: config.working_directory,
                        skills,
                        plan_mode: config.plan_mode,
                        auto_approve_permissions: config.auto_approve_permissions,
                        initial_messages: config.initial_messages,
                        reply: core_reply_tx,
                    })
                    .await
                {
                    Ok(()) => core_reply_rx
                        .await
                        .map_err(|_| HarnessError::CoreChannelClosed)
                        .and_then(|result| {
                            result.map_err(|reason| HarnessError::SessionCreationFailed { reason })
                        }),
                    Err(_) => Err(HarnessError::CoreChannelClosed),
                };
                let _ = reply.send(result);
                false
            }
            ScheduledAction::SendMessage {
                session_id,
                content,
                reply,
            } => {
                let result = harness_input
                    .send(CoreInput::UserMessage {
                        session_id,
                        content,
                    })
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                if let Some(reply) = reply {
                    let _ = reply.send(result);
                }
                false
            }
            ScheduledAction::SubmitToolResult {
                session_id,
                tool_use_id,
                output,
                is_error,
                reply,
            } => {
                let result = harness_input
                    .send(CoreInput::ToolResult {
                        session_id,
                        tool_use_id,
                        result: if is_error {
                            quine_core::ToolOutcome::Error { message: output }
                        } else {
                            quine_core::ToolOutcome::Success { output }
                        },
                    })
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                let _ = reply.send(result);
                false
            }
            ScheduledAction::SubmitInteractionResponse {
                session_id,
                response,
                reply,
            } => {
                let result = harness_input
                    .send(CoreInput::InteractionResponse {
                        session_id,
                        response,
                    })
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                let _ = reply.send(result);
                false
            }
            ScheduledAction::Cancel { session_id, reply } => {
                let result = harness_input
                    .send(CoreInput::Cancel { session_id })
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                let _ = reply.send(result);
                false
            }
            ScheduledAction::Shutdown { reply } => {
                let result = harness_input
                    .send(CoreInput::Shutdown)
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                let _ = reply.send(result);
                true
            }
            ScheduledAction::SpawnChildSession {
                parent_id,
                child_id,
                task,
                system_prompt,
                reply,
            } => {
                let (core_reply_tx, core_reply_rx) = oneshot::channel();
                let result = match harness_input
                    .send(CoreInput::SpawnSession {
                        parent_id,
                        child_id,
                        task,
                        system_prompt,
                        inheritance: InheritanceFlags::default(),
                        reply: core_reply_tx,
                    })
                    .await
                {
                    Ok(()) => core_reply_rx
                        .await
                        .map_err(|_| HarnessError::CoreChannelClosed)
                        .and_then(|result| {
                            result.map_err(|reason| HarnessError::SessionCreationFailed { reason })
                        }),
                    Err(_) => Err(HarnessError::CoreChannelClosed),
                };
                let _ = reply.send(result);
                false
            }
            ScheduledAction::SignalSession {
                session_id,
                signal,
                reply,
            } => {
                let result = harness_input
                    .send(CoreInput::Signal { session_id, signal })
                    .await
                    .map_err(|_| HarnessError::CoreChannelClosed);
                let _ = reply.send(result);
                false
            }
            ScheduledAction::SendIpcMessage {
                target,
                content,
                reply,
            } => {
                ipc_mailboxes.entry(target).or_default().push(content);
                let _ = reply.send(Ok(()));
                false
            }
            ScheduledAction::RecvIpcMessage { source, reply } => {
                let message = ipc_mailboxes.get_mut(&source).and_then(|messages| {
                    if messages.is_empty() {
                        None
                    } else {
                        Some(messages.remove(0))
                    }
                });
                let _ = reply.send(Ok(message));
                false
            }
        }
    }
}

/// Load skills by name using `quine-core` default skill support.
async fn load_skills_from_config(skill_names: &[String]) -> Vec<Skill> {
    if skill_names.is_empty() {
        return Vec::new();
    }

    let project_root = std::env::current_dir().unwrap_or_default();
    load_skills(&project_root, skill_names).await
}

#[async_trait]
impl HarnessService for LocalHarness {
    async fn create_session(&self, config: SessionConfig) -> Result<SessionId, HarnessError> {
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::CreateSession {
                session_id,
                config,
                reply: reply_tx,
            },
        ))
        .await?;

        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)??;

        Ok(session_id)
    }

    async fn send_message(
        &self,
        session_id: SessionId,
        content: String,
    ) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(ScheduledAction::SendMessage {
            session_id,
            content,
            reply: Some(reply_tx),
        }))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn submit_tool_result(
        &self,
        session_id: SessionId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    ) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::SubmitToolResult {
                session_id,
                tool_use_id,
                output,
                is_error,
                reply: reply_tx,
            },
        ))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn submit_interaction_response(
        &self,
        session_id: SessionId,
        response: InteractionResponse,
    ) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::SubmitInteractionResponse {
                session_id,
                response,
                reply: reply_tx,
            },
        ))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn cancel(&self, session_id: SessionId) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(ScheduledAction::Cancel {
            session_id,
            reply: reply_tx,
        }))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn shutdown(&self) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(ScheduledAction::Shutdown {
            reply: reply_tx,
        }))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    fn subscribe(&self) -> broadcast::Receiver<CoreOutput> {
        self.event_tx.subscribe()
    }

    async fn spawn_child_session(
        &self,
        parent_id: Option<SessionId>,
        task: String,
        system_prompt: Option<String>,
    ) -> Result<SessionId, HarnessError> {
        let child_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::SpawnChildSession {
                parent_id: parent_id.unwrap_or_default(),
                child_id,
                task,
                system_prompt,
                reply: reply_tx,
            },
        ))
        .await?;

        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)??;

        Ok(child_id)
    }

    async fn signal_session(
        &self,
        session_id: SessionId,
        signal: SessionSignal,
    ) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::SignalSession {
                session_id,
                signal,
                reply: reply_tx,
            },
        ))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn send_ipc_message(&self, target: String, content: String) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::SendIpcMessage {
                target,
                content,
                reply: reply_tx,
            },
        ))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }

    async fn recv_ipc_message(
        &self,
        source: String,
        _non_blocking: bool,
    ) -> Result<Option<String>, HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.enqueue(ScheduledCommand::immediate(
            ScheduledAction::RecvIpcMessage {
                source,
                reply: reply_tx,
            },
        ))
        .await?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
    }
}

struct ScheduledCommand {
    execute_at: Instant,
    action: ScheduledAction,
}

impl ScheduledCommand {
    fn immediate(action: ScheduledAction) -> Self {
        Self::new(action, Duration::ZERO)
    }

    fn new(action: ScheduledAction, delay: Duration) -> Self {
        Self {
            execute_at: Instant::now() + delay,
            action,
        }
    }
}

struct QueuedCommand {
    execute_at: Instant,
    sequence: u64,
    action: ScheduledAction,
}

impl QueuedCommand {
    fn new(command: ScheduledCommand, sequence: u64) -> Self {
        Self {
            execute_at: command.execute_at,
            sequence,
            action: command.action,
        }
    }
}

impl PartialEq for QueuedCommand {
    fn eq(&self, other: &Self) -> bool {
        self.execute_at == other.execute_at && self.sequence == other.sequence
    }
}

impl Eq for QueuedCommand {}

impl Ord for QueuedCommand {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .execute_at
            .cmp(&self.execute_at)
            .then_with(|| other.sequence.cmp(&self.sequence))
    }
}

impl PartialOrd for QueuedCommand {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

enum ScheduledAction {
    CreateSession {
        session_id: SessionId,
        config: SessionConfig,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    SendMessage {
        session_id: SessionId,
        content: String,
        reply: Option<oneshot::Sender<Result<(), HarnessError>>>,
    },
    SubmitToolResult {
        session_id: SessionId,
        tool_use_id: String,
        output: String,
        is_error: bool,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    SubmitInteractionResponse {
        session_id: SessionId,
        response: InteractionResponse,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    Cancel {
        session_id: SessionId,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    Shutdown {
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    SpawnChildSession {
        parent_id: SessionId,
        child_id: SessionId,
        task: String,
        system_prompt: Option<String>,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    SignalSession {
        session_id: SessionId,
        signal: SessionSignal,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    SendIpcMessage {
        target: String,
        content: String,
        reply: oneshot::Sender<Result<(), HarnessError>>,
    },
    RecvIpcMessage {
        source: String,
        reply: oneshot::Sender<Result<Option<String>, HarnessError>>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_core::{CoreOutput, SessionState};
    use quine_llm::{LlmEvent, LlmProvider, Message, MessageContent, Role, ToolDefinition};
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
                Ok(LlmEvent::Done { usage: None }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn local_harness_create_session_and_message() {
        let harness = LocalHarness::new(Arc::new(MockProvider), None);
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

    struct EchoProvider;

    #[async_trait::async_trait]
    impl LlmProvider for EchoProvider {
        async fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = messages
                .iter()
                .rev()
                .find_map(|message| match (&message.role, &message.content) {
                    (Role::User, MessageContent::Text(text)) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let events = vec![
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test(start_paused = true)]
    async fn local_harness_scheduled_message_runs_after_delay() {
        let harness = LocalHarness::new(Arc::new(EchoProvider), None);
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        assert!(matches!(
            rx.recv().await.unwrap(),
            CoreOutput::SessionStateChanged {
                state: SessionState::Idle,
                ..
            }
        ));

        harness
            .schedule_message(session_id, "scheduled".into(), Duration::from_secs(60))
            .await
            .unwrap();

        tokio::task::yield_now().await;
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));

        tokio::time::advance(Duration::from_secs(60)).await;

        let mut full_text = None;
        while full_text.is_none() {
            if let CoreOutput::TextComplete {
                full_text: text, ..
            } = rx.recv().await.unwrap()
            {
                full_text = Some(text);
            }
        }

        assert_eq!(full_text.as_deref(), Some("scheduled"));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn local_harness_orders_immediate_before_delayed_message() {
        let harness = LocalHarness::new(Arc::new(EchoProvider), None);
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        assert!(matches!(
            rx.recv().await.unwrap(),
            CoreOutput::SessionStateChanged {
                state: SessionState::Idle,
                ..
            }
        ));

        harness
            .schedule_message(session_id, "later".into(), Duration::from_secs(30))
            .await
            .unwrap();
        harness
            .send_message(session_id, "now".into())
            .await
            .unwrap();

        let mut completions = Vec::new();
        while completions.is_empty() {
            if let CoreOutput::TextComplete { full_text, .. } = rx.recv().await.unwrap() {
                completions.push(full_text);
            }
        }

        assert_eq!(completions, vec!["now"]);

        tokio::time::advance(Duration::from_secs(30)).await;

        while completions.len() < 2 {
            if let CoreOutput::TextComplete { full_text, .. } = rx.recv().await.unwrap() {
                completions.push(full_text);
            }
        }

        assert_eq!(completions, vec!["now", "later"]);
        harness.shutdown().await.unwrap();
    }
}
