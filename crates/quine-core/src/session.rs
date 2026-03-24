use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque identifier for an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

/// The lifecycle state of an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    /// Session created, waiting for first user message.
    Idle,
    /// Core is streaming a response from the LLM.
    Streaming,
    /// Waiting for the harness to return tool results.
    AwaitingToolResult,
    /// Session is paused.
    Paused,
    /// Session has been destroyed.
    Destroyed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_uniqueness() {
        let a = SessionId::new();
        let b = SessionId::new();
        assert_ne!(a, b);
    }

    #[test]
    fn session_id_equality() {
        let a = SessionId::new();
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn session_id_default() {
        let a = SessionId::default();
        let b = SessionId::default();
        assert_ne!(a, b);
    }
}
