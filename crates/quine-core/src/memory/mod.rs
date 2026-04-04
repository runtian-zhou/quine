mod diagnostics;
mod persistent;
mod prompt;
mod scopes;
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
pub use scopes::{
    authorize_memory_read, authorize_memory_write, build_memory_permission_context,
    compare_scope_priority, project_key, resolve_project_root, resolve_scoped_memory_paths,
    snapshot_scoped_persistent_memory_state, workspace_is_trusted, MemoryAuthorizationReason,
    MemoryConflictResolution, MemoryFeatureFlags, MemoryPermissionContext, MemoryPolicyConfig,
    MemoryReadPolicy, MemoryWritePolicy, PersistentMemoryScope, ScopeSelector,
    ScopedMemoryLookupOrder, ScopedMemoryPaths, ScopedMemoryResolution,
    ScopedPersistentMemoryState,
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
