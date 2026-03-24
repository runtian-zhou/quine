pub mod channel;
pub mod engine;
pub mod error;
pub mod session;

pub use channel::{
    create_channels, ChannelConfig, CoreHandle, CoreInput, CoreOutput, HarnessHandle, ToolOutcome,
};
pub use engine::run_core_loop;
pub use error::CoreError;
pub use session::{SessionId, SessionState};
