pub mod channel;
mod compaction;
pub mod engine;
pub mod error;
pub mod filesystem;
pub mod ipc;
pub mod memory;
pub mod permission;
pub mod persistence;
pub mod planner;
pub mod python;
mod scheduler;
pub mod session;
pub mod session_tree;
pub mod skill;
pub mod status_report;
pub mod tool;

pub use channel::{
    create_channels, ChannelConfig, CoreHandle, CoreInput, CoreOutput, HarnessHandle,
    SessionLlmConfig, ToolOutcome,
};
pub use engine::{
    run_core_loop, run_core_loop_with_compaction, run_core_loop_with_compaction_and_web_provider,
    run_core_loop_with_compaction_and_web_provider_and_python_runtime,
};
pub use error::CoreError;
pub use filesystem::{DirEntry, FsError, NullFilesystem, OverlayFilesystem, SessionFilesystem};
pub use memory::{
    authorize_memory_read, authorize_memory_write, build_memory_permission_context,
    resolve_scoped_memory_paths, workspace_is_trusted, CompactionSourceDiagnostics,
    MemoryAuthorizationReason, MemoryConflictResolution, MemoryDecisionReason, MemoryFeatureFlags,
    MemoryPermissionContext, MemoryPolicyConfig, MemoryReadPolicy, MemorySelectionEntryDiagnostics,
    MemorySkippedEntryDiagnostics, MemoryStatus, MemoryTurnDiagnostics, MemoryWritePolicy,
    PersistentExtractionDiagnostics, PersistentMemoryDiagnostics, PersistentMemoryScope,
    PromptMemoryDiagnostics, ScopeSelector, ScopedMemoryLookupOrder, ScopedMemoryPaths,
    ScopedPersistentMemoryState, SessionMemoryDiagnostics, SessionRefreshDiagnostics,
};
pub use permission::PermissionPromptBehavior;
pub use permission::{
    ApprovalRequestId, CommandDescriptor, CommandRisk, MatchedPermissionSource,
    PendingPermissionApproval, PermissionDecision, PermissionMatchKind, PermissionMode,
    PermissionOutcome, PermissionOutcomeKind, PermissionRequest, PermissionResource,
    PermissionRule, PermissionRuleEffect, PermissionRuleSet, PermissionRuleSource,
    PermissionRuntimeSnapshot, PermissionScope, PermissionTarget, RuleScope, ToolLocalDecision,
};
pub use persistence::{
    CoreCheckpoint, PersistedMemoryState, PersistedPersistentMemoryState, PersistedPlanStore,
    PersistedPromptMemoryState, PersistedSession, PersistedSessionConfig,
    PersistedSessionMemoryState, PersistedSessionState, PersistedSessionTree, PromptMemoryMode,
    CORE_CHECKPOINT_FORMAT_VERSION,
};
pub use python::{
    PersistedPythonState, PythonExecRequest, PythonExecResult, PythonInspectResult,
    PythonListGlobalsResult, PythonMethodSummary, PythonRuntime, PythonRuntimeError,
    PythonSymbolSummary,
};
pub use session::{ExitStatus, InheritanceFlags, SessionId, SessionSignal, SessionState};
pub use skill::{
    default_skill_service, list_available_skills, load_skill, load_skills, DefaultSkillService,
    FileSystemSkillLoader, Skill, SkillLoader, SkillMeta, SkillService, SkillToolDef,
};
pub use status_report::{
    default_status_report_min_tool_rounds, SessionStatusReport,
    DEFAULT_STATUS_REPORT_MIN_TOOL_ROUNDS,
};
pub use tool::{
    built_in_tool_definitions, CancellationChannel, ExecutionContext, InteractionChannel,
    InteractionKind, InteractionRequest, InteractionResponse, SelectOption, Tool, ToolError,
    ToolOutput, ToolRegistry,
};
