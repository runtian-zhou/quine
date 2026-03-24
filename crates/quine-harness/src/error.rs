use serde::{Deserialize, Serialize};

/// Errors produced by the harness layer.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum HarnessError {
    /// Session with the given ID was not found.
    #[error("session not found: {session_id}")]
    SessionNotFound { session_id: String },

    /// Session creation failed.
    #[error("failed to create session: {reason}")]
    SessionCreationFailed { reason: String },

    /// The harness has already been shut down.
    #[error("harness is shut down")]
    ShutDown,

    /// Failed to send a message to the core event loop.
    #[error("core channel closed")]
    CoreChannelClosed,

    /// IPC protocol error.
    #[error("protocol error: {message}")]
    ProtocolError { message: String },

    /// Internal error.
    #[error("internal error: {message}")]
    Internal { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display() {
        let err = HarnessError::SessionNotFound {
            session_id: "abc".into(),
        };
        assert!(err.to_string().contains("abc"));
    }

    #[test]
    fn error_serialization_roundtrip() {
        let err = HarnessError::ShutDown;
        let json = serde_json::to_string(&err).unwrap();
        let deserialized: HarnessError = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.to_string(), err.to_string());
    }
}
