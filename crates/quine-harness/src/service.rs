use async_trait::async_trait;
use quine_core::{CoreOutput, InteractionResponse, SessionId, SessionSignal};
use tokio::sync::broadcast;

use crate::config::SessionConfig;
use crate::error::HarnessError;

/// Async trait defining the harness service interface.
///
/// This is the primary abstraction for managing agent sessions. The harness
/// creates sessions, forwards messages to the core event loop, and distributes
/// events to subscribers.
#[async_trait]
pub trait HarnessService: Send + Sync {
    /// Create a new agent session and return its ID.
    async fn create_session(&self, config: SessionConfig) -> Result<SessionId, HarnessError>;

    /// Send a user message into an existing session.
    async fn send_message(
        &self,
        session_id: SessionId,
        content: String,
    ) -> Result<(), HarnessError>;

    /// Submit the result of a tool invocation requested by the core.
    async fn submit_tool_result(
        &self,
        session_id: SessionId,
        tool_use_id: String,
        output: String,
        is_error: bool,
    ) -> Result<(), HarnessError>;

    /// Submit the user's response to an interaction request from a tool.
    async fn submit_interaction_response(
        &self,
        session_id: SessionId,
        response: InteractionResponse,
    ) -> Result<(), HarnessError>;

    /// Cancel any in-flight work for a session.
    async fn cancel(&self, session_id: SessionId) -> Result<(), HarnessError>;

    /// Gracefully shut down the harness and all sessions.
    async fn shutdown(&self) -> Result<(), HarnessError>;

    /// Subscribe to events from the harness.
    ///
    /// Returns a broadcast receiver that yields `CoreOutput` events.
    fn subscribe(&self) -> broadcast::Receiver<CoreOutput>;

    /// List all active sessions with metadata.
    async fn list_sessions(&self) -> Result<Vec<serde_json::Value>, HarnessError> {
        Err(HarnessError::Internal {
            message: "not implemented".into(),
        })
    }

    /// Spawn a child session under an optional parent.
    async fn spawn_child_session(
        &self,
        _parent_id: Option<SessionId>,
        _task: String,
        _system_prompt: Option<String>,
    ) -> Result<SessionId, HarnessError> {
        Err(HarnessError::Internal {
            message: "not implemented".into(),
        })
    }

    /// Send a signal to a session.
    async fn signal_session(
        &self,
        _session_id: SessionId,
        _signal: SessionSignal,
    ) -> Result<(), HarnessError> {
        Err(HarnessError::Internal {
            message: "not implemented".into(),
        })
    }

    /// Send an IPC message to a target session.
    async fn send_ipc_message(
        &self,
        _target: String,
        _content: String,
    ) -> Result<(), HarnessError> {
        Err(HarnessError::Internal {
            message: "not implemented".into(),
        })
    }

    /// Receive an IPC message from a source session.
    async fn recv_ipc_message(
        &self,
        _source: String,
        _non_blocking: bool,
    ) -> Result<Option<String>, HarnessError> {
        Err(HarnessError::Internal {
            message: "not implemented".into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that HarnessService is object-safe.
    fn _assert_object_safe(_: &dyn HarnessService) {}
}
