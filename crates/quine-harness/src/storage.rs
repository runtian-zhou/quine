use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use quine_core::{
    built_in_tool_definitions,
    planner::{ActionPlan, ActionStatus},
    skill, CoreCheckpoint, MemoryTurnDiagnostics, PermissionRuntimeSnapshot,
    PersistedPromptMemoryState, PersistedSession, SessionId,
};
use quine_llm::{Message, MessageContent, Role, ToolDefinition};
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone, Serialize)]
pub struct PlanSnapshot {
    pub plan_id: String,
    pub title: String,
    pub actions: Vec<PlanActionSnapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanActionSnapshot {
    pub action_id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: PlanActionStatusSnapshot,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PlanActionStatusSnapshot {
    Pending,
    InProgress,
    Completed,
    Failed { error: String },
    Skipped { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct SkillSnapshot {
    pub name: String,
    pub description: String,
    pub version: String,
    pub system_prompt: Option<String>,
    pub source_path: PathBuf,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct SessionLineageSnapshot {
    pub parent_id: Option<String>,
    pub root_id: String,
    pub depth: usize,
    pub child_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionContextSnapshot {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub state: String,
    pub system_prompt: Option<String>,
    pub skills: Vec<String>,
    pub working_directory: PathBuf,
    pub plan_mode: bool,
    pub available_tools: Vec<ToolDefinition>,
    pub loaded_skills: Vec<SkillSnapshot>,
    pub plans: Vec<PlanSnapshot>,
    pub lineage: SessionLineageSnapshot,
    pub prompt_memory: Option<PersistedPromptMemoryState>,
    pub compact_memory_summary_markdown: Option<String>,
    pub memory_diagnostics: Option<MemoryTurnDiagnostics>,
    pub permission_diagnostics: Option<PermissionRuntimeSnapshot>,
    pub history: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HistoryEntry {
    Text {
        role: String,
        text: String,
    },
    ToolUse {
        role: String,
        text: Option<String>,
        tool_calls: Vec<ToolCallEntry>,
    },
    ToolResult {
        role: String,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCallEntry {
    pub tool_use_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

pub fn session_context_from_checkpoint(
    checkpoint: &CoreCheckpoint,
    session_id: SessionId,
    live_states: &HashMap<SessionId, String>,
    state_root: Option<&Path>,
) -> Option<SessionContextSnapshot> {
    checkpoint
        .sessions
        .iter()
        .find(|session| session.session_id == session_id)
        .map(|session| snapshot_from_persisted(checkpoint, session, live_states, state_root))
}

fn snapshot_from_persisted(
    checkpoint: &CoreCheckpoint,
    session: &PersistedSession,
    live_states: &HashMap<SessionId, String>,
    state_root: Option<&Path>,
) -> SessionContextSnapshot {
    SessionContextSnapshot {
        session_id: serialize_session_id(session.session_id),
        created_at: session.created_at,
        state: live_states
            .get(&session.session_id)
            .cloned()
            .unwrap_or_else(|| format!("{:?}", session.state).to_lowercase()),
        system_prompt: session.config.system_prompt.clone(),
        skills: session.config.skill_names.clone(),
        working_directory: session.config.working_directory.clone(),
        plan_mode: session.config.plan_mode,
        available_tools: build_available_tools(session),
        loaded_skills: build_loaded_skills(session),
        plans: session
            .plan_store
            .plans
            .iter()
            .map(plan_snapshot_from_action_plan)
            .collect(),
        lineage: lineage_snapshot(checkpoint, session.session_id),
        prompt_memory: session
            .memory_state
            .as_ref()
            .and_then(|state| state.prompt_memory.clone()),
        compact_memory_summary_markdown: state_root
            .and_then(|root| load_compact_memory_summary(root, session.session_id)),
        memory_diagnostics: session
            .memory_state
            .as_ref()
            .and_then(|state| state.memory_diagnostics.clone()),
        permission_diagnostics: session.permission_state.clone(),
        history: session
            .history
            .iter()
            .map(history_entry_from_message)
            .collect(),
    }
}

fn lineage_snapshot(checkpoint: &CoreCheckpoint, session_id: SessionId) -> SessionLineageSnapshot {
    let parent_id = checkpoint.session_tree.parents.get(&session_id).copied();
    let mut root_id = session_id;
    let mut depth = 0usize;
    while let Some(parent) = checkpoint.session_tree.parents.get(&root_id).copied() {
        root_id = parent;
        depth += 1;
    }
    let mut child_ids = checkpoint
        .session_tree
        .children
        .get(&session_id)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(serialize_session_id)
        .collect::<Vec<_>>();
    child_ids.sort();

    SessionLineageSnapshot {
        parent_id: parent_id.map(serialize_session_id),
        root_id: serialize_session_id(root_id),
        depth,
        child_ids,
    }
}

fn load_compact_memory_summary(state_root: &Path, session_id: SessionId) -> Option<String> {
    let summary_path = state_root
        .join("sessions")
        .join(serialize_session_id(session_id))
        .join("session-memory")
        .join("summary.md");
    let summary_markdown = std::fs::read_to_string(summary_path).ok()?;
    let trimmed = summary_markdown.trim();
    if trimmed.is_empty() || trimmed == "# Session Summary" {
        return None;
    }
    Some(summary_markdown)
}

fn plan_snapshot_from_action_plan(plan: &ActionPlan) -> PlanSnapshot {
    PlanSnapshot {
        plan_id: plan.plan_id.to_string(),
        title: plan.title.clone(),
        actions: plan
            .actions
            .iter()
            .map(|action| PlanActionSnapshot {
                action_id: action.action_id.to_string(),
                title: action.title.clone(),
                description: action.description.clone(),
                depends_on: action.depends_on.iter().map(ToString::to_string).collect(),
                status: plan_status_snapshot(&action.status),
                result: action.result.clone(),
            })
            .collect(),
    }
}

fn plan_status_snapshot(status: &ActionStatus) -> PlanActionStatusSnapshot {
    match status {
        ActionStatus::Pending => PlanActionStatusSnapshot::Pending,
        ActionStatus::InProgress => PlanActionStatusSnapshot::InProgress,
        ActionStatus::Completed => PlanActionStatusSnapshot::Completed,
        ActionStatus::Failed { error } => PlanActionStatusSnapshot::Failed {
            error: error.clone(),
        },
        ActionStatus::Skipped { reason } => PlanActionStatusSnapshot::Skipped {
            reason: reason.clone(),
        },
    }
}

fn build_loaded_skills(session: &PersistedSession) -> Vec<SkillSnapshot> {
    futures::executor::block_on(skill::load_skills(
        &session.config.working_directory,
        &session.config.skill_names,
    ))
    .into_iter()
    .map(|skill| SkillSnapshot {
        name: skill.meta.name,
        description: skill.meta.description,
        version: skill.meta.version,
        system_prompt: skill.system_prompt,
        source_path: skill.source_path,
        tool_names: skill
            .tool_definitions
            .into_iter()
            .map(|tool| tool.name)
            .collect(),
    })
    .collect()
}

fn build_available_tools(session: &PersistedSession) -> Vec<ToolDefinition> {
    let mut tools = built_in_tool_definitions(session.config.plan_mode);

    for skill in futures::executor::block_on(skill::load_skills(
        &session.config.working_directory,
        &session.config.skill_names,
    )) {
        tools.extend(
            skill
                .tool_definitions
                .into_iter()
                .map(|tool| ToolDefinition {
                    name: tool.name,
                    description: tool.description,
                    parameters: tool.parameters,
                    read_only: false,
                    idempotent: false,
                }),
        );
    }

    tools.sort_by(|left, right| left.name.cmp(&right.name));
    tools.dedup_by(|left, right| left.name == right.name);
    tools
}

fn history_entry_from_message(message: &Message) -> HistoryEntry {
    match &message.content {
        MessageContent::Text(text) => HistoryEntry::Text {
            role: role_name(&message.role),
            text: text.clone(),
        },
        MessageContent::ToolUse { text, tool_calls } => HistoryEntry::ToolUse {
            role: role_name(&message.role),
            text: text.clone(),
            tool_calls: tool_calls
                .iter()
                .map(|call| ToolCallEntry {
                    tool_use_id: call.tool_use_id.clone(),
                    tool_name: call.tool_name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
        },
        MessageContent::ToolResult {
            tool_use_id,
            output,
            is_error,
        } => HistoryEntry::ToolResult {
            role: role_name(&message.role),
            tool_use_id: tool_use_id.clone(),
            output: output.clone(),
            is_error: *is_error,
        },
    }
}

fn role_name(role: &Role) -> String {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
    .to_string()
}

fn serialize_session_id(session_id: SessionId) -> String {
    serde_json::to_value(session_id)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct StorageManager {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    format_version: u32,
    current_generation: u64,
}

const MANIFEST_FILE_NAME: &str = "manifest.json";
const TMP_EXTENSION: &str = ".tmp";
const STORAGE_FORMAT_VERSION: u32 = 1;

impl StorageManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn load_latest_checkpoint(&self) -> anyhow::Result<Option<CoreCheckpoint>> {
        let manifest_path = self.manifest_path();
        let manifest_bytes = match fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        let checkpoint_bytes = fs::read(self.checkpoint_path(manifest.current_generation)).await?;
        let checkpoint = serde_json::from_slice(&checkpoint_bytes)?;
        Ok(Some(checkpoint))
    }

    pub async fn commit_checkpoint(&self, checkpoint: &CoreCheckpoint) -> anyhow::Result<()> {
        self.ensure_root().await?;
        let next_generation = self.next_generation().await?;
        let checkpoint_path = self.checkpoint_path(next_generation);
        let checkpoint_tmp_path = self.temporary_path(&checkpoint_path);
        let manifest = Manifest {
            format_version: STORAGE_FORMAT_VERSION,
            current_generation: next_generation,
        };
        let manifest_path = self.manifest_path();
        let manifest_tmp_path = self.temporary_path(&manifest_path);

        self.write_json_atomic(&checkpoint_tmp_path, &checkpoint_path, checkpoint)
            .await?;
        self.write_json_atomic(&manifest_tmp_path, &manifest_path, &manifest)
            .await?;
        Ok(())
    }

    async fn next_generation(&self) -> anyhow::Result<u64> {
        let manifest = self.load_manifest().await?;
        Ok(manifest.map_or(1, |entry| entry.current_generation + 1))
    }

    async fn load_manifest(&self) -> anyhow::Result<Option<Manifest>> {
        let manifest_path = self.manifest_path();
        let manifest_bytes = match fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let manifest = serde_json::from_slice(&manifest_bytes)?;
        Ok(Some(manifest))
    }

    async fn ensure_root(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.root).await?;
        Ok(())
    }

    async fn write_json_atomic<T: Serialize>(
        &self,
        tmp_path: &Path,
        final_path: &Path,
        value: &T,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_vec_pretty(value)?;
        let mut file = fs::File::create(tmp_path).await?;
        file.write_all(&payload).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(tmp_path, final_path).await?;
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE_NAME)
    }

    fn checkpoint_path(&self, generation: u64) -> PathBuf {
        self.root.join(format!("checkpoint-{generation}.json"))
    }

    fn temporary_path(&self, final_path: &Path) -> PathBuf {
        let file_name = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("checkpoint.json");
        final_path.with_file_name(format!("{file_name}{TMP_EXTENSION}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use quine_core::MemoryPolicyConfig;
    use quine_core::{
        CoreCheckpoint, PersistedPlanStore, PersistedSession, PersistedSessionConfig,
        PersistedSessionState, PersistedSessionTree, PromptMemoryMode, SessionId,
    };

    fn make_temp_storage() -> StorageManager {
        let root =
            std::env::temp_dir().join(format!("quine-harness-storage-{}", uuid::Uuid::new_v4()));
        StorageManager::new(root)
    }

    use std::collections::HashMap;

    fn sample_checkpoint() -> CoreCheckpoint {
        CoreCheckpoint::new(
            vec![PersistedSession {
                session_id: SessionId::new(),
                created_at: Utc::now(),
                state: PersistedSessionState::Idle,
                config: PersistedSessionConfig {
                    system_prompt: Some("prompt".into()),
                    skill_names: Vec::new(),
                    working_directory: PathBuf::from("/tmp/project"),
                    plan_mode: false,
                    prompt_behavior: quine_core::PermissionPromptBehavior::Interactive,
                    prompt_memory_mode: quine_core::PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    auto_compact_threshold_percent: 60,
                },
                history: vec![quine_llm::Message::user("hello")],
                plan_store: PersistedPlanStore::default(),
                memory_state: Some(quine_core::PersistedMemoryState {
                    session_memory: None,
                    persistent_memory: None,
                    prompt_memory: Some(quine_core::PersistedPromptMemoryState {
                        mode: PromptMemoryMode::Disabled,
                        selected_entry_ids: Vec::new(),
                        selected_titles: Vec::new(),
                        skipped_reasons: Vec::new(),
                        truncated: false,
                    }),
                    memory_diagnostics: Some(quine_core::MemoryTurnDiagnostics {
                        session_memory: quine_core::SessionMemoryDiagnostics {
                            enabled: true,
                            summary_path: Some(PathBuf::from("/tmp/project/summary.md")),
                            metadata_path: Some(PathBuf::from("/tmp/project/summary.meta.json")),
                            refresh: quine_core::SessionRefreshDiagnostics {
                                attempted: false,
                                status: quine_core::MemoryStatus::NotRun,
                                reason: Some(quine_core::MemoryDecisionReason::NoActivityYet),
                                last_summarized_message_index: None,
                                refreshed_at: None,
                            },
                            compaction: Default::default(),
                        },
                        prompt_memory: quine_core::PromptMemoryDiagnostics {
                            mode: PromptMemoryMode::Disabled,
                            injection_ran: false,
                            status: quine_core::MemoryStatus::Skipped,
                            reason: Some(quine_core::MemoryDecisionReason::Disabled),
                            selected_entries: Vec::new(),
                            skipped_entries: Vec::new(),
                            truncated: false,
                        },
                        persistent_memory: quine_core::PersistentMemoryDiagnostics {
                            enabled: true,
                            project_root: Some(PathBuf::from("/tmp/project")),
                            readable_scopes: Vec::new(),
                            writable_scope: None,
                            conflict_resolution: None,
                            conflict_winner_scope: None,
                            write_status: quine_core::MemoryStatus::NotRun,
                            write_reason: None,
                            extraction: Default::default(),
                        },
                    }),
                }),
                permission_state: Some(quine_core::PermissionRuntimeSnapshot {
                    mode: quine_core::PermissionMode::Default,
                    pre_plan_mode: None,
                    rules: quine_core::PermissionRuleSet::default(),
                    workspace_root: PathBuf::from("/tmp/project"),
                    additional_allowed_roots: Vec::new(),
                    prompt_behavior: quine_core::PermissionPromptBehavior::Interactive,
                    last_decision: None,
                    pending_approval: None,
                }),
            }],
            PersistedSessionTree {
                parents: Default::default(),
                children: Default::default(),
                exit_statuses: Default::default(),
            },
        )
    }

    #[test]
    fn session_context_snapshot_includes_skills_and_tool_history() {
        let checkpoint = sample_checkpoint();
        let session = &checkpoint.sessions[0];
        let session_id = session.session_id;
        let snapshot = super::session_context_from_checkpoint(
            &checkpoint,
            session_id,
            &HashMap::from([(session_id, "streaming".to_string())]),
            None,
        )
        .unwrap();

        assert_eq!(snapshot.skills, Vec::<String>::new());
        assert_eq!(snapshot.state, "streaming");
        assert!(!snapshot.available_tools.is_empty());
        assert!(snapshot
            .available_tools
            .iter()
            .any(|tool| tool.name == "read_file"));
        assert!(snapshot
            .available_tools
            .iter()
            .any(|tool| tool.name == "apply_patch"));
        assert!(!snapshot
            .available_tools
            .iter()
            .any(|tool| tool.name == "write_file"));
        assert!(snapshot.loaded_skills.is_empty());
        assert!(snapshot.plans.is_empty());
        assert!(snapshot.memory_diagnostics.is_some());
        assert!(snapshot.permission_diagnostics.is_some());
        assert_eq!(snapshot.lineage.root_id, snapshot.session_id);
        assert_eq!(snapshot.lineage.depth, 0);
        assert!(snapshot.compact_memory_summary_markdown.is_none());
        match &snapshot.history[0] {
            super::HistoryEntry::Text { role, text } => {
                assert_eq!(role, "user");
                assert_eq!(text, "hello");
            }
            other => panic!("expected text history entry, got {other:?}"),
        }
    }

    #[test]
    fn get_session_context_distinguishes_persisted_and_session_rules() {
        let mut checkpoint = sample_checkpoint();
        checkpoint.sessions[0]
            .permission_state
            .as_mut()
            .expect("permission state should be present")
            .rules = quine_core::PermissionRuleSet {
            built_in: Vec::new(),
            session: vec![quine_core::PermissionRule {
                effect: quine_core::PermissionRuleEffect::Allow,
                scope: quine_core::RuleScope::Session,
                request_scope: Some(quine_core::PermissionScope::Execute),
                target: quine_core::PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: None,
            }],
            user: Vec::new(),
            workspace: vec![quine_core::PermissionRule {
                effect: quine_core::PermissionRuleEffect::Deny,
                scope: quine_core::RuleScope::Workspace,
                request_scope: Some(quine_core::PermissionScope::Write),
                target: quine_core::PermissionTarget::Path {
                    path: PathBuf::from("src"),
                },
                source_path: Some(PathBuf::from("/tmp/project/.quine/permissions.yaml")),
            }],
        };

        let session_id = checkpoint.sessions[0].session_id;
        let snapshot = super::session_context_from_checkpoint(
            &checkpoint,
            session_id,
            &HashMap::from([(session_id, "idle".to_string())]),
            None,
        )
        .expect("session context should project from checkpoint");
        let diagnostics = snapshot
            .permission_diagnostics
            .expect("permission diagnostics should be present");

        assert_eq!(diagnostics.rules.session.len(), 1);
        assert_eq!(diagnostics.rules.workspace.len(), 1);
        assert!(diagnostics.rules.session[0].source_path.is_none());
        assert_eq!(
            diagnostics.rules.workspace[0].source_path.as_deref(),
            Some(Path::new("/tmp/project/.quine/permissions.yaml"))
        );
    }

    #[test]
    fn session_context_snapshot_includes_lineage_and_compact_summary() {
        let storage = make_temp_storage();
        let parent_id = SessionId::new();
        let child_id = SessionId::new();
        let checkpoint = CoreCheckpoint::new(
            vec![
                PersistedSession {
                    session_id: parent_id,
                    ..sample_checkpoint().sessions.into_iter().next().unwrap()
                },
                PersistedSession {
                    session_id: child_id,
                    created_at: Utc::now(),
                    state: PersistedSessionState::Idle,
                    config: PersistedSessionConfig {
                        system_prompt: None,
                        skill_names: Vec::new(),
                        working_directory: PathBuf::from("/tmp/project"),
                        plan_mode: false,
                        prompt_behavior: quine_core::PermissionPromptBehavior::Interactive,
                        prompt_memory_mode: quine_core::PromptMemoryMode::Disabled,
                        agent_key: None,
                        team_key: None,
                        memory_policy: MemoryPolicyConfig::default(),
                        model_profile: None,
                        auto_compact_threshold_percent: 60,
                    },
                    history: Vec::new(),
                    plan_store: PersistedPlanStore::default(),
                    memory_state: None,
                    permission_state: None,
                },
            ],
            PersistedSessionTree {
                parents: HashMap::from([(child_id, parent_id)]),
                children: HashMap::from([(parent_id, vec![child_id])]),
                exit_statuses: HashMap::new(),
            },
        );

        let summary_dir = storage
            .root()
            .join("sessions")
            .join(child_id.to_string())
            .join("session-memory");
        std::fs::create_dir_all(&summary_dir).unwrap();
        std::fs::write(
            summary_dir.join("summary.md"),
            "# Session Summary\n\nCompact context body.\n",
        )
        .unwrap();

        let snapshot = super::session_context_from_checkpoint(
            &checkpoint,
            child_id,
            &HashMap::new(),
            Some(storage.root()),
        )
        .expect("session context should project from checkpoint");

        assert_eq!(
            snapshot.lineage.parent_id.as_deref(),
            Some(parent_id.to_string().as_str())
        );
        assert_eq!(snapshot.lineage.root_id, parent_id.to_string());
        assert_eq!(snapshot.lineage.depth, 1);
        assert_eq!(snapshot.lineage.child_ids, Vec::<String>::new());
        assert_eq!(
            snapshot.compact_memory_summary_markdown.as_deref(),
            Some("# Session Summary\n\nCompact context body.\n")
        );
    }

    #[tokio::test]
    async fn commit_and_load_roundtrip() {
        let storage = make_temp_storage();
        let checkpoint = sample_checkpoint();

        storage.commit_checkpoint(&checkpoint).await.unwrap();
        let loaded = storage.load_latest_checkpoint().await.unwrap().unwrap();

        assert_eq!(loaded.format_version, checkpoint.format_version);
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].history.len(), 1);
    }

    #[tokio::test]
    async fn manifest_controls_visible_generation() {
        let storage = make_temp_storage();
        let checkpoint = sample_checkpoint();
        storage.commit_checkpoint(&checkpoint).await.unwrap();

        let stray_path = storage.checkpoint_path(99);
        fs::write(&stray_path, serde_json::to_vec_pretty(&checkpoint).unwrap())
            .await
            .unwrap();

        let loaded = storage.load_latest_checkpoint().await.unwrap().unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert!(storage.manifest_path().exists());
    }
}
