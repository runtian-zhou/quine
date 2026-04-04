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

use crate::config::{
    default_memory_dir_from_state_dir, default_state_dir, max_context_window_from_env,
    SessionConfig,
};

use crate::error::HarnessError;
use crate::service::HarnessService;
use crate::storage::{session_context_from_checkpoint, StorageManager};
use crate::MemoryStore;

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
    plan_mode: bool,
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
        let (archive_root, storage) = match (archive_root, storage) {
            (Some(archive_root), Some(storage)) => (archive_root, Arc::new(storage)),
            (Some(archive_root), None) => (
                archive_root.clone(),
                Arc::new(StorageManager::new(archive_root)),
            ),
            (None, Some(storage)) => (storage.root().to_path_buf(), Arc::new(storage)),
            (None, None) => {
                let archive_root = default_state_dir();
                (
                    archive_root.clone(),
                    Arc::new(StorageManager::new(archive_root)),
                )
            }
        };
        let memory_store = Arc::new(MemoryStore::new(default_memory_dir_from_state_dir(
            &archive_root,
        )));
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
                                plan_mode: session.config.plan_mode,
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
            memory_store,
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
        memory_store: Arc<MemoryStore>,
    ) {
        let mut output = output.into_inner();
        while let Some(event) = output.recv().await {
            match &event {
                CoreOutput::CheckpointRequested { checkpoint } => {
                    let mut checkpoint = checkpoint.clone();
                    for session in &mut checkpoint.sessions {
                        match memory_store.extract_and_persist_for_session(session).await {
                            Ok(result) => {
                                let mut state = session.memory_state.clone().unwrap_or_default();
                                state.persistent_memory = result.state;
                                let diagnostics = state
                                    .memory_diagnostics
                                    .get_or_insert_with(quine_core::MemoryTurnDiagnostics::default);
                                diagnostics.persistent_memory.enabled = state
                                    .persistent_memory
                                    .as_ref()
                                    .map(|persistent| persistent.enabled)
                                    .unwrap_or(false);
                                diagnostics.persistent_memory.project_root =
                                    Some(session.config.working_directory.clone());
                                diagnostics.persistent_memory.extraction = result.diagnostics;
                                session.memory_state = Some(state);
                            }
                            Err(error) => {
                                let _ = event_tx.send(CoreOutput::SessionError {
                                    session_id: session.session_id,
                                    error: quine_core::CoreError::Internal {
                                        message: format!(
                                            "persistent memory extraction failed: {error}"
                                        ),
                                    },
                                });
                            }
                        }
                    }
                    if let Err(error) = storage.commit_checkpoint(&checkpoint).await {
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
                                plan_mode: false,
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

        self.sessions.lock().await.insert(
            session_id,
            SessionListing {
                state: quine_core::SessionState::Idle,
                created_at: Utc::now(),
                event_count: 0,
                plan_mode: config.plan_mode,
            },
        );

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
                    "plan_mode": session.plan_mode,
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
    use crate::storage::session_context_from_checkpoint;
    use quine_core::{CoreOutput, SessionState};
    use quine_llm::{LlmEvent, LlmProvider, Message, MessageContent, Role, ToolDefinition};
    use std::collections::HashMap;
    use std::fs;
    use std::pin::Pin;
    use std::sync::LazyLock;
    use tokio::fs as async_fs;
    use tokio::sync::Mutex;

    static PROMPT_MEMORY_ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

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

    struct ObservedMemoryProvider;

    #[async_trait::async_trait]
    impl LlmProvider for ObservedMemoryProvider {
        async fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let observed_memory = messages
                .iter()
                .filter(|message| message.role == Role::System)
                .filter_map(|message| match &message.content {
                    MessageContent::Text(text) => text
                        .strip_prefix("Relevant durable memory `")
                        .and_then(|text| text.split_once("`:\n"))
                        .map(|(entry_id, _)| entry_id.to_string()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join(",");
            let last_user = messages
                .iter()
                .rev()
                .find_map(|message| match (&message.role, &message.content) {
                    (Role::User, MessageContent::Text(text)) => Some(text.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let text = format!("observed-memory:{observed_memory}|last-user:{last_user}");
            let events = vec![
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    async fn wait_for_turn(
        rx: &mut tokio::sync::broadcast::Receiver<CoreOutput>,
        session_id: SessionId,
    ) -> String {
        let mut full_text = None;
        let mut saw_turn_complete = false;

        while !saw_turn_complete {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::TextComplete {
                    session_id: event_session_id,
                    full_text: text,
                })) if event_session_id == session_id => full_text = Some(text),
                Ok(Ok(CoreOutput::TurnComplete {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => saw_turn_complete = true,
                Ok(Ok(CoreOutput::ToolRequest {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    panic!("turn should not emit tool requests")
                }
                Ok(Ok(CoreOutput::InteractionNeeded {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    panic!("turn should not emit interaction requests")
                }
                Ok(Ok(CoreOutput::SessionError {
                    session_id: event_session_id,
                    error,
                })) if event_session_id == session_id => {
                    panic!("turn should not emit session errors: {error:?}")
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for turn completion"),
            }
        }

        full_text.expect("turn should emit text completion")
    }

    async fn write_prompt_memory_fixture(
        storage: &StorageManager,
        project_dir: &std::path::Path,
    ) -> std::path::PathBuf {
        let project_key = crate::memory_store::project_key(project_dir);
        let memory_dir = storage
            .root()
            .join("memory")
            .join("projects")
            .join(project_key);
        async_fs::create_dir_all(memory_dir.join("entries"))
            .await
            .unwrap();
        async_fs::write(
            memory_dir.join("MEMORY.md"),
            "# Durable Memory Index\n\n- rust-test-command\n- rust-build-command\n- editor-preference\n",
        )
        .await
        .unwrap();
        async_fs::write(
            memory_dir.join("index.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "entries": [
                    {
                        "entry_id": "rust-test-command",
                        "title": "Rust tests",
                        "summary": "Use cargo test for Rust test suite",
                        "slug": "rust-tests",
                        "path": "entries/rust-test-command.md",
                        "updated_at": "2026-04-04T00:00:00Z",
                        "keywords": ["cargo", "test", "rust"],
                        "pinned": false
                    },
                    {
                        "entry_id": "rust-build-command",
                        "title": "Workspace build",
                        "summary": "Use cargo build to compile workspace",
                        "slug": "workspace-build",
                        "path": "entries/rust-build-command.md",
                        "updated_at": "2026-04-04T00:00:01Z",
                        "keywords": ["cargo", "build", "workspace"],
                        "pinned": false
                    },
                    {
                        "entry_id": "editor-preference",
                        "title": "Editor preference",
                        "summary": "Prefer concise diffs in reviews",
                        "slug": "editor-preference",
                        "path": "entries/editor-preference.md",
                        "updated_at": "2026-04-04T00:00:02Z",
                        "keywords": ["editor", "review"],
                        "pinned": false
                    }
                ]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
        async_fs::write(
            memory_dir.join("entries/rust-test-command.md"),
            "---\nentry_id: rust-test-command\ntitle: Rust tests\nsummary: Use cargo test for Rust test suite\nkeywords:\n  - cargo\n  - test\n  - rust\ncreated_at: 2026-04-04T00:00:00Z\nupdated_at: 2026-04-04T00:00:00Z\nsource: explicit\nstatus: active\npinned: false\n---\n\nUse `cargo test` to run the Rust test suite.\n",
        )
        .await
        .unwrap();
        async_fs::write(
            memory_dir.join("entries/rust-build-command.md"),
            "---\nentry_id: rust-build-command\ntitle: Workspace build\nsummary: Use cargo build to compile workspace\nkeywords:\n  - cargo\n  - build\n  - workspace\ncreated_at: 2026-04-04T00:00:01Z\nupdated_at: 2026-04-04T00:00:01Z\nsource: explicit\nstatus: active\npinned: false\n---\n\nUse `cargo build` to compile workspace.\n",
        )
        .await
        .unwrap();
        async_fs::write(
            memory_dir.join("entries/editor-preference.md"),
            "---\nentry_id: editor-preference\ntitle: Editor preference\nsummary: Prefer concise diffs in reviews\nkeywords:\n  - editor\n  - review\ncreated_at: 2026-04-04T00:00:02Z\nupdated_at: 2026-04-04T00:00:02Z\nsource: explicit\nstatus: active\npinned: false\n---\n\nThe user prefers concise diffs in reviews.\n",
        )
        .await
        .unwrap();
        memory_dir
    }

    async fn wait_for_context_snapshot(
        harness: &LocalHarness,
        session_id: SessionId,
    ) -> crate::storage::SessionContextSnapshot {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(checkpoint) = harness.get_session_context(session_id).await {
                    if let Some(snapshot) =
                        session_context_from_checkpoint(&checkpoint, session_id, &HashMap::new())
                    {
                        break snapshot;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session context should become available")
    }

    async fn wait_for_context_snapshot_matching<F>(
        harness: &LocalHarness,
        session_id: SessionId,
        predicate: F,
    ) -> crate::storage::SessionContextSnapshot
    where
        F: Fn(&crate::storage::SessionContextSnapshot) -> bool,
    {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let snapshot = wait_for_context_snapshot(harness, session_id).await;
                if predicate(&snapshot) {
                    break snapshot;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session context should satisfy predicate")
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
    async fn persistent_memory_extracts_explicit_remember_and_forget_across_restart() {
        let storage = temp_storage();
        let project_dir =
            std::env::temp_dir().join(format!("quine-project-{}", uuid::Uuid::new_v4()));
        async_fs::create_dir_all(&project_dir).await.unwrap();
        async_fs::write(project_dir.join("CLAUDE.md"), "# test project\n")
            .await
            .unwrap();

        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(project_dir.clone()),
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        harness
            .send_message(
                session_id,
                "Remember this: use concise bullets in final responses".into(),
            )
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

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .memory_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.persistent_memory.extraction.created == 1)
                .unwrap_or(false)
        })
        .await;
        let extraction = &snapshot
            .memory_diagnostics
            .as_ref()
            .expect("memory diagnostics should be present")
            .persistent_memory
            .extraction;
        assert!(extraction.attempted);
        assert_eq!(extraction.status, quine_core::MemoryStatus::Succeeded);
        assert_eq!(extraction.created, 1);
        assert_eq!(extraction.tombstoned, 0);

        let memory_root = storage.root().join("memory");
        let project_key = crate::memory_store::project_key(&project_dir);
        let memory_dir = memory_root.join("projects").join(&project_key);
        let index_path = memory_dir.join("MEMORY.md");
        let entries_dir = memory_dir.join("entries");
        let index_before = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = async_fs::read_to_string(&index_path).await {
                    if contents.contains("use concise bullets in final responses") {
                        break contents;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(index_before.contains("use concise bullets in final responses"));
        let mut dir = async_fs::read_dir(&entries_dir).await.unwrap();
        let mut entry_count = 0usize;
        while (dir.next_entry().await.unwrap()).is_some() {
            entry_count += 1;
        }
        assert_eq!(entry_count, 1);

        harness.shutdown().await.unwrap();

        let restored = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        restored
            .send_message(
                session_id,
                "Forget this: use concise bullets in final responses".into(),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut rx = restored.subscribe();
            loop {
                if matches!(rx.recv().await.unwrap(), CoreOutput::TurnComplete { .. }) {
                    break;
                }
            }
        })
        .await
        .unwrap();

        let snapshot = wait_for_context_snapshot_matching(&restored, session_id, |snapshot| {
            snapshot
                .memory_diagnostics
                .as_ref()
                .map(|diagnostics| diagnostics.persistent_memory.extraction.tombstoned == 1)
                .unwrap_or(false)
        })
        .await;
        let extraction = &snapshot
            .memory_diagnostics
            .as_ref()
            .expect("memory diagnostics should be present")
            .persistent_memory
            .extraction;
        assert!(extraction.attempted);
        assert_eq!(extraction.status, quine_core::MemoryStatus::Succeeded);
        assert_eq!(extraction.tombstoned, 1);

        let index_after = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = async_fs::read_to_string(&index_path).await {
                    if !contents.contains("use concise bullets in final responses") {
                        break contents;
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!index_after.contains("use concise bullets in final responses"));
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if async_fs::read_dir(memory_dir.join("tombstones"))
                    .await
                    .unwrap()
                    .next_entry()
                    .await
                    .unwrap()
                    .is_some()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        restored.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(storage.root());
        let _ = fs::remove_dir_all(project_dir);
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
    async fn local_harness_context_reports_memory_diagnostics_after_turn() {
        let _env_guard = PROMPT_MEMORY_ENV_LOCK.lock().await;
        let previous_mode = std::env::var_os("QUINE_PROMPT_MEMORY_MODE");
        unsafe {
            std::env::set_var("QUINE_PROMPT_MEMORY_MODE", "disabled");
        }
        let storage = temp_storage();
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let mut rx = harness.subscribe();

        harness
            .send_message(session_id, "FEATURE-041 refresh diagnostics".into())
            .await
            .unwrap();
        let reply = wait_for_turn(&mut rx, session_id).await;
        assert_eq!(reply, "FEATURE-041 refresh diagnostics");

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .memory_diagnostics
                .as_ref()
                .map(|diagnostics| {
                    diagnostics.session_memory.refresh.status == quine_core::MemoryStatus::Succeeded
                        && diagnostics
                            .persistent_memory
                            .extraction
                            .last_extracted_message_index
                            .is_some()
                })
                .unwrap_or(false)
        })
        .await;

        let diagnostics = snapshot
            .memory_diagnostics
            .expect("memory diagnostics should be present");
        assert!(diagnostics.session_memory.enabled);
        assert!(diagnostics.session_memory.refresh.attempted);
        assert_eq!(
            diagnostics.session_memory.refresh.status,
            quine_core::MemoryStatus::Succeeded
        );
        assert!(diagnostics
            .session_memory
            .refresh
            .last_summarized_message_index
            .is_some());
        assert_eq!(
            diagnostics.prompt_memory.status,
            quine_core::MemoryStatus::Skipped
        );
        assert_eq!(
            diagnostics.prompt_memory.reason,
            Some(quine_core::MemoryDecisionReason::Disabled)
        );
        assert!(diagnostics.persistent_memory.enabled);
        assert_eq!(
            diagnostics.persistent_memory.extraction.reason,
            Some(quine_core::MemoryDecisionReason::NoChanges)
        );

        harness.shutdown().await.unwrap();
        match previous_mode {
            Some(value) => unsafe { std::env::set_var("QUINE_PROMPT_MEMORY_MODE", value) },
            None => unsafe { std::env::remove_var("QUINE_PROMPT_MEMORY_MODE") },
        }
        let _ = fs::remove_dir_all(storage.root());
    }

    #[tokio::test]
    async fn local_harness_persists_plan_mode_in_session_listing() {
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
                            && session.get("plan_mode").and_then(|value| value.as_bool())
                                == Some(true)
                    })
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("session should appear in local session listing");

        harness.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(storage.root());
    }

    #[tokio::test]
    async fn prompt_time_persistent_recall_multi_round_local_daemon() {
        let _env_guard = PROMPT_MEMORY_ENV_LOCK.lock().await;
        let previous_mode = std::env::var_os("QUINE_PROMPT_MEMORY_MODE");
        unsafe {
            std::env::set_var("QUINE_PROMPT_MEMORY_MODE", "targeted_recall");
        }

        let storage = temp_storage();
        let project_dir =
            std::env::temp_dir().join(format!("quine-project-{}", uuid::Uuid::new_v4()));
        async_fs::create_dir_all(&project_dir).await.unwrap();
        async_fs::write(project_dir.join("CLAUDE.md"), "# test project\n")
            .await
            .unwrap();
        let _memory_dir = write_prompt_memory_fixture(&storage, &project_dir).await;

        let harness = LocalHarness::new(Arc::new(ObservedMemoryProvider), Some(storage.clone()))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(project_dir.clone()),
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        let mut rx = harness.subscribe();

        harness
            .send_message(session_id, "Say exactly: round-1 acknowledged.".into())
            .await
            .unwrap();
        let round_1 = wait_for_turn(&mut rx, session_id).await;
        assert_eq!(
            round_1,
            "observed-memory:|last-user:Say exactly: round-1 acknowledged."
        );

        let snapshot = wait_for_context_snapshot(&harness, session_id).await;
        let prompt_memory = snapshot
            .prompt_memory
            .expect("prompt memory summary should persist");
        assert_eq!(
            prompt_memory.mode,
            quine_core::PromptMemoryMode::TargetedRecall
        );
        assert!(prompt_memory.selected_entry_ids.is_empty());
        assert!(snapshot.history.iter().all(|entry| match entry {
            crate::storage::HistoryEntry::Text { text, .. } => {
                !text.contains("Relevant durable memory `")
            }
            crate::storage::HistoryEntry::ToolUse { text, .. } => text
                .as_deref()
                .map(|text| !text.contains("Relevant durable memory `"))
                .unwrap_or(true),
            crate::storage::HistoryEntry::ToolResult { output, .. } => {
                !output.contains("Relevant durable memory `")
            }
        }));

        harness
            .send_message(
                session_id,
                "What command should I run to execute the Rust test suite? Answer with only the command.".into(),
            )
            .await
            .unwrap();
        let round_2 = wait_for_turn(&mut rx, session_id).await;
        assert_eq!(
            round_2,
            "observed-memory:rust-test-command|last-user:What command should I run to execute the Rust test suite? Answer with only the command."
        );

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .prompt_memory
                .as_ref()
                .map(|summary| summary.selected_entry_ids.clone())
                == Some(vec!["rust-test-command".to_string()])
        })
        .await;
        let prompt_memory = snapshot
            .prompt_memory
            .expect("prompt memory summary should persist");
        assert_eq!(
            prompt_memory.selected_entry_ids,
            vec!["rust-test-command".to_string()]
        );
        assert!(snapshot.history.iter().all(|entry| match entry {
            crate::storage::HistoryEntry::Text { text, .. } => {
                !text.contains("Relevant durable memory `")
            }
            crate::storage::HistoryEntry::ToolUse { text, .. } => text
                .as_deref()
                .map(|text| !text.contains("Relevant durable memory `"))
                .unwrap_or(true),
            crate::storage::HistoryEntry::ToolResult { output, .. } => {
                !output.contains("Relevant durable memory `")
            }
        }));

        harness
            .send_message(
                session_id,
                "What command should I run to build the workspace? Answer with only the command."
                    .into(),
            )
            .await
            .unwrap();
        let round_3 = wait_for_turn(&mut rx, session_id).await;
        assert_eq!(
            round_3,
            "observed-memory:rust-build-command|last-user:What command should I run to build the workspace? Answer with only the command."
        );

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .prompt_memory
                .as_ref()
                .map(|summary| summary.selected_entry_ids.clone())
                == Some(vec!["rust-build-command".to_string()])
        })
        .await;
        let prompt_memory = snapshot
            .prompt_memory
            .expect("prompt memory summary should persist");
        assert_eq!(
            prompt_memory.selected_entry_ids,
            vec!["rust-build-command".to_string()]
        );
        assert!(snapshot.history.iter().all(|entry| match entry {
            crate::storage::HistoryEntry::Text { text, .. } => {
                !text.contains("Relevant durable memory `")
            }
            crate::storage::HistoryEntry::ToolUse { text, .. } => text
                .as_deref()
                .map(|text| !text.contains("Relevant durable memory `"))
                .unwrap_or(true),
            crate::storage::HistoryEntry::ToolResult { output, .. } => {
                !output.contains("Relevant durable memory `")
            }
        }));

        harness.shutdown().await.unwrap();
        match previous_mode {
            Some(value) => unsafe { std::env::set_var("QUINE_PROMPT_MEMORY_MODE", value) },
            None => unsafe { std::env::remove_var("QUINE_PROMPT_MEMORY_MODE") },
        }
        let _ = fs::remove_dir_all(storage.root());
        let _ = fs::remove_dir_all(project_dir);
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
