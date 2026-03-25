pub mod ask_user;
pub mod bash;
pub mod plan;
pub mod read;
pub mod subagent;
pub mod write;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};

use crate::filesystem::SessionFilesystem;
use crate::session::SessionId;

/// Errors from tool execution.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum ToolError {
    /// The tool's input arguments were invalid.
    #[error("invalid arguments: {message}")]
    InvalidArguments { message: String },

    /// A filesystem error occurred during tool execution.
    #[error("filesystem error: {message}")]
    FilesystemError { message: String },

    /// The tool execution timed out.
    #[error("execution timed out after {seconds}s")]
    Timeout { seconds: u64 },

    /// The tool was cancelled (e.g., user denied permission).
    #[error("tool execution cancelled")]
    Cancelled,

    /// The tool execution was denied by the permission checker.
    #[error("permission denied: {reason}")]
    PermissionDenied { reason: String },

    /// An internal error occurred.
    #[error("internal error: {message}")]
    Internal { message: String },
}

/// The output of a successful tool execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    /// The textual output to show the LLM.
    pub content: String,
    /// Whether this output represents an error condition.
    pub is_error: bool,
}

impl ToolOutput {
    /// Create a successful output.
    pub fn success(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    /// Create an error output (tool ran but reported a problem).
    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// The kind of interaction requested from the user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum InteractionKind {
    /// Ask the user a free-form question.
    Question,
    /// Ask the user for confirmation (yes/no).
    Confirmation,
}

/// A request for user interaction, sent from a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRequest {
    /// The prompt to display to the user.
    pub prompt: String,
    /// The kind of interaction.
    pub kind: InteractionKind,
}

/// The user's response to an interaction request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionResponse {
    /// The user's textual response.
    pub response: String,
}

/// A channel for tools to request user interaction.
///
/// The tool sends an `InteractionRequest` and awaits an `InteractionResponse`.
pub struct InteractionChannel {
    /// Send interaction requests out to the harness/CLI.
    pub(crate) request_tx: mpsc::Sender<(InteractionRequest, oneshot::Sender<InteractionResponse>)>,
}

impl InteractionChannel {
    /// Ask the user a question and wait for their response.
    pub async fn ask(&self, request: InteractionRequest) -> Result<InteractionResponse, ToolError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.request_tx
            .send((request, response_tx))
            .await
            .map_err(|_| ToolError::Internal {
                message: "interaction channel closed".into(),
            })?;
        response_rx.await.map_err(|_| ToolError::Cancelled)
    }
}

/// Context provided to a tool during execution.
pub struct ExecutionContext {
    /// The session this tool is executing within.
    pub session_id: SessionId,
    /// The filesystem for this session.
    pub filesystem: Arc<dyn SessionFilesystem>,
    /// The working directory for this tool execution.
    pub working_directory: PathBuf,
    /// Channel for requesting user interaction (available for interactive tools).
    pub interaction_channel: Option<InteractionChannel>,
    /// Shared plan store for this session.
    pub plan_store: crate::tool::plan::PlanStore,
}

/// Trait for a tool that the agent can invoke.
///
/// Each tool has a name, description, parameter schema, and an async
/// execute method.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The unique name of this tool.
    fn name(&self) -> &str;

    /// A human-readable description of what this tool does.
    fn description(&self) -> &str;

    /// JSON Schema for the tool's parameters.
    fn parameters_schema(&self) -> serde_json::Value;

    /// Whether this tool requires user interaction during execution.
    fn is_interactive(&self) -> bool {
        false
    }

    /// Execute the tool with the given arguments and context.
    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError>;
}

/// Registry of available tools for a session.
pub struct ToolRegistry {
    tools: HashMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty tool registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Generate `ToolDefinition` list for the LLM.
    pub fn tool_definitions(&self) -> Vec<quine_llm::ToolDefinition> {
        let mut defs: Vec<quine_llm::ToolDefinition> = self
            .tools
            .values()
            .map(|tool| quine_llm::ToolDefinition {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                parameters: tool.parameters_schema(),
            })
            .collect();
        defs.sort_by(|a, b| a.name.cmp(&b.name));
        defs
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_output_success() {
        let output = ToolOutput::success("hello");
        assert_eq!(output.content, "hello");
        assert!(!output.is_error);
    }

    #[test]
    fn tool_output_error() {
        let output = ToolOutput::error("bad");
        assert_eq!(output.content, "bad");
        assert!(output.is_error);
    }

    #[test]
    fn tool_error_display() {
        let err = ToolError::Timeout { seconds: 30 };
        assert!(err.to_string().contains("30"));
    }

    #[test]
    fn registry_register_and_get() {
        struct DummyTool;

        #[async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str {
                "dummy"
            }
            fn description(&self) -> &str {
                "A dummy tool"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _arguments: serde_json::Value,
                _context: &ExecutionContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success("ok"))
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool));
        assert!(registry.get("dummy").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_tool_definitions() {
        struct DummyTool;

        #[async_trait]
        impl Tool for DummyTool {
            fn name(&self) -> &str {
                "dummy"
            }
            fn description(&self) -> &str {
                "desc"
            }
            fn parameters_schema(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }
            async fn execute(
                &self,
                _arguments: serde_json::Value,
                _context: &ExecutionContext,
            ) -> Result<ToolOutput, ToolError> {
                Ok(ToolOutput::success("ok"))
            }
        }

        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(DummyTool));
        let defs = registry.tool_definitions();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name, "dummy");
    }

    #[test]
    fn interaction_request_serialization() {
        let req = InteractionRequest {
            prompt: "Continue?".into(),
            kind: InteractionKind::Confirmation,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: InteractionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.prompt, "Continue?");
    }
}
