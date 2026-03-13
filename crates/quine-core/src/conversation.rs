use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm_types::{ChatContent, ChatMessage, ContentBlock};

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

impl Default for Conversation {
    fn default() -> Self {
        Self::new()
    }
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

/// Convert conversation log entries into ChatMessage list for the LLM.
pub fn entries_to_messages(entries: &[Entry]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    for entry in entries {
        match entry {
            Entry::ToolExecution {
                tool_call_id,
                result,
                ..
            } => {
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: result.output.clone(),
                });
            }
            other => {
                if !pending_tool_results.is_empty() {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: ChatContent::Blocks(std::mem::take(&mut pending_tool_results)),
                    });
                }
                match other {
                    Entry::UserMessage { content } => {
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: ChatContent::Text(content.clone()),
                        });
                    }
                    Entry::AssistantMessage {
                        content,
                        tool_calls,
                    } => {
                        let mut blocks = Vec::new();
                        if !content.is_empty() {
                            blocks.push(ContentBlock::Text {
                                text: content.clone(),
                            });
                        }
                        for tc in tool_calls {
                            blocks.push(ContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: if blocks.len() == 1
                                && matches!(&blocks[0], ContentBlock::Text { .. })
                            {
                                ChatContent::Text(content.clone())
                            } else {
                                ChatContent::Blocks(blocks)
                            },
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    // Flush any remaining tool results
    if !pending_tool_results.is_empty() {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(pending_tool_results),
        });
    }
    messages
}
