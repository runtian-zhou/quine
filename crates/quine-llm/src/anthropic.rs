use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::types::{LlmEvent, Message, MessageContent, Role, ToolDefinition};

/// Configuration for the Anthropic Messages API.
#[derive(Debug, Clone)]
pub struct AnthropicConfig {
    /// API key. Defaults to `ANTHROPIC_API_KEY` env var.
    pub api_key: String,
    /// Base URL. Defaults to `https://api.anthropic.com`.
    pub base_url: String,
    /// Model identifier (e.g., `claude-sonnet-4-20250514`).
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
}

impl AnthropicConfig {
    /// Create a config from environment variables and defaults.
    ///
    /// Reads `ANTHROPIC_API_KEY` and optionally `ANTHROPIC_BASE_URL`.
    pub fn from_env(model: impl Into<String>) -> anyhow::Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| LlmError::InvalidConfig {
            message: "ANTHROPIC_API_KEY not set".into(),
        })?;
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        Ok(Self {
            api_key,
            base_url,
            model: model.into(),
            max_tokens: 4096,
        })
    }
}

/// An LLM provider adapter for the Anthropic Messages API.
pub struct AnthropicProvider {
    config: AnthropicConfig,
    client: Client,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }
}

// --- Request types ---

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<AnthropicTool>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: AnthropicContent,
}

#[derive(Serialize)]
#[serde(untagged)]
enum AnthropicContent {
    Text(String),
    Blocks(Vec<AnthropicContentBlock>),
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "std::ops::Not::not")]
        is_error: bool,
    },
}

#[derive(Serialize)]
struct AnthropicTool {
    name: String,
    description: String,
    input_schema: serde_json::Value,
}

// --- Response / SSE types ---

#[derive(Deserialize)]
struct SseEvent {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    content_block: Option<ContentBlockStart>,
    #[serde(default)]
    delta: Option<DeltaPayload>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ContentBlockStart {
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String },
    #[serde(rename = "text")]
    Text {},
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum DeltaPayload {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

/// Tracks in-flight tool calls as their arguments stream in.
struct ToolCallTracker {
    calls: Vec<TrackedToolCall>,
    /// Maps content block index to tool call index in `calls`.
    block_to_tool: std::collections::HashMap<usize, usize>,
}

struct TrackedToolCall {
    id: String,
    name: String,
    arguments_json: String,
}

impl ToolCallTracker {
    fn new() -> Self {
        Self {
            calls: Vec::new(),
            block_to_tool: std::collections::HashMap::new(),
        }
    }

    fn start_tool(&mut self, block_index: usize, id: String, name: String) {
        let tool_index = self.calls.len();
        self.block_to_tool.insert(block_index, tool_index);
        self.calls.push(TrackedToolCall {
            id,
            name,
            arguments_json: String::new(),
        });
    }

    fn append_arguments(&mut self, block_index: usize, json: &str) {
        if let Some(&tool_index) = self.block_to_tool.get(&block_index) {
            if let Some(call) = self.calls.get_mut(tool_index) {
                call.arguments_json.push_str(json);
            }
        }
    }

    fn take_completed(self) -> Vec<LlmEvent> {
        self.calls
            .into_iter()
            .map(|c| {
                let arguments = serde_json::from_str(&c.arguments_json)
                    .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));
                LlmEvent::ToolCall {
                    tool_use_id: c.id,
                    tool_name: c.name,
                    arguments,
                }
            })
            .collect()
    }
}

fn convert_messages(messages: &[Message]) -> (Option<String>, Vec<AnthropicMessage>) {
    let mut system = None;
    let mut api_messages = Vec::new();

    for msg in messages {
        match msg.role {
            Role::System => {
                if let MessageContent::Text(text) = &msg.content {
                    system = Some(text.clone());
                }
            }
            Role::User => {
                if let MessageContent::Text(text) = &msg.content {
                    api_messages.push(AnthropicMessage {
                        role: "user".into(),
                        content: AnthropicContent::Text(text.clone()),
                    });
                }
            }
            Role::Assistant => match &msg.content {
                MessageContent::Text(text) => {
                    api_messages.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: AnthropicContent::Text(text.clone()),
                    });
                }
                MessageContent::ToolUse { text, tool_calls } => {
                    let mut blocks = Vec::new();
                    if let Some(text) = text {
                        blocks.push(AnthropicContentBlock::Text { text: text.clone() });
                    }
                    for tc in tool_calls {
                        blocks.push(AnthropicContentBlock::ToolUse {
                            id: tc.tool_use_id.clone(),
                            name: tc.tool_name.clone(),
                            input: tc.arguments.clone(),
                        });
                    }
                    api_messages.push(AnthropicMessage {
                        role: "assistant".into(),
                        content: AnthropicContent::Blocks(blocks),
                    });
                }
                _ => {}
            },
            Role::Tool => {
                if let MessageContent::ToolResult {
                    tool_use_id,
                    output,
                    is_error,
                } = &msg.content
                {
                    let block = AnthropicContentBlock::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        content: output.clone(),
                        is_error: *is_error,
                    };
                    // Merge consecutive tool results into a single "user" message
                    // to satisfy the Anthropic API's alternating-roles requirement.
                    if let Some(last) = api_messages.last_mut() {
                        if last.role == "user" {
                            if let AnthropicContent::Blocks(ref mut blocks) = last.content {
                                if blocks
                                    .iter()
                                    .all(|b| matches!(b, AnthropicContentBlock::ToolResult { .. }))
                                {
                                    blocks.push(block);
                                    continue;
                                }
                            }
                        }
                    }
                    api_messages.push(AnthropicMessage {
                        role: "user".into(),
                        content: AnthropicContent::Blocks(vec![block]),
                    });
                }
            }
        }
    }

    (system, api_messages)
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<AnthropicTool> {
    tools
        .iter()
        .map(|t| AnthropicTool {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.parameters.clone(),
        })
        .collect()
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<LlmEvent>> + Send>>> {
        let url = format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'));

        let (system, api_messages) = convert_messages(messages);

        let request_body = AnthropicRequest {
            model: self.config.model.clone(),
            max_tokens: self.config.max_tokens,
            system,
            messages: api_messages,
            stream: true,
            tools: convert_tools(tools),
        };

        let debug = std::env::var("QUINE_DEBUG").is_ok();
        if debug {
            if let Ok(debug_json) = serde_json::to_string_pretty(&request_body) {
                eprintln!("[anthropic] request body:\n{debug_json}");
            }
        }

        let response = self
            .client
            .post(&url)
            .header("x-api-key", &self.config.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&request_body)
            .send()
            .await
            .map_err(LlmError::from)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            if debug {
                eprintln!("[anthropic] error response: {body}");
            }
            return Err(LlmError::ProviderHttp { status, body }.into());
        }

        let byte_stream = response.bytes_stream();

        let event_stream = stream::unfold(
            (byte_stream, String::new(), ToolCallTracker::new(), false),
            |(mut byte_stream, mut buffer, mut tool_tracker, mut done)| async move {
                if done {
                    return None;
                }

                loop {
                    // Try to extract SSE events from the buffer
                    // Anthropic SSE format: "event: <type>\ndata: <json>\n\n"
                    if let Some(double_newline) = buffer.find("\n\n") {
                        let block = buffer[..double_newline].to_string();
                        buffer = buffer[double_newline + 2..].to_string();

                        let mut event_type = String::new();
                        let mut data = String::new();

                        for line in block.lines() {
                            if let Some(et) = line.strip_prefix("event: ") {
                                event_type = et.trim().to_string();
                            } else if let Some(d) = line.strip_prefix("data: ") {
                                data = d.trim().to_string();
                            }
                        }

                        if event_type.is_empty() || data.is_empty() {
                            continue;
                        }

                        match event_type.as_str() {
                            "content_block_start" => {
                                if let Ok(sse) = serde_json::from_str::<SseEvent>(&data) {
                                    if let Some(ContentBlockStart::ToolUse { id, name }) =
                                        sse.content_block
                                    {
                                        let block_index = sse.index.unwrap_or(0);
                                        tool_tracker.start_tool(block_index, id, name);
                                    }
                                }
                                continue;
                            }
                            "content_block_delta" => {
                                if let Ok(sse) = serde_json::from_str::<SseEvent>(&data) {
                                    match sse.delta {
                                        Some(DeltaPayload::TextDelta { text }) => {
                                            if !text.is_empty() {
                                                return Some((
                                                    stream::iter(vec![Ok(LlmEvent::TextDelta {
                                                        text,
                                                    })]),
                                                    (byte_stream, buffer, tool_tracker, done),
                                                ));
                                            }
                                        }
                                        Some(DeltaPayload::InputJsonDelta { partial_json }) => {
                                            if let Some(block_index) = sse.index {
                                                tool_tracker
                                                    .append_arguments(block_index, &partial_json);
                                            }
                                        }
                                        None => {}
                                    }
                                }
                                continue;
                            }
                            "message_stop" => {
                                let mut events: Vec<anyhow::Result<LlmEvent>> =
                                    tool_tracker.take_completed().into_iter().map(Ok).collect();
                                events.push(Ok(LlmEvent::Done));
                                done = true;
                                return Some((
                                    stream::iter(events),
                                    (byte_stream, buffer, ToolCallTracker::new(), done),
                                ));
                            }
                            _ => continue,
                        }
                    }

                    // Need more data
                    use futures::StreamExt;
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Some(Err(e)) => {
                            return Some((
                                stream::iter(vec![Err(e.into())]),
                                (byte_stream, buffer, tool_tracker, done),
                            ));
                        }
                        None => {
                            let mut events: Vec<anyhow::Result<LlmEvent>> =
                                tool_tracker.take_completed().into_iter().map(Ok).collect();
                            events.push(Ok(LlmEvent::Done));
                            done = true;
                            return Some((
                                stream::iter(events),
                                (byte_stream, buffer, ToolCallTracker::new(), done),
                            ));
                        }
                    }
                }
            },
        )
        .flatten();

        Ok(Box::pin(event_stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn convert_messages_extracts_system() {
        let messages = vec![Message::system("be helpful"), Message::user("hello")];
        let (system, api_msgs) = convert_messages(&messages);
        assert_eq!(system, Some("be helpful".into()));
        assert_eq!(api_msgs.len(), 1);
        assert_eq!(api_msgs[0].role, "user");
    }

    #[test]
    fn convert_messages_tool_result() {
        let messages = vec![
            Message::user("hello"),
            Message::tool_result("id-1", "result text", false),
        ];
        let (_, api_msgs) = convert_messages(&messages);
        assert_eq!(api_msgs.len(), 2);
        // Tool results become "user" role in Anthropic API
        assert_eq!(api_msgs[1].role, "user");
    }

    #[test]
    fn convert_messages_batches_consecutive_tool_results() {
        let messages = vec![
            Message::user("do two things"),
            Message::assistant_tool_use(
                None,
                vec![
                    crate::types::ToolUseRequest {
                        tool_use_id: "tc_1".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo hello"}),
                    },
                    crate::types::ToolUseRequest {
                        tool_use_id: "tc_2".into(),
                        tool_name: "read_file".into(),
                        arguments: serde_json::json!({"path": "Cargo.toml"}),
                    },
                ],
            ),
            Message::tool_result("tc_1", "hello", false),
            Message::tool_result("tc_2", "[workspace]", false),
        ];
        let (_, api_msgs) = convert_messages(&messages);
        // user, assistant, user (batched tool results) — 3 messages, not 4
        assert_eq!(
            api_msgs.len(),
            3,
            "consecutive tool results should be batched into one user message"
        );
        assert_eq!(api_msgs[2].role, "user");
        match &api_msgs[2].content {
            AnthropicContent::Blocks(blocks) => {
                assert_eq!(
                    blocks.len(),
                    2,
                    "batched message should contain 2 tool_result blocks"
                );
            }
            _ => panic!("expected Blocks content for batched tool results"),
        }
    }

    #[test]
    fn tool_call_tracker() {
        let mut tracker = ToolCallTracker::new();
        // block_index=1 (e.g., after a text block at index 0)
        tracker.start_tool(1, "id-1".into(), "test_tool".into());
        tracker.append_arguments(1, "{\"key\":");
        tracker.append_arguments(1, "\"value\"}");

        let events = tracker.take_completed();
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(tool_use_id, "id-1");
                assert_eq!(tool_name, "test_tool");
                assert_eq!(arguments["key"], "value");
            }
            _ => panic!("expected ToolCall"),
        }
    }
}
