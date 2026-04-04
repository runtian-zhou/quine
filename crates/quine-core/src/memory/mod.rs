mod diagnostics;
mod persistent;
mod prompt;
mod session;
mod summary;
mod template;

pub(crate) use diagnostics::default_turn_diagnostics;
pub use diagnostics::{
    CompactionSourceDiagnostics, MemoryDecisionReason, MemorySelectionEntryDiagnostics,
    MemorySkippedEntryDiagnostics, MemoryStatus, MemoryTurnDiagnostics,
    PersistentExtractionDiagnostics, PersistentMemoryDiagnostics, PromptMemoryDiagnostics,
    SessionMemoryDiagnostics, SessionRefreshDiagnostics,
};
pub(crate) use persistent::MemoryDiagnostics;
pub(crate) use prompt::{
    build_prompt_memory_injection, project_root_for_prompt_memory, splice_prompt_memory_messages,
};
pub(crate) use session::{
    load_compaction_snapshot, restore_memory_state, snapshot_memory_state,
    SessionMemoryCompactionSnapshot, SessionMemoryState,
};
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use session::{session_memory_paths, SESSION_MEMORY_TEMPLATE_VERSION};
#[cfg(test)]
pub(crate) use summary::{load_summary_metadata, SessionSummaryMetadata};
pub(crate) use summary::{refresh_summary_from_history, should_refresh_summary};
