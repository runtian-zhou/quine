use std::pin::Pin;

use async_trait::async_trait;
use futures::stream::{self, Stream, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::provider::LlmProvider;
use crate::retry::send_with_retry;
use crate::types::{LlmEvent, Message, MessageContent, Role, TokenUsage, ToolDefinition};

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
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<OpenAiTool>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<OpenAiUsage>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
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
    reasoning_content: Option<String>,
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

fn split_reasoning_tags(text: &str, in_think_block: &mut bool) -> (String, String) {
    let mut visible = String::new();
    let mut reasoning = String::new();
    let mut rest = text;

    while !rest.is_empty() {
        if *in_think_block {
            if let Some(end) = rest.find("</think>") {
                reasoning.push_str(&rest[..end]);
                rest = &rest[end + "</think>".len()..];
                *in_think_block = false;
            } else {
                reasoning.push_str(rest);
                break;
            }
        } else if let Some(start) = rest.find("<think>") {
            visible.push_str(&rest[..start]);
            rest = &rest[start + "<think>".len()..];
            *in_think_block = true;
        } else {
            visible.push_str(rest);
            break;
        }
    }

    (visible, reasoning)
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
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            max_tokens: self.config.max_tokens,
            tools: convert_tools(tools),
        };

        let mut req = self.client.post(&url).json(&request_body);
        if let Some(key) = &self.config.api_key {
            req = req.bearer_auth(key);
        }

        let response = send_with_retry(req, "openai_compat").await?;

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
                None::<TokenUsage>,
                false,
                false,
            ),
            |(
                mut byte_stream,
                mut buffer,
                mut tool_acc,
                mut usage,
                mut in_think_block,
                mut done,
            )| async move {
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
                                events.push(Ok(LlmEvent::Done {
                                    usage: usage.take(),
                                }));
                                done = true;
                                return Some((
                                    stream::iter(events),
                                    (
                                        byte_stream,
                                        buffer,
                                        ToolCallAccumulator::default(),
                                        usage,
                                        in_think_block,
                                        done,
                                    ),
                                ));
                            }

                            match serde_json::from_str::<ChatChunk>(data) {
                                Ok(chunk) => {
                                    let mut events: Vec<anyhow::Result<LlmEvent>> = Vec::new();

                                    if let Some(chunk_usage) = chunk.usage {
                                        usage = Some(TokenUsage {
                                            input_tokens: chunk_usage.prompt_tokens,
                                            output_tokens: chunk_usage.completion_tokens,
                                        });
                                    }

                                    for choice in &chunk.choices {
                                        if let Some(reasoning_text) =
                                            &choice.delta.reasoning_content
                                        {
                                            if !reasoning_text.is_empty() {
                                                events.push(Ok(LlmEvent::ReasoningDelta {
                                                    text: reasoning_text.clone(),
                                                }));
                                            }
                                        }
                                        if let Some(text) = &choice.delta.content {
                                            let (visible_text, reasoning_text) =
                                                split_reasoning_tags(text, &mut in_think_block);
                                            if !reasoning_text.is_empty() {
                                                events.push(Ok(LlmEvent::ReasoningDelta {
                                                    text: reasoning_text,
                                                }));
                                            }
                                            if !visible_text.is_empty() {
                                                events.push(Ok(LlmEvent::TextDelta {
                                                    text: visible_text,
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
                                            (
                                                byte_stream,
                                                buffer,
                                                tool_acc,
                                                usage,
                                                in_think_block,
                                                done,
                                            ),
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
                                        (
                                            byte_stream,
                                            buffer,
                                            tool_acc,
                                            usage,
                                            in_think_block,
                                            done,
                                        ),
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
                                (byte_stream, buffer, tool_acc, usage, in_think_block, done),
                            ));
                        }
                        None => {
                            // Stream ended without [DONE] — emit accumulated tools + Done
                            let mut events: Vec<anyhow::Result<LlmEvent>> =
                                tool_acc.take_completed().into_iter().map(Ok).collect();
                            events.push(Ok(LlmEvent::Done {
                                usage: usage.take(),
                            }));
                            done = true;
                            return Some((
                                stream::iter(events),
                                (
                                    byte_stream,
                                    buffer,
                                    ToolCallAccumulator::default(),
                                    usage,
                                    in_think_block,
                                    done,
                                ),
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
    fn split_reasoning_tags_separates_visible_and_reasoning_text() {
        let mut in_think_block = false;
        let (visible, reasoning) =
            split_reasoning_tags("before<think>hidden</think>after", &mut in_think_block);
        assert_eq!(visible, "beforeafter");
        assert_eq!(reasoning, "hidden");
        assert!(!in_think_block);
    }

    #[test]
    fn split_reasoning_tags_handles_multichunk_blocks() {
        let mut in_think_block = false;
        let (visible1, reasoning1) = split_reasoning_tags("a<think>hidden", &mut in_think_block);
        let (visible2, reasoning2) = split_reasoning_tags(" more</think>b", &mut in_think_block);
        assert_eq!(visible1, "a");
        assert_eq!(reasoning1, "hidden");
        assert_eq!(visible2, "b");
        assert_eq!(reasoning2, " more");
        assert!(!in_think_block);
    }

    #[test]
    fn chat_chunk_deserializes_stream_usage() {
        let chunk: ChatChunk = serde_json::from_value(serde_json::json!({
            "choices": [],
            "usage": {
                "prompt_tokens": 123,
                "completion_tokens": 45
            }
        }))
        .unwrap();

        let usage = chunk.usage.expect("usage should be present");
        assert_eq!(usage.prompt_tokens, 123);
        assert_eq!(usage.completion_tokens, 45);
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
