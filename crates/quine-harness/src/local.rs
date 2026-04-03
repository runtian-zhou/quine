use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;

use quine_core::{
    create_channels, load_skills, ChannelConfig, CoreCheckpoint, CoreInput, CoreOutput,
    HarnessHandle, InheritanceFlags, InteractionResponse, SessionId, SessionSignal, Skill,
};
use quine_llm::LlmProvider;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::Duration;

use crate::config::{default_state_dir, max_context_window_from_env, SessionConfig};
use crate::error::HarnessError;
use crate::service::HarnessService;
use crate::storage::{session_context_from_checkpoint, StorageManager};

/// Local in-process harness implementation.
///
/// Spawns the core event loop in a background task and fans out events to
/// subscribers via a broadcast channel. Tools are now executed directly
/// within the core, so this harness simply forwards events.
pub struct LocalHarness {
    core_input: mpsc::Sender<CoreInput>,
    event_tx: broadcast::Sender<CoreOutput>,
    /// Handle for the core event loop task.
    _core_task: tokio::task::JoinHandle<()>,
    /// Handle for the event fan-out task.
    _fanout_task: tokio::task::JoinHandle<()>,
    /// Durable checkpoint storage owned by the harness.
    _storage: Arc<StorageManager>,
    /// Mirrored live/restored session states.
    sessions: Arc<Mutex<HashMap<SessionId, SessionListing>>>,
}

#[derive(Debug, Clone)]
struct SessionListing {
    state: quine_core::SessionState,
    created_at: chrono::DateTime<Utc>,
    event_count: usize,
}

impl LocalHarness {
    /// Create a new `LocalHarness` that spawns the core event loop with the
    /// given LLM provider.
    pub async fn new(
        provider: Arc<dyn LlmProvider>,
        storage: Option<StorageManager>,
    ) -> Result<Self, HarnessError> {
        Self::with_storage(provider, storage, None).await
    }

    pub async fn with_archive_root(
        provider: Arc<dyn LlmProvider>,
        archive_root: Option<std::path::PathBuf>,
    ) -> Result<Self, HarnessError> {
        Self::with_storage(provider, None, archive_root).await
    }

    async fn with_storage(
        provider: Arc<dyn LlmProvider>,
        storage: Option<StorageManager>,
        archive_root: Option<std::path::PathBuf>,
    ) -> Result<Self, HarnessError> {
        let (harness_handle, core_handle) = create_channels(ChannelConfig::default());

        let HarnessHandle { input, output } = harness_handle;

        // Broadcast channel for fanning out core events.
        let (event_tx, _) = broadcast::channel::<CoreOutput>(256);
        let event_tx_clone = event_tx.clone();
        let archive_root = archive_root.unwrap_or_else(default_state_dir);
        let storage =
            Arc::new(storage.unwrap_or_else(|| StorageManager::new(archive_root.clone())));
        let restored_checkpoint =
            storage
                .load_latest_checkpoint()
                .await
                .map_err(|error| HarnessError::Internal {
                    message: format!("failed to load checkpoint: {error}"),
                })?;
        let initial_sessions = restored_checkpoint
            .as_ref()
            .map(|checkpoint| {
                checkpoint
                    .sessions
                    .iter()
                    .map(|session| {
                        (
                            session.session_id,
                            SessionListing {
                                state: session.state.into(),
                                created_at: session.created_at,
                                event_count: session.history.len(),
                            },
                        )
                    })
                    .collect::<HashMap<SessionId, SessionListing>>()
            })
            .unwrap_or_default();
        let sessions = Arc::new(Mutex::new(initial_sessions));

        let max_context_window = max_context_window_from_env();

        // Spawn the core event loop.
        let core_task = tokio::spawn(quine_core::run_core_loop_with_compaction(
            core_handle,
            provider,
            restored_checkpoint,
            archive_root,
            max_context_window,
        ));

        // Spawn a fan-out task that reads from the core output channel and
        // broadcasts events. The core now handles tool execution directly,
        // so no stub tool results are needed.
        let fanout_task = tokio::spawn(Self::fanout_loop(
            Mutex::new(output),
            event_tx_clone,
            Arc::clone(&storage),
            Arc::clone(&sessions),
        ));

        Ok(Self {
            core_input: input,
            event_tx,
            _core_task: core_task,
            _fanout_task: fanout_task,
            _storage: storage,
            sessions,
        })
    }

    /// Queue a user message to be delivered after `delay`.
    pub async fn schedule_message(
        &self,
        session_id: SessionId,
        content: String,
        delay: Duration,
    ) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::ScheduleUserMessage {
                session_id,
                content,
                delay,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    /// Fan-out loop: reads events from the core and broadcasts them.
    ///
    /// InteractionNeeded events are broadcast to subscribers (e.g., the CLI)
    /// which are responsible for collecting the user's response and sending
    /// it back via `submit_interaction_response`.
    async fn fanout_loop(
        output: Mutex<tokio::sync::mpsc::Receiver<CoreOutput>>,
        event_tx: broadcast::Sender<CoreOutput>,
        storage: Arc<StorageManager>,
        sessions: Arc<Mutex<HashMap<SessionId, SessionListing>>>,
    ) {
        let mut output = output.into_inner();
        while let Some(event) = output.recv().await {
            match &event {
                CoreOutput::CheckpointRequested { checkpoint } => {
                    if let Err(error) = storage.commit_checkpoint(checkpoint).await {
                        let _ = event_tx.send(CoreOutput::SessionError {
                            session_id: SessionId::default(),
                            error: quine_core::CoreError::Internal {
                                message: format!("failed to persist checkpoint: {error}"),
                            },
                        });
                    }
                    continue;
                }
                CoreOutput::SessionStateChanged { session_id, state } => {
                    let mut guard = sessions.lock().await;
                    if *state == quine_core::SessionState::Destroyed {
                        guard.remove(session_id);
                    } else {
                        guard
                            .entry(*session_id)
                            .and_modify(|session| session.state = *state)
                            .or_insert_with(|| SessionListing {
                                state: *state,
                                created_at: Utc::now(),
                                event_count: 0,
                            });
                    }
                }
                _ => {}
            }
            // Broadcast to all subscribers (ignore errors if no receivers).
            let _ = event_tx.send(event);
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
        let skills = load_skills_from_config(&config.skills).await;

        self.core_input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: config.system_prompt,
                working_directory: config.working_directory,
                skills,
                plan_mode: config.plan_mode,
                initial_messages: config.initial_messages,
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
        self.core_input
            .send(CoreInput::UserMessage {
                session_id,
                content,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn exit_plan_mode(&self, session_id: SessionId) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::ExitPlanMode {
                session_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|message| HarnessError::Internal { message })
    }

    async fn compact_session(&self, session_id: SessionId) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::CompactSession {
                session_id,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|message| HarnessError::Internal { message })
    }

    async fn submit_tool_result(
        &self,
        session_id: SessionId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    ) -> Result<(), HarnessError> {
        self.core_input
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
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn submit_interaction_response(
        &self,
        session_id: SessionId,
        response: InteractionResponse,
    ) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::InteractionResponse {
                session_id,
                response,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn cancel(&self, session_id: SessionId) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::Cancel { session_id })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn shutdown(&self) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::Shutdown)
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    fn subscribe(&self) -> broadcast::Receiver<CoreOutput> {
        self.event_tx.subscribe()
    }

    async fn list_sessions(&self) -> Result<Vec<serde_json::Value>, HarnessError> {
        let sessions = self.sessions.lock().await;
        let mut items: Vec<serde_json::Value> = sessions
            .iter()
            .map(|(session_id, session)| {
                let session_id = serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{session_id:?}"));
                serde_json::json!({
                    "session_id": session_id,
                    "status": format!("{:?}", session.state).to_lowercase(),
                    "first_event": session.created_at.to_rfc3339(),
                    "event_count": session.event_count,
                })
            })
            .collect();
        items.sort_by(|left, right| {
            right
                .get("first_event")
                .and_then(|value| value.as_str())
                .cmp(&left.get("first_event").and_then(|value| value.as_str()))
        });
        Ok(items)
    }

    async fn get_session_context(
        &self,
        session_id: SessionId,
    ) -> Result<CoreCheckpoint, HarnessError> {
        let sessions = self.sessions.lock().await;
        if !sessions.contains_key(&session_id) {
            return Err(HarnessError::SessionNotFound {
                session_id: serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            });
        }
        drop(sessions);

        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::RequestCheckpoint { reply: reply_tx })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;

        let checkpoint = self
            ._storage
            .load_latest_checkpoint()
            .await
            .map_err(|error| HarnessError::Internal {
                message: format!("failed to load checkpoint: {error}"),
            })?
            .ok_or_else(|| HarnessError::SessionNotFound {
                session_id: serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            })?;

        let sessions = self.sessions.lock().await;
        let live_states = sessions
            .iter()
            .map(|(id, session)| (*id, format!("{:?}", session.state).to_lowercase()))
            .collect();

        if session_context_from_checkpoint(&checkpoint, session_id, &live_states).is_none() {
            return Err(HarnessError::SessionNotFound {
                session_id: serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_default(),
            });
        }

        Ok(checkpoint)
    }

    async fn spawn_child_session(
        &self,
        parent_id: Option<SessionId>,
        task: String,
        system_prompt: Option<String>,
    ) -> Result<SessionId, HarnessError> {
        let child_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();

        self.core_input
            .send(CoreInput::SpawnSession {
                parent_id: parent_id.unwrap_or_default(),
                child_id,
                task,
                system_prompt,
                inheritance: InheritanceFlags::default(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;

        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|reason| HarnessError::SessionCreationFailed { reason })?;

        Ok(child_id)
    }

    async fn signal_session(
        &self,
        session_id: SessionId,
        signal: SessionSignal,
    ) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::Signal { session_id, signal })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }

    async fn send_ipc_message(&self, target: String, content: String) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::SendHarnessIpcMessage {
                target,
                content,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|message| HarnessError::Internal { message })
    }

    async fn recv_ipc_message(
        &self,
        source: String,
        non_blocking: bool,
    ) -> Result<Option<String>, HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::RecvHarnessIpcMessage {
                source,
                non_blocking,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        Ok(reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?)
    }

    async fn schedule_agent(
        &self,
        parent_id: Option<SessionId>,
        task: String,
        system_prompt: Option<String>,
        delay: Duration,
        cadence: Option<Duration>,
    ) -> Result<(), HarnessError> {
        self.core_input
            .send(CoreInput::ScheduleSpawnSession {
                parent_id: parent_id.unwrap_or_default(),
                task,
                system_prompt,
                delay,
                cadence,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_core::{CoreOutput, SessionState};
    use quine_llm::{LlmEvent, LlmProvider, Message, MessageContent, Role, ToolDefinition};
    use std::fs;
    use std::pin::Pin;

    fn temp_storage() -> StorageManager {
        StorageManager::new(
            std::env::temp_dir().join(format!("quine-harness-local-{}", uuid::Uuid::new_v4())),
        )
    }
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
        let harness = LocalHarness::new(Arc::new(MockProvider), None)
            .await
            .unwrap();
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

    #[tokio::test]
    async fn local_harness_restores_checkpointed_session() {
        let storage = temp_storage();
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        harness
            .send_message(session_id, "persist me".into())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            let mut rx = harness.subscribe();
            loop {
                if matches!(rx.recv().await.unwrap(), CoreOutput::TurnComplete { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();

        harness.shutdown().await.unwrap();

        let restored = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let mut rx = restored.subscribe();
        let mut saw_restored = false;
        for _ in 0..4 {
            if let Ok(Ok(CoreOutput::SessionStateChanged {
                session_id: restored_id,
                state: SessionState::Idle,
            })) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await
            {
                if restored_id == session_id {
                    saw_restored = true;
                    break;
                }
            }
        }
        assert!(saw_restored);
        restored.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(storage.root());
    }

    #[tokio::test]
    async fn local_harness_lists_restored_sessions_for_resume_e2e() {
        let storage = temp_storage();
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        harness
            .send_message(session_id, "persist and resume".into())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            let mut rx = harness.subscribe();
            loop {
                if matches!(rx.recv().await.unwrap(), CoreOutput::TurnComplete { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();

        harness.shutdown().await.unwrap();

        let restored = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id_str = serde_json::to_value(session_id)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("{session_id:?}"));
        let listed = restored.list_sessions().await.unwrap();
        let resumed = listed.iter().any(|session| {
            session
                .get("session_id")
                .and_then(|value| value.as_str())
                .map(|value| value == session_id_str)
                .unwrap_or(false)
        });
        assert!(
            resumed,
            "restored session should be visible to resume lookup"
        );

        let mut rx = restored.subscribe();
        restored
            .send_message(session_id, "after resume".into())
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match rx.recv().await.unwrap() {
                    CoreOutput::TextComplete { full_text, .. } if full_text == "after resume" => {
                        break;
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap();

        restored.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(storage.root());
    }

    #[tokio::test]
    async fn local_harness_exit_plan_mode_updates_persisted_session_state() {
        let storage = temp_storage();
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig {
                plan_mode: true,
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if harness
                    .list_sessions()
                    .await
                    .unwrap()
                    .iter()
                    .any(|session| {
                        session
                            .get("session_id")
                            .and_then(|value| value.as_str())
                            .map(|value| {
                                value
                                    == serde_json::to_value(session_id)
                                        .ok()
                                        .and_then(|value| value.as_str().map(str::to_owned))
                                        .unwrap_or_default()
                            })
                            .unwrap_or(false)
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session should appear in local session listing");

        let persisted_before = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Some(plan_mode) =
                    storage
                        .load_latest_checkpoint()
                        .await
                        .unwrap()
                        .and_then(|checkpoint| {
                            checkpoint
                                .sessions
                                .iter()
                                .find(|session| session.session_id == session_id)
                                .map(|session| session.config.plan_mode)
                        })
                {
                    break plan_mode;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session should be persisted before exit");
        assert!(persisted_before);

        harness.exit_plan_mode(session_id).await.unwrap();

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if storage
                    .load_latest_checkpoint()
                    .await
                    .unwrap()
                    .and_then(|checkpoint| {
                        checkpoint
                            .sessions
                            .iter()
                            .find(|session| session.session_id == session_id)
                            .map(|session| session.config.plan_mode)
                    })
                    == Some(false)
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("checkpoint should persist updated plan_mode");

        harness.shutdown().await.unwrap();

        let restored = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let checkpoint_restored = restored.get_session_context(session_id).await.unwrap();
        let session_restored = checkpoint_restored
            .sessions
            .iter()
            .find(|session| session.session_id == session_id)
            .expect("session should restore from checkpoint");
        assert!(!session_restored.config.plan_mode);

        restored.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(storage.root());
    }

    #[tokio::test]
    async fn local_harness_schedule_agent_one_shot() {
        let harness = LocalHarness::new(Arc::new(MockProvider), None)
            .await
            .unwrap();
        harness
            .schedule_agent(None, "do work".into(), None, Duration::from_secs(1), None)
            .await
            .unwrap();
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_harness_recvs_ipc_message() {
        let harness = LocalHarness::new(Arc::new(MockProvider), None)
            .await
            .unwrap();
        harness
            .send_ipc_message("worker".into(), "payload".into())
            .await
            .unwrap();

        let message = harness
            .recv_ipc_message("worker".into(), false)
            .await
            .unwrap();

        assert_eq!(message.as_deref(), Some("payload"));
        harness.shutdown().await.unwrap();
    }

    #[tokio::test(start_paused = true)]
    async fn local_harness_scheduled_message_runs_after_delay() {
        let harness = LocalHarness::new(Arc::new(EchoProvider), None)
            .await
            .unwrap();
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
        let _ = rx.recv().await.unwrap();

        harness
            .schedule_message(session_id, "scheduled".into(), Duration::from_secs(60))
            .await
            .unwrap();

        tokio::task::yield_now().await;

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
        let harness = LocalHarness::new(Arc::new(EchoProvider), None)
            .await
            .unwrap();
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
        let _ = rx.recv().await.unwrap();

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
