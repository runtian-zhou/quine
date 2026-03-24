use std::pin::Pin;

use async_trait::async_trait;
use futures::Stream;

use crate::types::{LlmEvent, Message, ToolDefinition};

/// Trait abstracting over any LLM backend.
///
/// Implementations handle the specifics of communicating with a particular
/// LLM API (Anthropic, OpenAI-compatible, etc.) and return a stream of
/// [`LlmEvent`]s.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Send a conversation to the LLM and receive a stream of events.
    ///
    /// # Arguments
    /// * `messages` - The conversation history.
    /// * `tools` - Tool definitions the LLM may invoke.
    ///
    /// # Returns
    /// A stream of [`LlmEvent`]s representing the LLM's response.
    async fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<LlmEvent>> + Send>>>;
}
