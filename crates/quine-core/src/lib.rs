pub mod channel;
pub mod engine;
pub mod error;
pub mod filesystem;
pub mod ipc;
pub mod permission;
pub mod planner;
pub mod session;
pub mod session_tree;
pub mod tool;

pub use channel::{
    create_channels, ChannelConfig, CoreHandle, CoreInput, CoreOutput, HarnessHandle, ToolOutcome,
};
pub use engine::run_core_loop;
pub use error::CoreError;
pub use filesystem::{DirEntry, FsError, NullFilesystem, OverlayFilesystem, SessionFilesystem};
pub use permission::{PermissionChecker, PermissionContext, PermissionDecision, PermissionError};
pub use session::{ExitStatus, InheritanceFlags, SessionId, SessionSignal, SessionState};
pub use tool::{
    ExecutionContext, InteractionChannel, InteractionKind, InteractionRequest, InteractionResponse,
    SelectOption, Tool, ToolError, ToolOutput, ToolRegistry,
};
