use crate::agent::AgentId;
use quine_llm::types::{CompletionResponse, StreamEvent};

/// Events flow through a single mpsc channel into the Dispatcher.
pub enum Event {
    /// User typed a line of input.
    UserInput(String),

    /// An LLM call completed (non-streaming).
    LlmResponse {
        agent_id: AgentId,
        response: CompletionResponse,
    },

    /// A streaming chunk arrived from the LLM.
    StreamChunk {
        agent_id: AgentId,
        event: StreamEvent,
    },

    /// An LLM call failed.
    LlmError {
        agent_id: AgentId,
        error: String,
    },

    /// Shut down the event loop.
    Shutdown,
}
