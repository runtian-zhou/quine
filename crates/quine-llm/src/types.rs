use serde::{Deserialize, Serialize};

/// Role of a message participant in the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

/// Content of a message, supporting text, tool results, and tool use requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    /// Plain text content.
    Text(String),
    /// Result from a tool invocation.
    ToolResult {
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
    /// Assistant requesting tool invocations (one or more).
    ToolUse {
        /// Optional text before tool calls.
        text: Option<String>,
        /// The tool calls requested by the assistant.
        tool_calls: Vec<ToolUseRequest>,
    },
}

/// A single tool use request from the assistant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolUseRequest {
    pub tool_use_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

/// A message in a conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: MessageContent,
}

impl Message {
    /// Create a system message.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: MessageContent::Text(text.into()),
        }
    }

    /// Create a user message.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: MessageContent::Text(text.into()),
        }
    }

    /// Create an assistant message.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::Text(text.into()),
        }
    }

    /// Create an assistant message requesting tool use.
    pub fn assistant_tool_use(
        text: Option<String>,
        tool_calls: Vec<ToolUseRequest>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: MessageContent::ToolUse { text, tool_calls },
        }
    }

    /// Create a tool result message.
    pub fn tool_result(
        tool_use_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self {
            role: Role::Tool,
            content: MessageContent::ToolResult {
                tool_use_id: tool_use_id.into(),
                output: output.into(),
                is_error,
            },
        }
    }
}

/// Definition of a tool the LLM can invoke.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Name of the tool.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema describing the tool's parameters.
    pub parameters: serde_json::Value,
}

/// Events streamed from the LLM provider.
#[derive(Debug, Clone)]
pub enum LlmEvent {
    /// A partial text token from the stream.
    TextDelta { text: String },
    /// The LLM is requesting a tool invocation.
    ToolCall {
        tool_use_id: String,
        tool_name: String,
        arguments: serde_json::Value,
    },
    /// The stream is complete.
    Done,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_constructors() {
        let msg = Message::user("hello");
        assert_eq!(msg.role, Role::User);
        match &msg.content {
            MessageContent::Text(t) => assert_eq!(t, "hello"),
            _ => panic!("expected text content"),
        }

        let msg = Message::system("you are helpful");
        assert_eq!(msg.role, Role::System);

        let msg = Message::assistant("sure");
        assert_eq!(msg.role, Role::Assistant);

        let msg = Message::tool_result("id-1", "result", false);
        assert_eq!(msg.role, Role::Tool);
        match &msg.content {
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                assert_eq!(tool_use_id, "id-1");
                assert_eq!(output, "result");
                assert!(!is_error);
            }
            _ => panic!("expected tool result content"),
        }
    }

    #[test]
    fn message_serialization_roundtrip() {
        let msg = Message::user("test message");
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: Message = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.role, Role::User);
    }

    #[test]
    fn tool_definition_serialization() {
        let tool = ToolDefinition {
            name: "read_file".into(),
            description: "Read a file from disk".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" }
                },
                "required": ["path"]
            }),
        };
        let json = serde_json::to_string(&tool).unwrap();
        let deserialized: ToolDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "read_file");
    }
}
