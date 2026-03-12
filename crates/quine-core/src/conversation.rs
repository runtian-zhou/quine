use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutput {
    pub success: bool,
    pub output: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Entry {
    UserMessage {
        content: String,
    },
    AssistantMessage {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    ToolExecution {
        tool_call_id: String,
        tool_name: String,
        arguments: Value,
        result: ToolOutput,
    },
}

#[derive(Debug, Clone)]
pub struct Conversation {
    pub entries: Vec<Entry>,
}

impl Conversation {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }
}
