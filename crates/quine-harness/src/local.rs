use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;

use quine_core::{
    create_channels, load_session_skills, ChannelConfig, CoreCheckpoint, CoreInput, CoreOutput,
    HarnessHandle, InheritanceFlags, InteractionResponse, PythonExecRequest, PythonRuntime,
    SessionId, SessionSignal, Skill,
};
use quine_llm::{LlmProvider, NoopWebProvider, WebProvider};
use tokio::sync::{broadcast, mpsc, oneshot, Mutex};
use tokio::time::Duration;

use crate::config::{
    auto_compact_threshold_percent_from_env, default_memory_dir_from_state_dir, default_state_dir,
    load_persisted_permission_rules, max_context_window_from_env, resolve_session_llm_config,
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
    provider_manager: ProviderManager,
    python_runtime: Arc<PythonRuntime>,
}

#[derive(Debug, Clone)]
struct SessionListing {
    state: quine_core::SessionState,
    created_at: chrono::DateTime<Utc>,
    event_count: usize,
    title: Option<String>,
    summary: Option<String>,
    plan_mode: bool,
    parent_id: Option<SessionId>,
    model_profile: Option<String>,
    session_group: String,
}

#[derive(Clone)]
struct ProviderManager {
    default_provider: Arc<dyn LlmProvider>,
    default_max_context_window: Option<u64>,
}

impl ProviderManager {
    fn new(
        default_provider: Arc<dyn LlmProvider>,
        default_max_context_window: Option<u64>,
    ) -> Self {
        Self {
            default_provider,
            default_max_context_window,
        }
    }

    fn resolve(
        &self,
        model_profile: Option<&str>,
    ) -> Result<quine_core::SessionLlmConfig, HarnessError> {
        match model_profile {
            Some(profile) => resolve_session_llm_config(Some(profile)).map_err(|error| {
                HarnessError::SessionCreationFailed {
                    reason: error.to_string(),
                }
            }),
            None => Ok(quine_core::SessionLlmConfig {
                provider: Arc::clone(&self.default_provider),
                max_context_window: self.default_max_context_window,
                model_profile: None,
            }),
        }
    }

    fn resolve_for_restore(&self, model_profile: Option<&str>) -> quine_core::SessionLlmConfig {
        match model_profile {
            Some(profile) => match self.resolve(Some(profile)) {
                Ok(config) => config,
                Err(error) => {
                    eprintln!(
                        "[daemon] restored session model profile `{profile}` is unavailable; falling back to default provider: {error}"
                    );
                    quine_core::SessionLlmConfig {
                        provider: Arc::clone(&self.default_provider),
                        max_context_window: self.default_max_context_window,
                        model_profile: None,
                    }
                }
            },
            None => quine_core::SessionLlmConfig {
                provider: Arc::clone(&self.default_provider),
                max_context_window: self.default_max_context_window,
                model_profile: None,
            },
        }
    }
}

fn persisted_listing_summary(session: &quine_core::PersistedSession) -> Option<String> {
    session
        .memory_state
        .as_ref()
        .and_then(|state| state.session_memory.as_ref())
        .and_then(|state| state.listing_summary.clone())
}

async fn refresh_checkpoint_session_summaries(
    sessions: &Arc<Mutex<HashMap<SessionId, SessionListing>>>,
    checkpoint: &quine_core::persistence::CoreCheckpoint,
    state_root: &std::path::Path,
) -> Result<()> {
    let live_states = HashMap::new();
    let mut guard = sessions.lock().await;
    for persisted_session in &checkpoint.sessions {
        let session_id = persisted_session.session_id;
        let _ =
            session_context_from_checkpoint(checkpoint, session_id, &live_states, Some(state_root));
        if let Some(session) = guard.get_mut(&session_id) {
            session.summary = persisted_listing_summary(persisted_session);
        }
    }

    Ok(())
}

fn serialize_session_id(session_id: SessionId) -> String {
    serde_json::to_value(session_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn effective_group_for_listing(session_id: SessionId, session_group: Option<&str>) -> String {
    session_group
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| serialize_session_id(session_id))
}

fn session_lineage(
    sessions: &HashMap<SessionId, SessionListing>,
) -> HashMap<String, (String, usize)> {
    sessions
        .keys()
        .copied()
        .map(|session_id| {
            let mut depth = 0usize;
            let mut root_id = session_id;
            while let Some(parent_id) = sessions.get(&root_id).and_then(|session| session.parent_id)
            {
                root_id = parent_id;
                depth += 1;
            }
            (
                serialize_session_id(session_id),
                (serialize_session_id(root_id), depth),
            )
        })
        .collect()
}

impl LocalHarness {
    async fn request_checkpoint(&self) -> Result<(), HarnessError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::RequestCheckpoint { reply: reply_tx })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        Ok(())
    }

    async fn resolve_python_group(
        &self,
        session_id: Option<SessionId>,
        session_group: Option<String>,
    ) -> Result<String, HarnessError> {
        match (session_id, session_group) {
            (Some(_), Some(_)) => Err(HarnessError::Internal {
                message: "provide either session_id or session_group, not both".into(),
            }),
            (None, None) => Err(HarnessError::Internal {
                message: "missing session_id or session_group".into(),
            }),
            (None, Some(group)) => Ok(group),
            (Some(session_id), None) => self
                .sessions
                .lock()
                .await
                .get(&session_id)
                .map(|session| session.session_group.clone())
                .ok_or_else(|| HarnessError::SessionNotFound {
                    session_id: serialize_session_id(session_id),
                }),
        }
    }

    /// Create a new `LocalHarness` that spawns the core event loop with the
    /// given LLM provider.
    pub async fn new(
        provider: Arc<dyn LlmProvider>,
        storage: Option<StorageManager>,
    ) -> Result<Self, HarnessError> {
        Self::new_with_web_provider(provider, Arc::new(NoopWebProvider), storage).await
    }

    pub async fn new_with_web_provider(
        provider: Arc<dyn LlmProvider>,
        web_provider: Arc<dyn WebProvider>,
        storage: Option<StorageManager>,
    ) -> Result<Self, HarnessError> {
        Self::with_storage(provider, web_provider, storage, None).await
    }

    pub async fn with_archive_root(
        provider: Arc<dyn LlmProvider>,
        archive_root: Option<std::path::PathBuf>,
    ) -> Result<Self, HarnessError> {
        Self::with_archive_root_and_web_provider(provider, Arc::new(NoopWebProvider), archive_root)
            .await
    }

    pub async fn with_archive_root_and_web_provider(
        provider: Arc<dyn LlmProvider>,
        web_provider: Arc<dyn WebProvider>,
        archive_root: Option<std::path::PathBuf>,
    ) -> Result<Self, HarnessError> {
        Self::with_storage(provider, web_provider, None, archive_root).await
    }

    async fn with_storage(
        provider: Arc<dyn LlmProvider>,
        web_provider: Arc<dyn WebProvider>,
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
                                title: None,
                                summary: persisted_listing_summary(session),
                                plan_mode: session.config.plan_mode,
                                parent_id: checkpoint
                                    .session_tree
                                    .parents
                                    .get(&session.session_id)
                                    .copied(),
                                model_profile: session.config.model_profile.clone(),
                                session_group: effective_group_for_listing(
                                    session.session_id,
                                    session.config.session_group.as_deref(),
                                ),
                            },
                        )
                    })
                    .collect::<HashMap<SessionId, SessionListing>>()
            })
            .unwrap_or_default();
        let sessions = Arc::new(Mutex::new(initial_sessions));

        let max_context_window = max_context_window_from_env();
        let provider_manager = ProviderManager::new(Arc::clone(&provider), max_context_window);
        let python_runtime = PythonRuntime::new();
        let restored_profile_updates = restored_checkpoint
            .as_ref()
            .map(|checkpoint| {
                checkpoint
                    .sessions
                    .iter()
                    .filter_map(|session| {
                        session
                            .config
                            .model_profile
                            .as_ref()
                            .map(|profile| (session.session_id, profile.clone()))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        // Spawn the core event loop.
        let core_task = tokio::spawn(
            quine_core::run_core_loop_with_compaction_and_web_provider_and_python_runtime(
                core_handle,
                Arc::clone(&provider),
                web_provider,
                restored_checkpoint,
                archive_root,
                max_context_window,
                Arc::clone(&python_runtime),
            ),
        );

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

        let harness = Self {
            core_input: input,
            event_tx,
            _core_task: core_task,
            _fanout_task: fanout_task,
            _storage: storage,
            sessions,
            provider_manager,
            python_runtime,
        };

        for (session_id, profile) in restored_profile_updates {
            let session_llm = harness.provider_manager.resolve_for_restore(Some(&profile));
            let restored_model_profile = session_llm.model_profile.clone();
            let (reply_tx, reply_rx) = oneshot::channel();
            harness
                .core_input
                .send(CoreInput::UpdateSessionLlm {
                    session_id,
                    session_llm,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| HarnessError::CoreChannelClosed)?;
            let update_result = reply_rx
                .await
                .map_err(|_| HarnessError::CoreChannelClosed)?;
            if let Err(reason) = update_result {
                if reason == "unknown session" {
                    harness.sessions.lock().await.remove(&session_id);
                    eprintln!(
                        "[daemon] skipping model-profile restore for missing session {session_id}"
                    );
                    continue;
                }
                return Err(HarnessError::SessionCreationFailed { reason });
            }
            let mut sessions = harness.sessions.lock().await;
            if let Some(session) = sessions.get_mut(&session_id) {
                session.model_profile = restored_model_profile;
            }
        }

        Ok(harness)
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
                                diagnostics.persistent_memory.readable_scopes = state
                                    .persistent_memory
                                    .as_ref()
                                    .and_then(|persistent| persistent.scope_state.as_ref())
                                    .map(|scope_state| scope_state.readable_scopes.clone())
                                    .unwrap_or_default();
                                diagnostics.persistent_memory.writable_scope =
                                    result.writable_scope.clone();
                                diagnostics.persistent_memory.conflict_resolution = state
                                    .persistent_memory
                                    .as_ref()
                                    .and_then(|persistent| persistent.scope_state.as_ref())
                                    .map(|scope_state| scope_state.conflict_resolution);
                                diagnostics.persistent_memory.write_status = result.write_status;
                                diagnostics.persistent_memory.write_reason = result.write_reason;
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
                    } else if let Err(error) =
                        refresh_checkpoint_session_summaries(&sessions, &checkpoint, storage.root())
                            .await
                    {
                        tracing::warn!(?error, "failed to refresh session summaries");
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
                                title: None,
                                summary: None,
                                plan_mode: false,
                                parent_id: None,
                                model_profile: None,
                                session_group: serialize_session_id(*session_id),
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

/// Resolve auto-attached project skills plus any explicit requested skills.
async fn load_skills_from_config(
    working_directory: &std::path::Path,
    skill_names: &[String],
) -> Vec<Skill> {
    load_session_skills(working_directory, skill_names).await
}

impl LocalHarness {
    #[cfg(test)]
    pub(crate) async fn latest_checkpoint_for_tests(&self) -> Result<CoreCheckpoint, HarnessError> {
        self._storage
            .load_latest_checkpoint()
            .await
            .map_err(|error| HarnessError::Internal {
                message: format!("failed to load checkpoint: {error}"),
            })?
            .ok_or_else(|| HarnessError::Internal {
                message: "no checkpoint available".into(),
            })
    }
}

#[async_trait]
impl HarnessService for LocalHarness {
    async fn create_session(&self, config: SessionConfig) -> Result<SessionId, HarnessError> {
        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        let working_directory = config
            .working_directory
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let skills = load_skills_from_config(&working_directory, &config.skills).await;
        let permission_rules =
            load_persisted_permission_rules(&working_directory).map_err(|error| {
                HarnessError::SessionCreationFailed {
                    reason: error.to_string(),
                }
            })?;
        let session_llm = self
            .provider_manager
            .resolve(config.model_profile.as_deref())?;
        let auto_compact_threshold_percent = if config.auto_compact_threshold_percent == 0 {
            auto_compact_threshold_percent_from_env()
        } else {
            config.auto_compact_threshold_percent.clamp(1, 100)
        };
        let status_report_min_tool_rounds = if config.status_report_min_tool_rounds == 0 {
            quine_core::default_status_report_min_tool_rounds()
        } else {
            config.status_report_min_tool_rounds
        };

        self.core_input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: config.system_prompt,
                working_directory: Some(working_directory),
                skills,
                plan_mode: config.plan_mode,
                prompt_behavior: config.prompt_behavior,
                permission_rules,
                initial_messages: config.initial_messages,
                agent_key: config.agent_key,
                team_key: config.team_key,
                session_group: config.session_group.clone(),
                memory_policy: config.memory_policy,
                session_llm: session_llm.clone(),
                auto_compact_threshold_percent,
                status_report_min_tool_rounds,
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
                title: None,
                summary: None,
                plan_mode: config.plan_mode,
                parent_id: None,
                model_profile: session_llm.model_profile,
                session_group: effective_group_for_listing(
                    session_id,
                    config.session_group.as_deref(),
                ),
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

    async fn set_session_model_profile(
        &self,
        session_id: SessionId,
        model_profile: String,
    ) -> Result<(), HarnessError> {
        let session_llm = self.provider_manager.resolve(Some(&model_profile))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.core_input
            .send(CoreInput::UpdateSessionLlm {
                session_id,
                session_llm,
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|message| HarnessError::Internal { message })?;

        if let Some(session) = self.sessions.lock().await.get_mut(&session_id) {
            session.model_profile = Some(model_profile);
        }
        Ok(())
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
        let lineage = session_lineage(&sessions);
        let mut items: Vec<serde_json::Value> = sessions
            .iter()
            .map(|(session_id, session)| {
                let session_id = serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{session_id:?}"));
                let (root_id, depth) = lineage
                    .get(session_id.as_str())
                    .cloned()
                    .unwrap_or_else(|| (session_id.clone(), 0));
                serde_json::json!({
                    "session_id": session_id,
                    "status": format!("{:?}", session.state).to_lowercase(),
                    "first_event": session.created_at.to_rfc3339(),
                    "event_count": session.event_count,
                    "title": session.title,
                    "summary": session.summary,
                    "plan_mode": session.plan_mode,
                    "parent_id": session.parent_id.map(serialize_session_id),
                    "model_profile": session.model_profile,
                    "session_group": session.session_group,
                    "root_id": root_id,
                    "depth": depth,
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

        self.request_checkpoint().await?;

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

        if session_context_from_checkpoint(
            &checkpoint,
            session_id,
            &live_states,
            Some(self._storage.root()),
        )
        .is_none()
        {
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
                prompt_behavior: quine_core::PermissionPromptBehavior::Interactive,
                permission_rules: quine_core::PermissionRuleSet::default(),
                inheritance: InheritanceFlags::default(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?;

        reply_rx
            .await
            .map_err(|_| HarnessError::CoreChannelClosed)?
            .map_err(|reason| HarnessError::SessionCreationFailed { reason })?;

        let inherited_model_profile = if let Some(id) = parent_id {
            self.sessions
                .lock()
                .await
                .get(&id)
                .map(|session| (session.model_profile.clone(), session.session_group.clone()))
        } else {
            None
        };

        self.sessions.lock().await.insert(
            child_id,
            SessionListing {
                state: quine_core::SessionState::Idle,
                created_at: Utc::now(),
                event_count: 0,
                title: None,
                summary: None,
                plan_mode: false,
                parent_id,
                model_profile: inherited_model_profile
                    .as_ref()
                    .and_then(|(model_profile, _)| model_profile.clone()),
                session_group: inherited_model_profile
                    .map(|(_, session_group)| session_group)
                    .unwrap_or_else(|| serialize_session_id(child_id)),
            },
        );

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

    async fn python_exec(
        &self,
        session_id: Option<SessionId>,
        session_group: Option<String>,
        request: PythonExecRequest,
    ) -> Result<quine_core::PythonExecResult, HarnessError> {
        let group = self.resolve_python_group(session_id, session_group).await?;
        let result = self
            .python_runtime
            .exec(&group, &request)
            .await
            .map_err(|error| HarnessError::Internal {
                message: error.to_string(),
            })?;
        let should_checkpoint = self
            .sessions
            .lock()
            .await
            .values()
            .any(|session| session.session_group == group);
        if should_checkpoint {
            self.request_checkpoint().await?;
        }
        Ok(result)
    }

    async fn python_list_globals(
        &self,
        session_id: Option<SessionId>,
        session_group: Option<String>,
    ) -> Result<quine_core::PythonListGlobalsResult, HarnessError> {
        let group = self.resolve_python_group(session_id, session_group).await?;
        self.python_runtime
            .list_globals(&group)
            .await
            .map_err(|error| HarnessError::Internal {
                message: error.to_string(),
            })
    }

    async fn python_inspect_global(
        &self,
        session_id: Option<SessionId>,
        session_group: Option<String>,
        name: String,
    ) -> Result<quine_core::PythonInspectResult, HarnessError> {
        let group = self.resolve_python_group(session_id, session_group).await?;
        self.python_runtime
            .inspect(&group, &name)
            .await
            .map_err(|error| HarnessError::Internal {
                message: error.to_string(),
            })
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

    fn state_root(&self) -> Option<std::path::PathBuf> {
        Some(self._storage.root().to_path_buf())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::session_context_from_checkpoint;
    use quine_core::{CoreOutput, PermissionPromptBehavior, SessionState};
    use quine_llm::{LlmEvent, LlmProvider, Message, MessageContent, Role, ToolDefinition};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::LazyLock;
    use tokio::fs as async_fs;
    use tokio::sync::{Mutex, Notify};

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

    struct ConcurrentSessionProvider {
        response_text: String,
        started: AtomicUsize,
        started_notify: Notify,
        released: AtomicBool,
        release_notify: Notify,
    }

    impl ConcurrentSessionProvider {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
                started: AtomicUsize::new(0),
                started_notify: Notify::new(),
                released: AtomicBool::new(false),
                release_notify: Notify::new(),
            }
        }

        async fn wait_until_started(&self, expected: usize) {
            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if self.started.load(Ordering::SeqCst) >= expected {
                        break;
                    }
                    self.started_notify.notified().await;
                }
            })
            .await
            .expect("sessions never entered the provider concurrently");
        }

        fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
            self.release_notify.notify_waiters();
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ConcurrentSessionProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.started_notify.notify_waiters();

            tokio::time::timeout(std::time::Duration::from_secs(1), async {
                loop {
                    if self.started.load(Ordering::SeqCst) >= 2 {
                        break;
                    }
                    self.started_notify.notified().await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("second session never reached the provider"))?;

            loop {
                if self.released.load(Ordering::SeqCst) {
                    break;
                }
                self.release_notify.notified().await;
            }

            let text = self.response_text.clone();
            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ])))
        }
    }

    #[tokio::test]
    async fn create_session_bootstraps_permission_context_without_explicit_inputs() {
        let harness = LocalHarness::new(Arc::new(MockProvider), Some(temp_storage()))
            .await
            .unwrap();

        let session_id = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        let snapshot = wait_for_context_snapshot(&harness, session_id).await;
        let checkpoint = harness.latest_checkpoint_for_tests().await.unwrap();
        let projected =
            session_context_from_checkpoint(&checkpoint, session_id, &HashMap::new(), None)
                .expect("session snapshot should exist in checkpoint");

        assert_eq!(snapshot.session_id, projected.session_id);
        assert_eq!(snapshot.working_directory, projected.working_directory);
        assert!(!snapshot.plan_mode);
        assert!(!projected.plan_mode);

        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn create_session_auto_attaches_project_claude_command_skills() {
        let storage = temp_storage();
        let project_dir =
            std::env::temp_dir().join(format!("quine-project-{}", uuid::Uuid::new_v4()));
        let commands_dir = project_dir.join(".claude").join("commands");
        async_fs::create_dir_all(&commands_dir).await.unwrap();
        async_fs::write(
            commands_dir.join("auto-attached.md"),
            "Always mention that this skill was auto-attached.\n",
        )
        .await
        .unwrap();
        async_fs::write(commands_dir.join("helper.py"), "print('not a skill')\n")
            .await
            .unwrap();

        let harness = LocalHarness::new(Arc::new(MockProvider), Some(storage))
            .await
            .unwrap();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(project_dir.clone()),
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .loaded_skills
                .iter()
                .any(|skill| skill.name == "auto-attached")
        })
        .await;

        assert_eq!(snapshot.skills, vec!["auto-attached"]);
        assert_eq!(snapshot.loaded_skills.len(), 1);
        assert_eq!(snapshot.loaded_skills[0].name, "auto-attached");
        assert_eq!(
            snapshot.loaded_skills[0].source_path,
            commands_dir.join("auto-attached.md")
        );

        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn create_session_keeps_project_auto_skills_when_explicit_skill_names_overlap() {
        let storage = temp_storage();
        let project_dir =
            std::env::temp_dir().join(format!("quine-project-{}", uuid::Uuid::new_v4()));
        let commands_dir = project_dir.join(".claude").join("commands");
        async_fs::create_dir_all(&commands_dir).await.unwrap();
        async_fs::write(
            commands_dir.join("project-only.md"),
            "Project-specific auto-attached instructions.\n",
        )
        .await
        .unwrap();
        async_fs::write(
            commands_dir.join("second-auto.md"),
            "A second project skill that should stay attached.\n",
        )
        .await
        .unwrap();

        let harness = LocalHarness::new(Arc::new(MockProvider), Some(storage))
            .await
            .unwrap();
        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(project_dir),
                skills: vec!["project-only".into()],
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot.loaded_skills.len() == 2
        })
        .await;

        assert_eq!(snapshot.skills, vec!["project-only", "second-auto"]);
        let loaded_names = snapshot
            .loaded_skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(loaded_names, vec!["project-only", "second-auto"]);

        harness.shutdown().await.unwrap();
    }

    #[test]
    fn persisted_listing_summary_reads_session_memory_field() {
        let session = quine_core::PersistedSession {
            session_id: SessionId::new(),
            created_at: Utc::now(),
            state: quine_core::PersistedSessionState::Idle,
            config: quine_core::PersistedSessionConfig {
                system_prompt: None,
                skill_names: Vec::new(),
                working_directory: PathBuf::from("."),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                prompt_memory_mode: quine_core::PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: quine_core::MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: 60,
                status_report_min_tool_rounds: quine_core::default_status_report_min_tool_rounds(),
            },
            history: Vec::new(),
            plan_store: quine_core::PersistedPlanStore::default(),
            memory_state: Some(quine_core::PersistedMemoryState {
                session_memory: Some(quine_core::PersistedSessionMemoryState {
                    enabled: true,
                    last_summarized_message_index: Some(3),
                    template_version: 1,
                    listing_summary: Some(
                        "Tracks a model-generated session summary for listings.".into(),
                    ),
                }),
                persistent_memory: None,
                prompt_memory: None,
                memory_diagnostics: None,
            }),
            permission_state: None,
            status_report: None,
            python_state: None,
        };

        assert_eq!(
            persisted_listing_summary(&session).as_deref(),
            Some("Tracks a model-generated session summary for listings.")
        );
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

    #[tokio::test]
    async fn local_harness_handles_two_sessions_concurrently() {
        let provider = Arc::new(ConcurrentSessionProvider::new("parallel reply"));
        let harness = LocalHarness::new(provider.clone(), Some(temp_storage()))
            .await
            .unwrap();
        let mut rx = harness.subscribe();

        let session_a = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();
        let session_b = harness
            .create_session(SessionConfig::default())
            .await
            .unwrap();

        harness
            .send_message(session_a, "first".into())
            .await
            .unwrap();
        harness
            .send_message(session_b, "second".into())
            .await
            .unwrap();

        provider.wait_until_started(2).await;
        provider.release();

        let mut completed = Vec::new();
        while completed.len() < 2 {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::TurnComplete { session_id, .. })) => {
                    if !completed.contains(&session_id) {
                        completed.push(session_id);
                    }
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for concurrent session completions"),
            }
        }

        assert!(completed.contains(&session_a));
        assert!(completed.contains(&session_b));

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

    struct ApprovalProvider {
        call_count: std::sync::atomic::AtomicU32,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ApprovalProvider {
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
                        tool_use_id: "toolu_apply_patch".into(),
                        tool_name: "apply_patch".into(),
                        arguments: serde_json::json!({
                            "file_path": "approval-check.txt",
                            "new_file_content": "approved-by-operator"
                        }),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            } else {
                vec![
                    Ok(LlmEvent::TextDelta {
                        text: "approval complete".into(),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn local_harness_routes_permission_approval_and_resumes_turn() {
        let storage = temp_storage();
        let workspace = storage.root().to_path_buf();
        let harness = LocalHarness::new(
            Arc::new(ApprovalProvider {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }),
            Some(storage),
        )
        .await
        .unwrap();
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(workspace.clone()),
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        harness
            .send_message(session_id, "create the approval test file".into())
            .await
            .unwrap();

        let mut saw_interaction = false;
        let mut saw_text_complete = false;
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::InteractionNeeded {
                    session_id: event_session_id,
                    request,
                })) if event_session_id == session_id => {
                    saw_interaction = true;
                    assert!(request
                        .source_label
                        .as_deref()
                        .is_some_and(|label| label.starts_with("permission:")));
                    harness
                        .submit_interaction_response(
                            session_id,
                            InteractionResponse {
                                response: "approve once".into(),
                                selected_indices: vec![0],
                            },
                        )
                        .await
                        .unwrap();
                }
                Ok(Ok(CoreOutput::TextComplete {
                    session_id: event_session_id,
                    full_text,
                })) if event_session_id == session_id => {
                    assert_eq!(full_text, "approval complete");
                    saw_text_complete = true;
                }
                Ok(Ok(CoreOutput::TurnComplete {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for approval flow"),
            }
        }

        assert!(saw_interaction);
        assert!(saw_text_complete);
        assert_eq!(
            async_fs::read_to_string(workspace.join("approval-check.txt"))
                .await
                .unwrap(),
            "approved-by-operator"
        );

        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn get_session_context_includes_permission_diagnostics() {
        let storage = temp_storage();
        let workspace = storage.root().to_path_buf();
        let harness = LocalHarness::new(Arc::new(MockProvider), Some(storage))
            .await
            .unwrap();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(workspace.clone()),
                prompt_behavior: PermissionPromptBehavior::Headless,
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        let snapshot = wait_for_context_snapshot(&harness, session_id).await;
        let diagnostics = snapshot
            .permission_diagnostics
            .expect("permission diagnostics should be present");
        assert_eq!(diagnostics.mode, quine_core::PermissionMode::Default);
        assert_eq!(
            diagnostics.prompt_behavior,
            PermissionPromptBehavior::Headless
        );
        assert_eq!(diagnostics.workspace_root, workspace);
        assert!(diagnostics.additional_allowed_roots.is_empty());
        assert!(diagnostics.rules.session.is_empty());
        assert!(diagnostics.last_decision.is_none());
        assert!(diagnostics.pending_approval.is_none());

        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn get_session_context_surfaces_pending_permission_approval_and_last_decision() {
        let storage = temp_storage();
        let workspace = storage.root().to_path_buf();
        let harness = LocalHarness::new(
            Arc::new(ApprovalProvider {
                call_count: std::sync::atomic::AtomicU32::new(0),
            }),
            Some(storage),
        )
        .await
        .unwrap();
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(workspace),
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        harness
            .send_message(session_id, "create the approval test file".into())
            .await
            .unwrap();

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::InteractionNeeded {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    break;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for permission interaction"),
            }
        }

        let pending_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(checkpoint) = harness.get_session_context(session_id).await {
                    if let Some(snapshot) = session_context_from_checkpoint(
                        &checkpoint,
                        session_id,
                        &HashMap::new(),
                        None,
                    ) {
                        if snapshot
                            .permission_diagnostics
                            .as_ref()
                            .and_then(|diagnostics| diagnostics.pending_approval.as_ref())
                            .is_some()
                        {
                            break snapshot;
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pending approval should become visible in session context");
        let pending = pending_snapshot
            .permission_diagnostics
            .and_then(|diagnostics| diagnostics.pending_approval)
            .expect("pending approval should be visible");
        assert_eq!(pending.outcome.request.tool_name, "apply_patch");
        assert!(pending
            .outcome
            .reason
            .contains("permission resolved by Default mode"));

        harness
            .submit_interaction_response(
                session_id,
                InteractionResponse {
                    response: "deny once".into(),
                    selected_indices: vec![1],
                },
            )
            .await
            .unwrap();

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::TurnComplete {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => break,
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for denied approval turn completion"),
            }
        }

        let denied_snapshot = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(checkpoint) = harness.get_session_context(session_id).await {
                    if let Some(snapshot) = session_context_from_checkpoint(
                        &checkpoint,
                        session_id,
                        &HashMap::new(),
                        None,
                    ) {
                        if snapshot
                            .permission_diagnostics
                            .as_ref()
                            .is_some_and(|diagnostics| {
                                diagnostics.last_decision.is_some()
                                    && diagnostics.pending_approval.is_none()
                            })
                        {
                            break snapshot;
                        }
                    }
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("last permission decision should become visible in session context");
        let diagnostics = denied_snapshot
            .permission_diagnostics
            .expect("permission diagnostics should be present");
        assert!(diagnostics.pending_approval.is_none());
        let last_decision = diagnostics
            .last_decision
            .expect("last permission decision should be recorded");
        assert_eq!(last_decision.request.tool_name, "apply_patch");
        assert_eq!(
            last_decision.source.kind,
            quine_core::PermissionMatchKind::ModeDefault
        );
        assert!(last_decision
            .reason
            .contains("permission resolved by Default mode"));

        harness.shutdown().await.unwrap();
    }

    struct BackgroundApprovalProvider {
        call_count: std::sync::atomic::AtomicU32,
        marker_name: String,
    }

    #[async_trait::async_trait]
    impl LlmProvider for BackgroundApprovalProvider {
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
                        tool_use_id: "toolu_background_bash".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({
                            "command": format!("touch {}", self.marker_name),
                        }),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            } else {
                vec![
                    Ok(LlmEvent::TextDelta {
                        text: "background complete".into(),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            };
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
                    if let Some(snapshot) = session_context_from_checkpoint(
                        &checkpoint,
                        session_id,
                        &HashMap::new(),
                        None,
                    ) {
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
    async fn local_harness_restore_falls_back_for_unknown_model_profile() {
        let storage = temp_storage();
        storage
            .commit_checkpoint(&CoreCheckpoint::new(
                vec![quine_core::PersistedSession {
                    session_id: SessionId::new(),
                    created_at: Utc::now(),
                    state: quine_core::PersistedSessionState::Idle,
                    config: quine_core::PersistedSessionConfig {
                        system_prompt: None,
                        skill_names: Vec::new(),
                        working_directory: std::env::current_dir().unwrap_or_default(),
                        plan_mode: false,
                        prompt_behavior: PermissionPromptBehavior::Interactive,
                        prompt_memory_mode: quine_core::PromptMemoryMode::Disabled,
                        agent_key: None,
                        team_key: None,
                        memory_policy: quine_core::MemoryPolicyConfig::default(),
                        model_profile: Some("missing-profile".into()),
                        session_group: None,
                        auto_compact_threshold_percent: 60,
                        status_report_min_tool_rounds:
                            quine_core::default_status_report_min_tool_rounds(),
                    },
                    history: Vec::new(),
                    plan_store: quine_core::PersistedPlanStore::default(),
                    memory_state: None,
                    permission_state: None,
                    status_report: None,
                    python_state: None,
                }],
                quine_core::PersistedSessionTree {
                    parents: HashMap::new(),
                    children: HashMap::new(),
                    exit_statuses: HashMap::new(),
                },
            ))
            .await
            .unwrap();

        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .expect("unknown restored model profiles should fall back during startup");

        let sessions = harness.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(
            sessions[0]
                .get("model_profile")
                .and_then(|value| value.as_str()),
            None
        );

        harness.shutdown().await.unwrap();

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
    async fn advanced_memory_policy_denies_agent_writes_without_fallback() {
        let storage = temp_storage();
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(storage.clone()))
            .await
            .unwrap();
        let working_directory = storage.root().join("untrusted-project");
        std::fs::create_dir_all(&working_directory).unwrap();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(working_directory.clone()),
                agent_key: Some("planner".into()),
                team_key: Some("infra".into()),
                memory_policy: quine_core::MemoryPolicyConfig {
                    flags: quine_core::MemoryFeatureFlags {
                        advanced_scopes_enabled: true,
                        agent_memory_enabled: true,
                        team_memory_enabled: true,
                        ..quine_core::MemoryFeatureFlags::default()
                    },
                    default_write_scope: Some(quine_core::ScopeSelector::Agent),
                    write_policy: quine_core::MemoryWritePolicy {
                        require_trusted_workspace_for_writes: true,
                        require_explicit_user_intent_for_agent_writes: true,
                        ..quine_core::MemoryWritePolicy::default()
                    },
                    ..quine_core::MemoryPolicyConfig::default()
                },
                ..SessionConfig::default()
            })
            .await
            .unwrap();
        let mut rx = harness.subscribe();

        harness
            .send_message(
                session_id,
                "Remember: my deployment region is us-west-2.".into(),
            )
            .await
            .unwrap();
        let reply = wait_for_turn(&mut rx, session_id).await;
        assert_eq!(reply, "Remember: my deployment region is us-west-2.");

        let snapshot = wait_for_context_snapshot_matching(&harness, session_id, |snapshot| {
            snapshot
                .memory_diagnostics
                .as_ref()
                .map(|diagnostics| {
                    diagnostics
                        .persistent_memory
                        .extraction
                        .last_extracted_message_index
                        .is_some()
                })
                .unwrap_or(false)
        })
        .await;
        let diagnostics = snapshot.memory_diagnostics.unwrap();
        assert_eq!(
            diagnostics.persistent_memory.writable_scope,
            Some(quine_core::PersistentMemoryScope::agent(
                crate::memory_store::project_key(&working_directory),
                "planner",
            ))
        );
        assert_eq!(
            diagnostics.persistent_memory.write_status,
            quine_core::MemoryStatus::Skipped
        );

        let agent_root = storage
            .root()
            .join("memory")
            .join("agents")
            .join(crate::memory_store::project_key(&working_directory))
            .join("planner");
        let project_root = storage
            .root()
            .join("memory")
            .join("projects")
            .join(crate::memory_store::project_key(&working_directory));
        assert!(!agent_root.exists());
        assert!(!project_root.exists());

        harness.shutdown().await.unwrap();
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
        let harness = LocalHarness::new(Arc::new(MockProvider), Some(temp_storage()))
            .await
            .unwrap();
        harness
            .schedule_agent(None, "do work".into(), None, Duration::from_secs(1), None)
            .await
            .unwrap();
        harness.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn local_harness_scheduler_background_style_session_denies_without_pending_approval() {
        let marker_name = format!("qa-050-scheduled-background-{}.txt", uuid::Uuid::new_v4());
        let storage = temp_storage();
        let workspace = storage.root().to_path_buf();
        let marker_path = workspace.join(&marker_name);
        let _ = fs::remove_file(&marker_path);

        let harness = LocalHarness::new(
            Arc::new(BackgroundApprovalProvider {
                call_count: std::sync::atomic::AtomicU32::new(0),
                marker_name: marker_name.clone(),
            }),
            Some(storage),
        )
        .await
        .unwrap();
        let mut rx = harness.subscribe();

        let session_id = harness
            .create_session(SessionConfig {
                working_directory: Some(workspace.clone()),
                prompt_behavior: PermissionPromptBehavior::Headless,
                ..SessionConfig::default()
            })
            .await
            .unwrap();

        harness
            .send_message(
                session_id,
                "attempt the scheduled background mutation".into(),
            )
            .await
            .unwrap();

        let mut saw_interaction = false;
        let mut saw_denied_bash = false;
        let saw_turn_complete = loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv()).await {
                Ok(Ok(CoreOutput::InteractionNeeded {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    saw_interaction = true;
                }
                Ok(Ok(CoreOutput::ToolResult {
                    session_id: event_session_id,
                    tool_name,
                    is_error,
                    content,
                    ..
                })) if event_session_id == session_id && tool_name == "bash" => {
                    assert!(is_error, "headless bash should fail safe");
                    assert!(content.contains("permission denied"));
                    saw_denied_bash = true;
                }
                Ok(Ok(CoreOutput::TurnComplete {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => {
                    break true;
                }
                Ok(Ok(_)) => {}
                Ok(Err(error)) => panic!("event stream closed unexpectedly: {error}"),
                Err(_) => panic!("timeout waiting for headless background-style session"),
            }
        };

        assert!(!saw_interaction, "background session should not prompt");
        assert!(
            saw_denied_bash,
            "headless session should emit a permission denial"
        );
        assert!(
            saw_turn_complete,
            "headless session should complete deterministically"
        );
        assert!(
            !marker_path.exists(),
            "headless denial must not run the bash command"
        );

        harness.shutdown().await.unwrap();
        let _ = fs::remove_file(marker_path);
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
        let harness = LocalHarness::new(Arc::new(EchoProvider), Some(temp_storage()))
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

        harness
            .schedule_message(session_id, "scheduled".into(), Duration::from_secs(60))
            .await
            .unwrap();

        tokio::task::yield_now().await;

        tokio::time::advance(Duration::from_secs(60)).await;
        tokio::task::yield_now().await;

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
        tokio::task::yield_now().await;

        while completions.len() < 2 {
            if let CoreOutput::TextComplete { full_text, .. } = rx.recv().await.unwrap() {
                completions.push(full_text);
            }
        }

        assert_eq!(completions, vec!["now", "later"]);
        harness.shutdown().await.unwrap();
    }
}
