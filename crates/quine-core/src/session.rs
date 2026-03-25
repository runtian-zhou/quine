use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Opaque identifier for an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create a SessionId from an existing UUID.
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
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

/// Controls what a child session inherits from its parent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InheritanceFlags {
    pub history: bool,
    pub filesystem: bool,
    pub tools: bool,
    pub working_directory: bool,
}

impl Default for InheritanceFlags {
    fn default() -> Self {
        Self {
            history: false,
            filesystem: true,
            tools: true,
            working_directory: true,
        }
    }
}

/// Signals that can be sent to an agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionSignal {
    Term,
    Kill,
    Stop,
    Continue,
}

/// Exit status of a completed child session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExitStatus {
    Success { output: String },
    Failed { error: String },
    Killed,
    Cancelled,
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

    #[test]
    fn inheritance_flags_default() {
        let flags = InheritanceFlags::default();
        assert!(!flags.history);
        assert!(flags.filesystem);
        assert!(flags.tools);
        assert!(flags.working_directory);
    }

    #[test]
    fn inheritance_flags_custom() {
        let flags = InheritanceFlags {
            history: true,
            filesystem: false,
            tools: false,
            working_directory: false,
        };
        assert!(flags.history);
        assert!(!flags.filesystem);
        assert!(!flags.tools);
        assert!(!flags.working_directory);
    }

    #[test]
    fn session_signal_equality() {
        assert_eq!(SessionSignal::Term, SessionSignal::Term);
        assert_ne!(SessionSignal::Term, SessionSignal::Kill);
        assert_ne!(SessionSignal::Stop, SessionSignal::Continue);
    }

    #[test]
    fn exit_status_success() {
        let status = ExitStatus::Success {
            output: "done".into(),
        };
        match status {
            ExitStatus::Success { output } => assert_eq!(output, "done"),
            _ => panic!("expected Success"),
        }
    }

    #[test]
    fn exit_status_failed() {
        let status = ExitStatus::Failed {
            error: "oops".into(),
        };
        match status {
            ExitStatus::Failed { error } => assert_eq!(error, "oops"),
            _ => panic!("expected Failed"),
        }
    }

    #[test]
    fn exit_status_killed_and_cancelled() {
        let killed = ExitStatus::Killed;
        let cancelled = ExitStatus::Cancelled;
        assert!(matches!(killed, ExitStatus::Killed));
        assert!(matches!(cancelled, ExitStatus::Cancelled));
    }

    #[test]
    fn inheritance_flags_serde_roundtrip() {
        let flags = InheritanceFlags::default();
        let json = serde_json::to_string(&flags).unwrap();
        let deserialized: InheritanceFlags = serde_json::from_str(&json).unwrap();
        assert_eq!(flags.history, deserialized.history);
        assert_eq!(flags.filesystem, deserialized.filesystem);
        assert_eq!(flags.tools, deserialized.tools);
        assert_eq!(flags.working_directory, deserialized.working_directory);
    }

    #[test]
    fn session_signal_serde_roundtrip() {
        let signal = SessionSignal::Stop;
        let json = serde_json::to_string(&signal).unwrap();
        let deserialized: SessionSignal = serde_json::from_str(&json).unwrap();
        assert_eq!(signal, deserialized);
    }

    #[test]
    fn exit_status_serde_roundtrip() {
        let status = ExitStatus::Success {
            output: "hello".into(),
        };
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: ExitStatus = serde_json::from_str(&json).unwrap();
        match deserialized {
            ExitStatus::Success { output } => assert_eq!(output, "hello"),
            _ => panic!("expected Success"),
        }
    }
}
