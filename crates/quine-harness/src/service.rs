use async_trait::async_trait;
use quine_core::{CoreOutput, SessionId};
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

    /// Cancel any in-flight work for a session.
    async fn cancel(&self, session_id: SessionId) -> Result<(), HarnessError>;

    /// Gracefully shut down the harness and all sessions.
    async fn shutdown(&self) -> Result<(), HarnessError>;

    /// Subscribe to events from the harness.
    ///
    /// Returns a broadcast receiver that yields `CoreOutput` events.
    fn subscribe(&self) -> broadcast::Receiver<CoreOutput>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that HarnessService is object-safe.
    fn _assert_object_safe(_: &dyn HarnessService) {}
}
