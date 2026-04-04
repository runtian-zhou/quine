use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::memory::{
    MemoryAuthorizationReason, MemoryConflictResolution, PersistentMemoryScope,
    ScopedPersistentMemoryState,
};
use crate::persistence::PromptMemoryMode;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MemoryTurnDiagnostics {
    pub session_memory: SessionMemoryDiagnostics,
    pub prompt_memory: PromptMemoryDiagnostics,
    pub persistent_memory: PersistentMemoryDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct SessionMemoryDiagnostics {
    pub enabled: bool,
    pub summary_path: Option<PathBuf>,
    pub metadata_path: Option<PathBuf>,
    pub refresh: SessionRefreshDiagnostics,
    pub compaction: CompactionDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionRefreshDiagnostics {
    pub attempted: bool,
    pub status: MemoryStatus,
    pub reason: Option<MemoryDecisionReason>,
    pub last_summarized_message_index: Option<usize>,
    pub refreshed_at: Option<String>,
}

impl Default for SessionRefreshDiagnostics {
    fn default() -> Self {
        Self {
            attempted: false,
            status: MemoryStatus::NotRun,
            reason: None,
            last_summarized_message_index: None,
            refreshed_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CompactionDiagnostics {
    pub status: MemoryStatus,
    pub source: Option<CompactionSourceDiagnostics>,
    pub reason: Option<MemoryDecisionReason>,
    pub tail_start: Option<usize>,
}

impl Default for CompactionDiagnostics {
    fn default() -> Self {
        Self {
            status: MemoryStatus::NotRun,
            source: None,
            reason: None,
            tail_start: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptMemoryDiagnostics {
    pub mode: PromptMemoryMode,
    pub injection_ran: bool,
    pub status: MemoryStatus,
    pub reason: Option<MemoryDecisionReason>,
    pub selected_entries: Vec<MemorySelectionEntryDiagnostics>,
    pub skipped_entries: Vec<MemorySkippedEntryDiagnostics>,
    pub truncated: bool,
}

impl Default for PromptMemoryDiagnostics {
    fn default() -> Self {
        Self {
            mode: PromptMemoryMode::Disabled,
            injection_ran: false,
            status: MemoryStatus::NotRun,
            reason: None,
            selected_entries: Vec::new(),
            skipped_entries: Vec::new(),
            truncated: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct PersistentMemoryDiagnostics {
    pub enabled: bool,
    pub project_root: Option<PathBuf>,
    #[serde(default)]
    pub readable_scopes: Vec<PersistentMemoryScope>,
    #[serde(default)]
    pub writable_scope: Option<PersistentMemoryScope>,
    #[serde(default)]
    pub conflict_resolution: Option<MemoryConflictResolution>,
    #[serde(default)]
    pub conflict_winner_scope: Option<PersistentMemoryScope>,
    #[serde(default)]
    pub write_status: MemoryStatus,
    #[serde(default)]
    pub write_reason: Option<MemoryAuthorizationReason>,
    pub extraction: PersistentExtractionDiagnostics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentExtractionDiagnostics {
    pub attempted: bool,
    pub status: MemoryStatus,
    pub reason: Option<MemoryDecisionReason>,
    pub last_extracted_message_index: Option<usize>,
    pub created: usize,
    pub updated: usize,
    pub tombstoned: usize,
    pub ignored: usize,
}

impl Default for PersistentExtractionDiagnostics {
    fn default() -> Self {
        Self {
            attempted: false,
            status: MemoryStatus::NotRun,
            reason: None,
            last_extracted_message_index: None,
            created: 0,
            updated: 0,
            tombstoned: 0,
            ignored: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySelectionEntryDiagnostics {
    pub entry_id: String,
    pub title: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemorySkippedEntryDiagnostics {
    pub entry_id: String,
    pub reason: MemoryDecisionReason,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryStatus {
    #[default]
    NotRun,
    Succeeded,
    Skipped,
    FailedBestEffort,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompactionSourceDiagnostics {
    SessionMemory,
    LegacySummarizer,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryDecisionReason {
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

pub(crate) fn default_turn_diagnostics(
    summary_path: &Path,
    metadata_path: &Path,
    prompt_memory_mode: PromptMemoryMode,
    project_root: PathBuf,
    persistent_enabled: bool,
    scope_state: Option<&ScopedPersistentMemoryState>,
) -> MemoryTurnDiagnostics {
    MemoryTurnDiagnostics {
        session_memory: SessionMemoryDiagnostics {
            enabled: true,
            summary_path: Some(summary_path.to_path_buf()),
            metadata_path: Some(metadata_path.to_path_buf()),
            refresh: SessionRefreshDiagnostics::default(),
            compaction: CompactionDiagnostics::default(),
        },
        prompt_memory: PromptMemoryDiagnostics {
            mode: prompt_memory_mode,
            ..PromptMemoryDiagnostics::default()
        },
        persistent_memory: PersistentMemoryDiagnostics {
            enabled: persistent_enabled,
            project_root: Some(project_root),
            readable_scopes: scope_state
                .map(|state| state.readable_scopes.clone())
                .unwrap_or_default(),
            writable_scope: scope_state.and_then(|state| state.writable_scope.clone()),
            conflict_resolution: scope_state.map(|state| state.conflict_resolution),
            conflict_winner_scope: None,
            write_status: MemoryStatus::NotRun,
            write_reason: None,
            extraction: PersistentExtractionDiagnostics::default(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_turn_diagnostics_roundtrip_serializes() {
        let diagnostics = MemoryTurnDiagnostics {
            session_memory: SessionMemoryDiagnostics {
                enabled: true,
                summary_path: Some(PathBuf::from("/tmp/summary.md")),
                metadata_path: Some(PathBuf::from("/tmp/summary.meta.json")),
                refresh: SessionRefreshDiagnostics {
                    attempted: true,
                    status: MemoryStatus::Succeeded,
                    reason: None,
                    last_summarized_message_index: Some(4),
                    refreshed_at: Some("2025-01-01T00:00:00Z".into()),
                },
                compaction: CompactionDiagnostics {
                    status: MemoryStatus::Skipped,
                    source: Some(CompactionSourceDiagnostics::LegacySummarizer),
                    reason: Some(MemoryDecisionReason::Fallback),
                    tail_start: Some(5),
                },
            },
            prompt_memory: PromptMemoryDiagnostics {
                mode: PromptMemoryMode::TargetedRecall,
                injection_ran: true,
                status: MemoryStatus::Succeeded,
                reason: None,
                selected_entries: vec![MemorySelectionEntryDiagnostics {
                    entry_id: "cargo-test".into(),
                    title: "Cargo test".into(),
                    path: "entries/cargo-test.md".into(),
                }],
                skipped_entries: vec![MemorySkippedEntryDiagnostics {
                    entry_id: "other".into(),
                    reason: MemoryDecisionReason::Budget,
                }],
                truncated: false,
            },
            persistent_memory: PersistentMemoryDiagnostics {
                enabled: true,
                project_root: Some(PathBuf::from("/repo")),
                readable_scopes: vec![PersistentMemoryScope::project("project")],
                writable_scope: Some(PersistentMemoryScope::project("project")),
                conflict_resolution: Some(MemoryConflictResolution::PreferNarrowerScope),
                conflict_winner_scope: Some(PersistentMemoryScope::project("project")),
                write_status: MemoryStatus::Succeeded,
                write_reason: None,
                extraction: PersistentExtractionDiagnostics {
                    attempted: true,
                    status: MemoryStatus::Succeeded,
                    reason: None,
                    last_extracted_message_index: Some(8),
                    created: 1,
                    updated: 0,
                    tombstoned: 0,
                    ignored: 2,
                },
            },
        };

        let value = serde_json::to_value(&diagnostics).unwrap();
        let roundtrip: MemoryTurnDiagnostics = serde_json::from_value(value).unwrap();
        assert_eq!(roundtrip, diagnostics);
    }
}
