use serde::{Deserialize, Serialize};

use crate::session::SessionState;

/// Errors the core can report through the output channel.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum CoreError {
    /// LLM provider returned an error.
    #[error("LLM error: {message}")]
    LlmError { message: String },

    /// The referenced session does not exist.
    #[error("session not found")]
    SessionNotFound,

    /// An invalid operation for the current session state.
    #[error("invalid state: expected {expected:?}, got {actual:?}")]
    InvalidState {
        expected: SessionState,
        actual: SessionState,
    },

    /// A tool result arrived for an unknown tool_use_id.
    #[error("unknown tool_use_id: {tool_use_id}")]
    UnknownToolUseId { tool_use_id: String },

    /// Internal error.
    #[error("internal error: {message}")]
    Internal { message: String },
}
