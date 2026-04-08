use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::memory::{MemoryPolicyConfig, ScopedPersistentMemoryState};
use crate::permission::{PermissionPromptBehavior, PermissionRuntimeSnapshot};
use crate::planner::ActionPlan;
use crate::session::{ExitStatus, SessionId, SessionState};
use crate::status_report::{default_status_report_min_tool_rounds, SessionStatusReport};

pub const CORE_CHECKPOINT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoreCheckpoint {
    pub format_version: u32,
    pub sessions: Vec<PersistedSession>,
    pub session_tree: PersistedSessionTree,
}

impl CoreCheckpoint {
    pub fn new(sessions: Vec<PersistedSession>, session_tree: PersistedSessionTree) -> Self {
        Self {
            format_version: CORE_CHECKPOINT_FORMAT_VERSION,
            sessions,
            session_tree,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSession {
    pub session_id: SessionId,
    pub created_at: DateTime<Utc>,
    pub state: PersistedSessionState,
    pub config: PersistedSessionConfig,
    pub history: Vec<quine_llm::Message>,
    pub plan_store: PersistedPlanStore,
    #[serde(default)]
    pub memory_state: Option<PersistedMemoryState>,
    #[serde(default)]
    pub permission_state: Option<PermissionRuntimeSnapshot>,
    #[serde(default)]
    pub status_report: Option<SessionStatusReport>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSessionConfig {
    pub system_prompt: Option<String>,
    pub skill_names: Vec<String>,
    pub working_directory: PathBuf,
    pub plan_mode: bool,
    #[serde(default = "default_permission_prompt_behavior")]
    pub prompt_behavior: PermissionPromptBehavior,
    #[serde(default)]
    pub prompt_memory_mode: PromptMemoryMode,
    #[serde(default)]
    pub agent_key: Option<String>,
    #[serde(default)]
    pub team_key: Option<String>,
    #[serde(default)]
    pub memory_policy: MemoryPolicyConfig,
    #[serde(default)]
    pub model_profile: Option<String>,
    #[serde(default = "default_auto_compact_threshold_percent")]
    pub auto_compact_threshold_percent: u8,
    #[serde(default = "default_status_report_min_tool_rounds")]
    pub status_report_min_tool_rounds: u32,
}

fn default_auto_compact_threshold_percent() -> u8 {
    crate::compaction::DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT
}

fn default_permission_prompt_behavior() -> PermissionPromptBehavior {
    PermissionPromptBehavior::Interactive
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedMemoryState {
    #[serde(default)]
    pub session_memory: Option<PersistedSessionMemoryState>,
    #[serde(default)]
    pub persistent_memory: Option<PersistedPersistentMemoryState>,
    #[serde(default)]
    pub prompt_memory: Option<PersistedPromptMemoryState>,
    #[serde(default)]
    pub memory_diagnostics: Option<crate::memory::MemoryTurnDiagnostics>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PromptMemoryMode {
    #[default]
    Disabled,
    IndexOnly,
    TargetedRecall,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedSessionMemoryState {
    pub enabled: bool,
    pub last_summarized_message_index: Option<usize>,
    pub template_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPersistentMemoryState {
    pub enabled: bool,
    pub last_extracted_message_index: Option<usize>,
    #[serde(default)]
    pub scope_state: Option<ScopedPersistentMemoryState>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistedPromptMemoryState {
    pub mode: PromptMemoryMode,
    #[serde(default)]
    pub selected_entry_ids: Vec<String>,
    #[serde(default)]
    pub selected_titles: Vec<String>,
    #[serde(default)]
    pub skipped_reasons: Vec<String>,
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PersistedSessionState {
    Idle,
    Paused,
    Destroyed,
}

impl PersistedSessionState {
    pub fn from_runtime(state: SessionState) -> Option<Self> {
        match state {
            SessionState::Idle => Some(Self::Idle),
            SessionState::Paused => Some(Self::Paused),
            SessionState::Destroyed => Some(Self::Destroyed),
            SessionState::Streaming | SessionState::AwaitingToolResult | SessionState::Waiting => {
                None
            }
        }
    }
}

impl From<PersistedSessionState> for SessionState {
    fn from(value: PersistedSessionState) -> Self {
        match value {
            PersistedSessionState::Idle => SessionState::Idle,
            PersistedSessionState::Paused => SessionState::Paused,
            PersistedSessionState::Destroyed => SessionState::Destroyed,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedSessionTree {
    pub parents: HashMap<SessionId, SessionId>,
    pub children: HashMap<SessionId, Vec<SessionId>>,
    pub exit_statuses: HashMap<SessionId, ExitStatus>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PersistedPlanStore {
    pub plans: Vec<ActionPlan>,
}

impl PersistedPlanStore {
    pub fn from_plans(plans: impl IntoIterator<Item = ActionPlan>) -> Self {
        Self {
            plans: plans.into_iter().collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{Action, ActionId, ActionStatus, PlanId};

    #[test]
    fn unstable_runtime_states_are_not_persistable() {
        assert_eq!(
            PersistedSessionState::from_runtime(SessionState::Streaming),
            None
        );
        assert_eq!(
            PersistedSessionState::from_runtime(SessionState::AwaitingToolResult),
            None
        );
        assert_eq!(
            PersistedSessionState::from_runtime(SessionState::Waiting),
            None
        );
    }

    #[test]
    fn stable_runtime_states_are_persistable() {
        assert_eq!(
            PersistedSessionState::from_runtime(SessionState::Idle),
            Some(PersistedSessionState::Idle)
        );
        assert_eq!(
            PersistedSessionState::from_runtime(SessionState::Paused),
            Some(PersistedSessionState::Paused)
        );
    }

    #[test]
    fn plan_store_serializes() {
        let store = PersistedPlanStore::from_plans([ActionPlan {
            plan_id: PlanId::new(),
            title: "plan".into(),
            actions: vec![Action {
                action_id: ActionId::new("a1"),
                title: "title".into(),
                description: "desc".into(),
                depends_on: Vec::new(),
                status: ActionStatus::Pending,
                result: None,
            }],
        }]);

        let json = serde_json::to_string(&store).unwrap();
        let roundtrip: PersistedPlanStore = serde_json::from_str(&json).unwrap();
        assert_eq!(roundtrip.plans.len(), 1);
        assert_eq!(roundtrip.plans[0].title, "plan");
    }

    #[test]
    fn memory_state_defaults_for_older_checkpoints() {
        let json = serde_json::json!({
            "session_id": SessionId::new(),
            "created_at": Utc::now(),
            "state": "Idle",
            "config": {
                "system_prompt": null,
                "skill_names": [],
                "working_directory": ".",
                "plan_mode": false,
                "prompt_memory_mode": "disabled"
            },
            "history": [],
            "plan_store": { "plans": [] }
        });
        let roundtrip: PersistedSession = serde_json::from_value(json).unwrap();
        assert!(roundtrip.memory_state.is_none());
        assert!(roundtrip.permission_state.is_none());
        assert!(roundtrip.status_report.is_none());
        assert_eq!(
            roundtrip.config.status_report_min_tool_rounds,
            default_status_report_min_tool_rounds()
        );
    }

    #[test]
    fn persisted_memory_state_round_trips_without_summary_contents() {
        let state = PersistedMemoryState {
            session_memory: Some(PersistedSessionMemoryState {
                enabled: true,
                last_summarized_message_index: Some(4),
                template_version: 1,
                listing_summary: Some("Summarizes the current session for listing.".into()),
            }),
            persistent_memory: Some(PersistedPersistentMemoryState {
                enabled: true,
                last_extracted_message_index: Some(6),
                scope_state: None,
            }),
            prompt_memory: Some(PersistedPromptMemoryState {
                mode: PromptMemoryMode::TargetedRecall,
                selected_entry_ids: vec!["entry-a".into()],
                selected_titles: vec!["Entry A".into()],
                skipped_reasons: vec!["budget".into()],
                truncated: true,
            }),
            memory_diagnostics: None,
        };
        let json = serde_json::to_value(&state).unwrap();
        assert!(json.get("summary").is_none());
        let roundtrip: PersistedMemoryState = serde_json::from_value(json).unwrap();
        assert_eq!(roundtrip, state);
        assert_eq!(
            roundtrip
                .persistent_memory
                .as_ref()
                .and_then(|state| state.last_extracted_message_index),
            Some(6)
        );
        assert_eq!(
            roundtrip.prompt_memory.as_ref().map(|state| state.mode),
            Some(PromptMemoryMode::TargetedRecall)
        );
    }
}
