use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::types::{LlmEvent, Message, MessageContent, Role, ToolDefinition};

/// Configuration for an OpenAI-compatible LLM endpoint.
#[derive(Debug, Clone)]
pub struct OpenAiCompatConfig {
    /// Base URL for the API (e.g., `http://127.0.0.1:1234/v1`).
    pub base_url: String,
    /// Optional API key.
    pub api_key: Option<String>,
    /// Model identifier.
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
}

/// An LLM provider adapter for OpenAI-compatible APIs.
///
/// Works with OpenAI, LM Studio, ollama, vLLM, and any other service
/// exposing the OpenAI chat completions endpoint.
pub struct OpenAiCompatProvider {
    config: OpenAiCompatConfig,
    client: Client,
}

impl OpenAiCompatProvider {
    pub fn new(config: OpenAiCompatConfig) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { config, client }
    }
}

// --- Request types ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OpenAiToolCall>>,
}

#[derive(Serialize)]
struct OpenAiTool {
    r#type: String,
    function: OpenAiFunction,
}

#[derive(Serialize)]
struct OpenAiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// --- Response types ---

#[derive(Deserialize)]
struct ChatChunk {
    choices: Vec<ChunkChoice>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OpenAiToolCallDelta>>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OpenAiToolCall {
    id: String,
    r#type: String,
    function: OpenAiToolCallFunction,
}

#[derive(Deserialize, Clone)]
struct OpenAiToolCallDelta {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAiToolCallFunctionDelta>,
}

#[derive(Serialize, Deserialize, Clone)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: String,
}

#[derive(Deserialize, Clone)]
struct OpenAiToolCallFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

fn convert_message(msg: &Message) -> ChatMessage {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    match &msg.content {
        MessageContent::Text(text) => ChatMessage {
            role: role.into(),
            content: Some(text.clone()),
            tool_call_id: None,
            tool_calls: None,
        },
        MessageContent::ToolResult {
            tool_use_id,
            output,
            ..
        } => ChatMessage {
            role: "tool".into(),
            content: Some(output.clone()),
            tool_call_id: Some(tool_use_id.clone()),
            tool_calls: None,
        },
        MessageContent::ToolUse { text, tool_calls } => ChatMessage {
            role: "assistant".into(),
            content: text.clone(),
            tool_call_id: None,
            tool_calls: Some(
                tool_calls
                    .iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.tool_use_id.clone(),
                        r#type: "function".into(),
                        function: OpenAiToolCallFunction {
                            name: tc.tool_name.clone(),
                            arguments: serde_json::to_string(&tc.arguments).unwrap_or_default(),
                        },
                    })
                    .collect(),
            ),
        },
    }
}

fn convert_tools(tools: &[ToolDefinition]) -> Vec<OpenAiTool> {
    tools
        .iter()
        .map(|t| OpenAiTool {
            r#type: "function".into(),
            function: OpenAiFunction {
                name: t.name.clone(),
                description: t.description.clone(),
                parameters: t.parameters.clone(),
            },
        })
        .collect()
}

/// Accumulates tool call deltas across SSE chunks.
#[derive(Default)]
struct ToolCallAccumulator {
    calls: Vec<AccumulatedToolCall>,
}

struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

impl ToolCallAccumulator {
    fn process_delta(&mut self, delta: &OpenAiToolCallDelta) {
        let index = delta.index;
        // Grow the accumulator if needed
        while self.calls.len() <= index {
            self.calls.push(AccumulatedToolCall {
                id: String::new(),
                name: String::new(),
                arguments: String::new(),
            });
        }
        if let Some(id) = &delta.id {
            self.calls[index].id.clone_from(id);
        }
        if let Some(func) = &delta.function {
            if let Some(name) = &func.name {
                self.calls[index].name.clone_from(name);
            }
            if let Some(args) = &func.arguments {
                self.calls[index].arguments.push_str(args);
            }
        }
    }

    fn take_completed(self) -> Vec<LlmEvent> {
        self.calls
            .into_iter()
            .filter(|c| !c.id.is_empty())
            .map(|c| {
                let arguments = serde_json::from_str(&c.arguments)
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

#[async_trait]
impl LlmProvider for OpenAiCompatProvider {
    async fn send(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<LlmEvent>> + Send>>> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let request_body = ChatRequest {
            model: self.config.model.clone(),
            messages: messages.iter().map(convert_message).collect(),
            stream: true,
            max_tokens: self.config.max_tokens,
            tools: convert_tools(tools),
        };

        let mut req = self.client.post(&url).json(&request_body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }

        let response = req.send().await.map_err(LlmError::from)?;

        if !response.status().is_success() {
            let status = response.status().as_u16();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unable to read body".into());
            return Err(LlmError::ProviderHttp { status, body }.into());
        }

        let byte_stream = response.bytes_stream();

        let event_stream = stream::unfold(
            (
                byte_stream,
                String::new(),
                ToolCallAccumulator::default(),
                false,
            ),
            |(mut byte_stream, mut buffer, mut tool_acc, mut done)| async move {
                if done {
                    return None;
                }

                loop {
                    // Try to extract a complete SSE line from the buffer
                    if let Some(newline_pos) = buffer.find('\n') {
                        let line = buffer[..newline_pos].trim_end_matches('\r').to_string();
                        buffer = buffer[newline_pos + 1..].to_string();

                        if line.is_empty() {
                            continue;
                        }

                        if let Some(data) = line.strip_prefix("data: ") {
                            let data = data.trim();
                            if data == "[DONE]" {
                                // Emit any accumulated tool calls, then Done
                                let mut events: Vec<anyhow::Result<LlmEvent>> =
                                    tool_acc.take_completed().into_iter().map(Ok).collect();
                                events.push(Ok(LlmEvent::Done));
                                done = true;
                                return Some((
                                    stream::iter(events),
                                    (byte_stream, buffer, ToolCallAccumulator::default(), done),
                                ));
                            }

                            match serde_json::from_str::<ChatChunk>(data) {
                                Ok(chunk) => {
                                    let mut events: Vec<anyhow::Result<LlmEvent>> = Vec::new();

                                    for choice in &chunk.choices {
                                        if let Some(text) = &choice.delta.content {
                                            if !text.is_empty() {
                                                events.push(Ok(LlmEvent::TextDelta {
                                                    text: text.clone(),
                                                }));
                                            }
                                        }
                                        if let Some(tool_calls) = &choice.delta.tool_calls {
                                            for tc_delta in tool_calls {
                                                tool_acc.process_delta(tc_delta);
                                            }
                                        }
                                        if choice.finish_reason.as_deref() == Some("tool_calls") {
                                            let completed = tool_acc.take_completed();
                                            events.extend(completed.into_iter().map(Ok));
                                            tool_acc = ToolCallAccumulator::default();
                                        }
                                    }

                                    if !events.is_empty() {
                                        return Some((
                                            stream::iter(events),
                                            (byte_stream, buffer, tool_acc, done),
                                        ));
                                    }
                                    continue;
                                }
                                Err(e) => {
                                    return Some((
                                        stream::iter(vec![Err(LlmError::ParseError {
                                            message: format!("failed to parse chunk: {e}: {data}"),
                                        }
                                        .into())]),
                                        (byte_stream, buffer, tool_acc, done),
                                    ));
                                }
                            }
                        }
                        // Skip non-data lines (e.g., event: or comments)
                        continue;
                    }

                    // Need more data from the network
                    use futures::StreamExt;
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            buffer.push_str(&String::from_utf8_lossy(&bytes));
                        }
                        Some(Err(e)) => {
                            return Some((
                                stream::iter(vec![Err(e.into())]),
                                (byte_stream, buffer, tool_acc, done),
                            ));
                        }
                        None => {
                            // Stream ended without [DONE] — emit accumulated tools + Done
                            let mut events: Vec<anyhow::Result<LlmEvent>> =
                                tool_acc.take_completed().into_iter().map(Ok).collect();
                            events.push(Ok(LlmEvent::Done));
                            done = true;
                            return Some((
                                stream::iter(events),
                                (byte_stream, buffer, ToolCallAccumulator::default(), done),
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
    fn convert_message_user() {
        let msg = Message::user("hello");
        let chat = convert_message(&msg);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content.unwrap(), "hello");
    }

    #[test]
    fn convert_message_tool_result() {
        let msg = Message::tool_result("id-1", "output", false);
        let chat = convert_message(&msg);
        assert_eq!(chat.role, "tool");
        assert_eq!(chat.tool_call_id.unwrap(), "id-1");
    }

    #[test]
    fn convert_tools_produces_function_type() {
        let tools = vec![ToolDefinition {
            name: "test".into(),
            description: "test tool".into(),
            parameters: serde_json::json!({}),
        }];
        let converted = convert_tools(&tools);
        assert_eq!(converted.len(), 1);
        assert_eq!(converted[0].r#type, "function");
        assert_eq!(converted[0].function.name, "test");
    }

    #[test]
    fn tool_call_accumulator_basic() {
        let mut acc = ToolCallAccumulator::default();

        // First delta has id and name
        acc.process_delta(&OpenAiToolCallDelta {
            index: 0,
            id: Some("call_1".into()),
            function: Some(OpenAiToolCallFunctionDelta {
                name: Some("read_file".into()),
                arguments: Some("{\"path\":".into()),
            }),
        });

        // Second delta has more arguments
        acc.process_delta(&OpenAiToolCallDelta {
            index: 0,
            id: None,
            function: Some(OpenAiToolCallFunctionDelta {
                name: None,
                arguments: Some("\"/tmp/test\"}".into()),
            }),
        });

        let events = acc.take_completed();
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(tool_use_id, "call_1");
                assert_eq!(tool_name, "read_file");
                assert_eq!(arguments["path"], "/tmp/test");
            }
            _ => panic!("expected ToolCall event"),
        }
    }
}
