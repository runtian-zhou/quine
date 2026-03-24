pub mod channel;
pub mod engine;
pub mod error;
pub mod filesystem;
pub mod session;
pub mod tool;

pub use channel::{
    create_channels, ChannelConfig, CoreHandle, CoreInput, CoreOutput, HarnessHandle, ToolOutcome,
};
pub use engine::run_core_loop;
pub use error::CoreError;
pub use filesystem::{DirEntry, FsError, OverlayFilesystem, SessionFilesystem};
pub use session::{SessionId, SessionState};
pub use tool::{
    ExecutionContext, InteractionChannel, InteractionKind, InteractionRequest, InteractionResponse,
    Tool, ToolError, ToolOutput, ToolRegistry,
};
