pub mod channel;
mod compaction;
pub mod engine;
pub mod error;
pub mod filesystem;
pub mod ipc;
mod memory;
pub mod persistence;
pub mod planner;
mod scheduler;
pub mod session;
pub mod session_tree;
pub mod skill;
pub mod tool;

pub use channel::{
    create_channels, ChannelConfig, CoreHandle, CoreInput, CoreOutput, HarnessHandle, ToolOutcome,
};
pub use engine::{run_core_loop, run_core_loop_with_compaction};
pub use error::CoreError;
pub use filesystem::{DirEntry, FsError, NullFilesystem, OverlayFilesystem, SessionFilesystem};
pub use persistence::{
    CoreCheckpoint, PersistedMemoryState, PersistedPersistentMemoryState, PersistedPlanStore,
    PersistedPromptMemoryState, PersistedSession, PersistedSessionConfig,
    PersistedSessionMemoryState, PersistedSessionState, PersistedSessionTree, PromptMemoryMode,
    CORE_CHECKPOINT_FORMAT_VERSION,
};
pub use session::{ExitStatus, InheritanceFlags, SessionId, SessionSignal, SessionState};
pub use skill::{
    default_skill_service, list_available_skills, load_skill, load_skills, DefaultSkillService,
    FileSystemSkillLoader, Skill, SkillLoader, SkillMeta, SkillService, SkillToolDef,
};
pub use tool::{
    built_in_tool_definitions, CancellationChannel, ExecutionContext, InteractionChannel,
    InteractionKind, InteractionRequest, InteractionResponse, SelectOption, Tool, ToolError,
    ToolOutput, ToolRegistry,
};
