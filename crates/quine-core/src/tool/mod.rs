pub mod ask_user;
pub mod bash;
pub mod find;
pub mod plan;
pub mod read;
pub mod recv_message;
pub mod send_message;
pub mod signal;
pub mod skill_template;
pub mod spawn;
pub mod subagent;
pub mod wait_child;
pub mod web_open;
pub mod web_search;
pub mod write;

use std::collections::HashMap;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream;
use quine_llm::{LlmEvent, LlmProvider, NoopWebProvider};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot, watch};

use crate::filesystem::SessionFilesystem;
use crate::python::PythonRuntime;
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum InteractionKind {
    /// Ask the user a free-form question.
    Question,
    /// Ask the user for confirmation (yes/no).
    Confirmation,
    /// Select exactly one option from a list.
    SingleSelect,
    /// Select one or more options from a list.
    MultiSelect,
}

/// An option in a selection list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectOption {
    /// Display label for the option.
    pub label: String,
    /// Optional description shown below the label.
    pub description: Option<String>,
}

/// A request for user interaction, sent from a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionRequest {
    /// The prompt to display to the user.
    pub prompt: String,
    /// The kind of interaction.
    pub kind: InteractionKind,
    /// Available options for SingleSelect/MultiSelect.
    #[serde(default)]
    pub options: Vec<SelectOption>,
    /// Whether to allow free-form input in addition to options.
    #[serde(default)]
    pub allow_freeform: bool,
    /// Label identifying the source of this interaction (e.g. "subagent: <task>").
    #[serde(default)]
    pub source_label: Option<String>,
}

/// The user's response to an interaction request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionResponse {
    /// The user's textual response.
    pub response: String,
    /// For MultiSelect: indices of selected options (0-based).
    #[serde(default)]
    pub selected_indices: Vec<usize>,
}

/// A channel for tools to request user interaction.
///
/// The tool sends an `InteractionRequest` and awaits an `InteractionResponse`.
#[derive(Clone)]
pub struct InteractionChannel {
    /// Send interaction requests out to the harness/CLI.
    pub(crate) request_tx: mpsc::Sender<(InteractionRequest, oneshot::Sender<InteractionResponse>)>,
}

impl InteractionChannel {
    /// Ask the user a question and wait for their response.
    pub async fn ask(
        &self,
        request: InteractionRequest,
        cancellation: &CancellationChannel,
    ) -> Result<InteractionResponse, ToolError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.request_tx
            .send((request, response_tx))
            .await
            .map_err(|_| ToolError::Internal {
                message: "interaction channel closed".into(),
            })?;

        tokio::select! {
            response = response_rx => response.map_err(|_| ToolError::Cancelled),
            _ = cancellation.cancelled() => Err(ToolError::Cancelled),
        }
    }
}

/// A cloneable per-tool cancellation channel.
#[derive(Clone)]
pub struct CancellationChannel {
    receiver: watch::Receiver<bool>,
    keepalive: Option<watch::Sender<bool>>,
}

impl CancellationChannel {
    /// Create a cancellation sender/receiver pair for a single tool execution.
    pub fn new_pair() -> (watch::Sender<bool>, Self) {
        let (sender, receiver) = watch::channel(false);
        (
            sender,
            Self {
                receiver,
                keepalive: None,
            },
        )
    }

    /// Create a channel that is never cancelled unless explicitly replaced.
    pub fn never() -> Self {
        let (sender, receiver) = watch::channel(false);
        Self {
            receiver,
            keepalive: Some(sender),
        }
    }

    /// Returns true once cancellation has been signaled.
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }

    /// Wait until cancellation is signaled.
    pub async fn cancelled(&self) {
        if *self.receiver.borrow() {
            return;
        }

        let mut receiver = self.receiver.clone();
        while receiver.changed().await.is_ok() {
            if *receiver.borrow() {
                return;
            }
        }

        let _ = &self.keepalive;
    }
}

/// Context provided to a tool during execution.
///
/// Cancellation is modeled as a per-execution channel instead of mutable
/// session flags. Tools can wait on `context.cancellation.cancelled()` to abort
/// immediately without relying on engine-owned deferred input state.
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
    /// The effective python session group for this session.
    pub session_group: String,
    /// Shared python runtime for session-scoped execution.
    pub python_runtime: Arc<PythonRuntime>,
    /// Sender for sending messages back to the core event loop (for spawn, signal, etc.).
    pub core_input: Option<mpsc::Sender<crate::channel::CoreInput>>,
    /// Per-execution cancellation channel for immediate tool aborts.
    pub cancellation: CancellationChannel,
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

    /// Whether this tool is safe to classify as read-only.
    fn is_read_only(&self) -> bool {
        false
    }

    /// Whether repeated executions with the same arguments are safe.
    fn is_idempotent(&self) -> bool {
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
                read_only: tool.is_read_only(),
                idempotent: tool.is_idempotent(),
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

struct NoopProvider;

#[async_trait]
impl LlmProvider for NoopProvider {
    async fn send(
        &self,
        _messages: &[quine_llm::Message],
        _tools: &[quine_llm::ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>> {
        Ok(Box::pin(stream::empty()))
    }
}

/// Build the built-in tool definitions for a session mode.
pub fn built_in_tool_definitions(plan_mode: bool) -> Vec<quine_llm::ToolDefinition> {
    let mut registry = ToolRegistry::new();
    registry.register(Arc::new(ask_user::AskUserTool));
    registry.register(Arc::new(bash::BashTool));
    registry.register(Arc::new(find::FindTool));
    registry.register(Arc::new(plan::PlanTool::new(plan::new_plan_store())));
    registry.register(Arc::new(read::ReadTool));
    registry.register(Arc::new(web_open::WebOpenTool::new(Arc::new(
        NoopWebProvider,
    ))));
    registry.register(Arc::new(web_search::WebSearchTool::new(Arc::new(
        NoopWebProvider,
    ))));
    if !plan_mode {
        registry.register(Arc::new(write::WriteTool));
        registry.register(Arc::new(subagent::SubagentTool::new(
            Arc::new(NoopProvider),
            Arc::new(NoopWebProvider),
        )));
        registry.register(Arc::new(spawn::SpawnTool));
        registry.register(Arc::new(wait_child::WaitChildTool));
        registry.register(Arc::new(signal::SignalTool));
        registry.register(Arc::new(send_message::SendMessageTool));
        registry.register(Arc::new(recv_message::RecvMessageTool));
    }
    registry.tool_definitions()
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
            fn is_read_only(&self) -> bool {
                true
            }
            fn is_idempotent(&self) -> bool {
                true
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
        assert!(defs[0].read_only);
        assert!(defs[0].idempotent);
    }

    #[test]
    fn built_in_tool_definitions_match_runtime_tool_names() {
        let defs = built_in_tool_definitions(false);
        assert!(defs.iter().any(|tool| tool.name == "apply_patch"));
        assert!(!defs.iter().any(|tool| tool.name == "write_file"));
        assert!(defs.iter().any(|tool| tool.name == "recv_message"));
        assert!(defs.iter().any(|tool| tool.name == "web_open"));
        assert!(defs.iter().any(|tool| tool.name == "web_search"));
    }

    #[test]
    fn interaction_request_serialization() {
        let req = InteractionRequest {
            prompt: "Continue?".into(),
            kind: InteractionKind::Confirmation,
            options: Vec::new(),
            allow_freeform: false,
            source_label: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: InteractionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.prompt, "Continue?");
    }

    #[test]
    fn interaction_request_with_options_serialization() {
        let req = InteractionRequest {
            prompt: "Pick one".into(),
            kind: InteractionKind::SingleSelect,
            options: vec![
                SelectOption {
                    label: "A".into(),
                    description: Some("Option A".into()),
                },
                SelectOption {
                    label: "B".into(),
                    description: None,
                },
            ],
            allow_freeform: true,
            source_label: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let de: InteractionRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(de.options.len(), 2);
        assert_eq!(de.options[0].label, "A");
        assert_eq!(de.kind, InteractionKind::SingleSelect);
        assert!(de.allow_freeform);
    }

    #[tokio::test]
    async fn cancellation_channel_never_stays_pending() {
        let cancellation = CancellationChannel::never();
        assert!(!cancellation.is_cancelled());

        let timed = tokio::time::timeout(
            std::time::Duration::from_millis(20),
            cancellation.cancelled(),
        )
        .await;
        assert!(
            timed.is_err(),
            "never() channel should not resolve without an explicit cancel"
        );
    }

    #[tokio::test]
    async fn cancellation_channel_pair_resolves_after_signal() {
        let (tx, cancellation) = CancellationChannel::new_pair();
        assert!(!cancellation.is_cancelled());
        tx.send(true).unwrap();
        cancellation.cancelled().await;
        assert!(cancellation.is_cancelled());
    }
}
