use std::path::PathBuf;

use chrono::{DateTime, Utc};
use quine_llm::ToolDefinition;
use serde::{Deserialize, Serialize};

use crate::client::IpcClient;
use crate::render::Renderer;
use quine_harness::protocol::methods;
use quine_harness::PermissionPromptBehavior;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanSnapshot {
    pub plan_id: String,
    pub title: String,
    pub actions: Vec<PlanActionSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanActionSnapshot {
    pub action_id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: PlanActionStatusSnapshot,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PlanActionStatusSnapshot {
    Pending,
    InProgress,
    Completed,
    Failed { error: String },
    Skipped { reason: String },
}

impl PlanActionStatusSnapshot {
    pub fn label(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::Skipped { .. } => "skipped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillSnapshot {
    pub name: String,
    pub description: String,
    pub version: String,
    pub system_prompt: Option<String>,
    pub source_path: PathBuf,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PromptMemoryMode {
    Disabled,
    IndexOnly,
    TargetedRecall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PromptMemorySnapshot {
    pub mode: PromptMemoryMode,
    pub selected_entry_ids: Vec<String>,
    pub selected_titles: Vec<String>,
    pub skipped_reasons: Vec<String>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryStatusSnapshot {
    NotRun,
    Succeeded,
    Skipped,
    FailedBestEffort,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryDecisionReasonSnapshot {
    Disabled,
    NoActivityYet,
    NoNewMessages,
    NoChanges,
    NoQuery,
    NoIndex,
    NoMatchingEntries,
    Duplicate,
    Budget,
    RefreshNotNeeded,
    MissingSummary,
    InvalidBoundary,
    Fallback,
    NotAttempted,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PersistentMemoryScopeSnapshot {
    Project {
        project_key: String,
    },
    Agent {
        project_key: String,
        agent_key: String,
    },
    Team {
        team_key: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryConflictResolutionSnapshot {
    PreferNarrowerScope,
    PreferBroaderScope,
    PreferMostRecentlyUpdated,
    ErrorOnConflictingWrites,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MemoryAuthorizationReasonSnapshot {
    ScopeDisabled,
    ScopeUnavailable,
    TrustedWorkspaceRequired,
    ExplicitIntentRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CompactionSourceSnapshot {
    SessionMemory,
    LegacySummarizer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemorySelectionEntrySnapshot {
    pub entry_id: String,
    pub title: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemorySkippedEntrySnapshot {
    pub entry_id: String,
    pub reason: MemoryDecisionReasonSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionRefreshDiagnosticsSnapshot {
    pub attempted: bool,
    pub status: MemoryStatusSnapshot,
    pub reason: Option<MemoryDecisionReasonSnapshot>,
    pub last_summarized_message_index: Option<usize>,
    pub refreshed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CompactionDiagnosticsSnapshot {
    pub status: MemoryStatusSnapshot,
    pub source: Option<CompactionSourceSnapshot>,
    pub reason: Option<MemoryDecisionReasonSnapshot>,
    pub tail_start: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionMemoryDiagnosticsSnapshot {
    pub enabled: bool,
    pub summary_path: Option<PathBuf>,
    pub metadata_path: Option<PathBuf>,
    pub refresh: SessionRefreshDiagnosticsSnapshot,
    pub compaction: CompactionDiagnosticsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PromptMemoryDiagnosticsSnapshot {
    pub mode: PromptMemoryMode,
    pub injection_ran: bool,
    pub status: MemoryStatusSnapshot,
    pub reason: Option<MemoryDecisionReasonSnapshot>,
    pub selected_entries: Vec<MemorySelectionEntrySnapshot>,
    pub skipped_entries: Vec<MemorySkippedEntrySnapshot>,
    pub truncated: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistentExtractionDiagnosticsSnapshot {
    pub attempted: bool,
    pub status: MemoryStatusSnapshot,
    pub reason: Option<MemoryDecisionReasonSnapshot>,
    pub last_extracted_message_index: Option<usize>,
    pub created: usize,
    pub updated: usize,
    pub tombstoned: usize,
    pub ignored: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PersistentMemoryDiagnosticsSnapshot {
    pub enabled: bool,
    pub project_root: Option<PathBuf>,
    pub readable_scopes: Vec<PersistentMemoryScopeSnapshot>,
    pub writable_scope: Option<PersistentMemoryScopeSnapshot>,
    pub conflict_resolution: Option<MemoryConflictResolutionSnapshot>,
    pub conflict_winner_scope: Option<PersistentMemoryScopeSnapshot>,
    pub write_status: MemoryStatusSnapshot,
    pub write_reason: Option<MemoryAuthorizationReasonSnapshot>,
    pub extraction: PersistentExtractionDiagnosticsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MemoryDiagnosticsSnapshot {
    pub session_memory: SessionMemoryDiagnosticsSnapshot,
    pub prompt_memory: PromptMemoryDiagnosticsSnapshot,
    pub persistent_memory: PersistentMemoryDiagnosticsSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionModeSnapshot {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionDecisionSnapshot {
    Allow,
    Deny,
    Ask,
    Defer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionRuleEffectSnapshot {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionRuleSourceSnapshot {
    BuiltIn,
    Session,
    User,
    Workspace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionRuleScopeSnapshot {
    Session,
    Workspace,
    Global,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionRequestScopeSnapshot {
    Read,
    Write,
    Execute,
    ProcessControl,
    AgentControl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PermissionTargetSnapshot {
    Any,
    Tool { name: String },
    Path { path: PathBuf },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionRuleSnapshot {
    pub effect: PermissionRuleEffectSnapshot,
    pub scope: PermissionRuleScopeSnapshot,
    pub request_scope: Option<PermissionRequestScopeSnapshot>,
    pub target: PermissionTargetSnapshot,
    pub source_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionRuleSetSnapshot {
    pub built_in: Vec<PermissionRuleSnapshot>,
    pub session: Vec<PermissionRuleSnapshot>,
    pub user: Vec<PermissionRuleSnapshot>,
    pub workspace: Vec<PermissionRuleSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionMatchKindSnapshot {
    ToolLocal,
    FilesystemBoundary,
    Rule,
    ModeDefault,
    HeadlessFallback,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct MatchedPermissionSourceSnapshot {
    pub kind: PermissionMatchKindSnapshot,
    pub rule_source: Option<PermissionRuleSourceSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PermissionResourceSnapshot {
    None,
    Path {
        path: PathBuf,
    },
    Command {
        descriptor: PermissionCommandDescriptorSnapshot,
    },
    Process {
        target: String,
    },
    Agent {
        target: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CommandRiskSnapshot {
    ReadOnly,
    Mutating,
    NestedShell,
    Interpreter,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionCommandDescriptorSnapshot {
    pub command: String,
    pub program: Option<String>,
    pub argv: Vec<String>,
    pub risk: CommandRiskSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionRequestSnapshot {
    pub tool_name: String,
    pub action: Option<String>,
    pub scope: PermissionRequestScopeSnapshot,
    pub resource: PermissionResourceSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionOutcomeSnapshot {
    pub kind: String,
    pub final_decision: PermissionDecisionSnapshot,
    pub source: MatchedPermissionSourceSnapshot,
    pub reason: String,
    pub request: PermissionRequestSnapshot,
    pub matched_rule: Option<PermissionRuleSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PendingPermissionApprovalSnapshot {
    pub request_id: String,
    pub outcome: PermissionOutcomeSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PermissionDiagnosticsSnapshot {
    pub mode: PermissionModeSnapshot,
    pub pre_plan_mode: Option<PermissionModeSnapshot>,
    pub prompt_behavior: PermissionPromptBehavior,
    pub workspace_root: PathBuf,
    pub additional_allowed_roots: Vec<PathBuf>,
    pub rules: PermissionRuleSetSnapshot,
    pub last_decision: Option<PermissionOutcomeSnapshot>,
    pub pending_approval: Option<PendingPermissionApprovalSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionContextSnapshot {
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
    pub prompt_memory: Option<PromptMemorySnapshot>,
    pub memory_diagnostics: Option<MemoryDiagnosticsSnapshot>,
    pub permission_diagnostics: Option<PermissionDiagnosticsSnapshot>,
    pub history: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HistoryEntry {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolCallEntry {
    pub tool_use_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

pub(crate) async fn fetch_session_context(
    client: &mut IpcClient,
    session_id: &str,
) -> anyhow::Result<SessionContextSnapshot> {
    let params = serde_json::json!({ "session_id": session_id });
    let result = client
        .call(methods::GET_SESSION_CONTEXT, Some(params))
        .await?;
    let value = result.map_err(|message| anyhow::anyhow!(message))?;
    Ok(serde_json::from_value(value)?)
}

pub(crate) async fn render_session_context<R: Renderer>(
    renderer: &mut R,
    client: &mut IpcClient,
    session_id: &str,
) -> anyhow::Result<()> {
    let snapshot = fetch_session_context(client, session_id).await?;
    renderer
        .render_info(&serde_json::to_string_pretty(&snapshot)?)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn session_context_snapshot_deserializes_memory_diagnostics() {
        let value = json!({
            "session_id": "s1",
            "created_at": "2025-01-01T00:00:00Z",
            "state": "idle",
            "system_prompt": null,
            "skills": [],
            "working_directory": "/tmp/project",
            "plan_mode": false,
            "available_tools": [],
            "loaded_skills": [],
            "plans": [],
            "prompt_memory": null,
            "memory_diagnostics": {
                "session_memory": {
                    "enabled": true,
                    "summary_path": "/tmp/project/summary.md",
                    "metadata_path": "/tmp/project/summary.meta.json",
                    "refresh": {
                        "attempted": false,
                        "status": "not_run",
                        "reason": "no_activity_yet",
                        "last_summarized_message_index": null,
                        "refreshed_at": null
                    },
                    "compaction": {
                        "status": "not_run",
                        "source": null,
                        "reason": null,
                        "tail_start": null
                    }
                },
                "prompt_memory": {
                    "mode": "disabled",
                    "injection_ran": false,
                    "status": "skipped",
                    "reason": "disabled",
                    "selected_entries": [],
                    "skipped_entries": [],
                    "truncated": false
                },
                "persistent_memory": {
                    "enabled": true,
                    "project_root": "/tmp/project",
                    "readable_scopes": [],
                    "writable_scope": null,
                    "conflict_resolution": null,
                    "conflict_winner_scope": null,
                    "write_status": "not_run",
                    "write_reason": null,
                    "extraction": {
                        "attempted": false,
                        "status": "not_run",
                        "reason": null,
                        "last_extracted_message_index": null,
                        "created": 0,
                        "updated": 0,
                        "tombstoned": 0,
                        "ignored": 0
                    }
                }
            },
            "permission_diagnostics": {
                "mode": "default",
                "pre_plan_mode": null,
                "prompt_behavior": "interactive",
                "workspace_root": "/tmp/project",
                "additional_allowed_roots": [],
                "rules": {
                    "built_in": [],
                    "session": [],
                    "user": [],
                    "workspace": []
                },
                "last_decision": null,
                "pending_approval": null
            },
            "history": []
        });

        let snapshot: SessionContextSnapshot = serde_json::from_value(value).unwrap();
        assert!(snapshot.memory_diagnostics.is_some());
        assert!(snapshot.permission_diagnostics.is_some());
    }
}
