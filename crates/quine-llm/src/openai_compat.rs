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
    /// Whether to ask the provider to emit parallelizable tool call batches.
    pub parallel_tool_calls: bool,
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
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    parallel_tool_calls: bool,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<serde_json::Value>,
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

#[derive(Serialize, Clone)]
struct OpenAiToolCall {
    id: String,
    r#type: String,
    function: OpenAiToolCallFunction,
}

#[derive(Deserialize, Clone)]
struct OpenAiToolCallDelta {
    #[serde(default)]
    index: Option<usize>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OpenAiToolCallFunctionDelta>,
}

#[derive(Serialize, Clone)]
struct OpenAiToolCallFunction {
    name: String,
    arguments: OpenAiToolCallArguments,
}

#[derive(Serialize, Clone)]
#[serde(untagged)]
enum OpenAiToolCallArguments {
    JsonString(String),
}

#[derive(Deserialize, Clone)]
struct OpenAiToolCallFunctionDelta {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

fn use_qwen_tool_call_parser(model: &str) -> bool {
    model.to_ascii_lowercase().contains("qwen")
}

fn use_structured_tool_result_content(model: &str) -> bool {
    model.to_ascii_lowercase().contains("qwen")
}

fn infer_tool_error_type(output: &str) -> &'static str {
    let normalized = output.trim().to_ascii_lowercase();
    if normalized.starts_with("filesystem error: ") {
        return infer_tool_error_type(
            output
                .trim()
                .split_once(':')
                .map(|(_, rest)| rest.trim())
                .unwrap_or(output),
        );
    }
    if normalized.starts_with("not found:") || normalized.contains(" no such file or directory") {
        "not_found"
    } else if normalized.starts_with("permission denied:") {
        "permission_denied"
    } else if normalized.starts_with("path traversal denied:") {
        "path_traversal"
    } else if normalized.starts_with("invalid arguments:") {
        "invalid_arguments"
    } else if normalized.starts_with("wrong entry type") {
        "wrong_entry_type"
    } else if normalized.starts_with("i/o error:") {
        "io_error"
    } else if normalized.starts_with("execution timed out after")
        || normalized.ends_with(" timed out")
        || normalized.contains(" timed out ")
    {
        "timeout"
    } else if normalized == "tool execution cancelled"
        || normalized.contains(" cancelled")
        || normalized.contains("canceled")
    {
        "cancelled"
    } else if normalized.starts_with("unknown tool:") {
        "unknown_tool"
    } else if normalized.starts_with("internal error:")
        || normalized.starts_with("failed to spawn:")
        || normalized.starts_with("failed to execute command:")
        || normalized.starts_with("tool task panicked:")
        || normalized.starts_with("input channel closed")
        || normalized.starts_with("session not found")
    {
        "internal_error"
    } else if normalized.starts_with("missing required parameter:") {
        "invalid_arguments"
    } else {
        "tool_error"
    }
}

fn format_tool_result_content(
    output: &str,
    is_error: bool,
    structured_tool_result_content: bool,
) -> String {
    if !structured_tool_result_content {
        return output.to_string();
    }

    let content = if is_error {
        serde_json::json!({
            "ok": false,
            "error": {
                "type": infer_tool_error_type(output),
                "message": output,
            }
        })
    } else {
        serde_json::json!({
            "ok": true,
            "content": output,
        })
    };

    serde_json::to_string(&content).unwrap_or_else(|_| output.to_string())
}

fn convert_message(msg: &Message, structured_tool_result_content: bool) -> ChatMessage {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    match &msg.content {
        MessageContent::Text(text) => ChatMessage {
            role: role.into(),
            content: Some(serde_json::Value::String(text.clone())),
            tool_call_id: None,
            tool_calls: None,
        },
        MessageContent::ToolResult {
            tool_use_id,
            output,
            is_error,
        } => ChatMessage {
            role: "tool".into(),
            content: Some(serde_json::Value::String(format_tool_result_content(
                output,
                *is_error,
                structured_tool_result_content,
            ))),
            tool_call_id: Some(tool_use_id.clone()),
            tool_calls: None,
        },
        MessageContent::ToolUse { text, tool_calls } => ChatMessage {
            role: "assistant".into(),
            content: Some(serde_json::Value::String(text.clone().unwrap_or_default())),
            tool_call_id: None,
            tool_calls: Some(
                tool_calls
                    .iter()
                    .map(|tc| OpenAiToolCall {
                        id: tc.tool_use_id.clone(),
                        r#type: "function".into(),
                        function: OpenAiToolCallFunction {
                            name: tc.tool_name.clone(),
                            arguments: OpenAiToolCallArguments::JsonString(
                                serde_json::to_string(&tc.arguments).unwrap_or_default(),
                            ),
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
    next_tool_call_index: u64,
}

struct AccumulatedToolCall {
    id: String,
    name: String,
    arguments: String,
}

#[derive(Default)]
struct QwenToolCallParser {
    pending: String,
    next_tool_call_index: u64,
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

fn parse_qwen_parameter_value(text: &str) -> serde_json::Value {
    let trimmed = text.trim();
    serde_json::from_str(trimmed).unwrap_or_else(|_| serde_json::Value::String(trimmed.to_string()))
}

fn parse_qwen_tool_call_block(block: &str, tool_use_id: String) -> Option<LlmEvent> {
    let body = block
        .strip_prefix("<tool_call>")
        .and_then(|text| text.strip_suffix("</tool_call>"))?
        .trim();
    let function_body = body.strip_prefix("<function=")?;
    let function_name_end = function_body.find('>')?;
    let tool_name = function_body[..function_name_end].trim();
    let function_rest = &function_body[function_name_end + 1..];
    let function_end = function_rest.find("</function>")?;
    let parameter_body = &function_rest[..function_end];

    let mut arguments = serde_json::Map::new();
    let mut remaining = parameter_body;
    loop {
        let trimmed = remaining.trim_start();
        if trimmed.is_empty() {
            break;
        }
        let parameter_body = trimmed.strip_prefix("<parameter=")?;
        let parameter_name_end = parameter_body.find('>')?;
        let parameter_name = parameter_body[..parameter_name_end].trim();
        let parameter_rest = &parameter_body[parameter_name_end + 1..];
        let parameter_end = parameter_rest.find("</parameter>")?;
        let parameter_value = &parameter_rest[..parameter_end];
        arguments.insert(
            parameter_name.to_string(),
            parse_qwen_parameter_value(parameter_value),
        );
        remaining = &parameter_rest[parameter_end + "</parameter>".len()..];
    }

    Some(LlmEvent::ToolCall {
        tool_use_id,
        tool_name: tool_name.to_string(),
        arguments: serde_json::Value::Object(arguments),
    })
}

fn qwen_text_safe_prefix_len(text: &str) -> usize {
    match text.rfind('<') {
        Some(index) if !text[index..].contains('>') => index,
        _ => text.len(),
    }
}

impl QwenToolCallParser {
    fn next_tool_use_id(&mut self) -> String {
        self.next_tool_call_index += 1;
        format!("qwen-tool-call-{}", self.next_tool_call_index)
    }

    fn push_text(&mut self, text: &str) -> Vec<LlmEvent> {
        self.pending.push_str(text);
        let mut events = Vec::new();

        loop {
            if let Some(start) = self.pending.find("<tool_call>") {
                if start > 0 {
                    events.push(LlmEvent::TextDelta {
                        text: self.pending[..start].to_string(),
                    });
                    self.pending.drain(..start);
                }

                if let Some(end_start) = self.pending.find("</tool_call>") {
                    let end = end_start + "</tool_call>".len();
                    let block = self.pending[..end].to_string();
                    self.pending.drain(..end);
                    let tool_use_id = self.next_tool_use_id();
                    match parse_qwen_tool_call_block(&block, tool_use_id) {
                        Some(event) => events.push(event),
                        None => events.push(LlmEvent::TextDelta { text: block }),
                    }
                    continue;
                }
                break;
            }

            let safe_prefix_len = qwen_text_safe_prefix_len(&self.pending);
            if safe_prefix_len > 0 {
                events.push(LlmEvent::TextDelta {
                    text: self.pending[..safe_prefix_len].to_string(),
                });
                self.pending.drain(..safe_prefix_len);
            }
            break;
        }

        events
    }

    fn finish(&mut self) -> Vec<LlmEvent> {
        if self.pending.is_empty() {
            Vec::new()
        } else {
            vec![LlmEvent::TextDelta {
                text: std::mem::take(&mut self.pending),
            }]
        }
    }
}

impl ToolCallAccumulator {
    fn next_tool_use_id(&mut self) -> String {
        self.next_tool_call_index += 1;
        format!("openai-tool-call-{}", self.next_tool_call_index)
    }

    fn process_delta(&mut self, delta: &OpenAiToolCallDelta, missing_index_starts_new_call: bool) {
        let index = match delta.index {
            Some(index) => index,
            None if missing_index_starts_new_call => {
                if let Some(id) = &delta.id {
                    if let Some(existing_index) = self.calls.iter().position(|call| call.id == *id)
                    {
                        existing_index
                    } else {
                        self.calls.push(AccumulatedToolCall {
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        });
                        self.calls.len() - 1
                    }
                } else {
                    self.calls.push(AccumulatedToolCall {
                        id: String::new(),
                        name: String::new(),
                        arguments: String::new(),
                    });
                    self.calls.len() - 1
                }
            }
            None => 0,
        };
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
        if self.calls[index].id.is_empty()
            && (!self.calls[index].name.is_empty() || !self.calls[index].arguments.is_empty())
        {
            self.calls[index].id = self.next_tool_use_id();
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
        let structured_tool_result_content = use_structured_tool_result_content(&self.config.model);
        let qwen_tool_call_parser_enabled = use_qwen_tool_call_parser(&self.config.model);
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );

        let request_body = ChatRequest {
            model: self.config.model.clone(),
            messages: messages
                .iter()
                .map(|message| convert_message(message, structured_tool_result_content))
                .collect(),
            stream: true,
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            max_tokens: self.config.max_tokens,
            tools: convert_tools(tools),
            parallel_tool_calls: self.config.parallel_tool_calls,
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
                QwenToolCallParser::default(),
                None::<TokenUsage>,
                false,
                false,
            ),
            move |(
                mut byte_stream,
                mut buffer,
                mut tool_acc,
                mut qwen_tool_parser,
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
                                if qwen_tool_call_parser_enabled {
                                    events.extend(qwen_tool_parser.finish().into_iter().map(Ok));
                                }
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
                                        QwenToolCallParser::default(),
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
                                                if qwen_tool_call_parser_enabled {
                                                    events.extend(
                                                        qwen_tool_parser
                                                            .push_text(&visible_text)
                                                            .into_iter()
                                                            .map(Ok),
                                                    );
                                                } else {
                                                    events.push(Ok(LlmEvent::TextDelta {
                                                        text: visible_text,
                                                    }));
                                                }
                                            }
                                        }
                                        if let Some(tool_calls) = &choice.delta.tool_calls {
                                            for tc_delta in tool_calls {
                                                tool_acc.process_delta(
                                                    tc_delta,
                                                    qwen_tool_call_parser_enabled,
                                                );
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
                                                qwen_tool_parser,
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
                                            qwen_tool_parser,
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
                                (
                                    byte_stream,
                                    buffer,
                                    tool_acc,
                                    qwen_tool_parser,
                                    usage,
                                    in_think_block,
                                    done,
                                ),
                            ));
                        }
                        None => {
                            // Stream ended without [DONE] — emit accumulated tools + Done
                            let mut events: Vec<anyhow::Result<LlmEvent>> =
                                tool_acc.take_completed().into_iter().map(Ok).collect();
                            if qwen_tool_call_parser_enabled {
                                events.extend(qwen_tool_parser.finish().into_iter().map(Ok));
                            }
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
                                    QwenToolCallParser::default(),
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
        let chat = convert_message(&msg, false);
        assert_eq!(chat.role, "user");
        assert_eq!(chat.content, Some(serde_json::json!("hello")));
    }

    #[test]
    fn convert_message_tool_result() {
        let msg = Message::tool_result("id-1", "output", false);
        let chat = convert_message(&msg, false);
        assert_eq!(chat.role, "tool");
        assert_eq!(chat.tool_call_id.unwrap(), "id-1");
    }

    #[test]
    fn convert_message_tool_use_sets_empty_content_when_missing() {
        let msg = Message::assistant_tool_use(
            None,
            vec![crate::types::ToolUseRequest {
                tool_use_id: "id-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"file_path": "/tmp/test.txt"}),
            }],
        );
        let chat = convert_message(&msg, false);
        assert_eq!(chat.role, "assistant");
        assert_eq!(chat.content, Some(serde_json::json!("")));
        assert!(chat.tool_calls.is_some());
    }

    #[test]
    fn convert_message_tool_use_omits_whitespace_only_content() {
        let msg = Message::assistant_tool_use(
            Some("\n\t ".into()),
            vec![crate::types::ToolUseRequest {
                tool_use_id: "id-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"file_path": "/tmp/test.txt"}),
            }],
        );
        let chat = convert_message(&msg, false);
        assert_eq!(chat.role, "assistant");
        assert_eq!(chat.content, Some(serde_json::json!("\n\t ")));
        assert!(chat.tool_calls.is_some());
    }

    #[test]
    fn convert_message_tool_use_serializes_without_content_field_when_empty() {
        let msg = Message::assistant_tool_use(
            None,
            vec![crate::types::ToolUseRequest {
                tool_use_id: "id-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"file_path": "/tmp/test.txt"}),
            }],
        );
        let chat = convert_message(&msg, false);
        let json = serde_json::to_value(&chat).unwrap();
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"], "");
        assert!(json.get("tool_calls").is_some());
    }

    #[test]
    fn convert_message_tool_use_stringifies_tool_arguments() {
        let msg = Message::assistant_tool_use(
            None,
            vec![crate::types::ToolUseRequest {
                tool_use_id: "id-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"file_path": "/tmp/test.txt"}),
            }],
        );
        let chat = convert_message(&msg, true);
        let tool_calls = chat.tool_calls.expect("tool calls");
        let json = serde_json::to_value(&tool_calls[0]).unwrap();
        assert_eq!(
            json["function"]["arguments"],
            "{\"file_path\":\"/tmp/test.txt\"}"
        );
    }

    #[test]
    fn chat_request_serializes_tool_use_arguments_as_string() {
        let msg = Message::assistant_tool_use(
            None,
            vec![crate::types::ToolUseRequest {
                tool_use_id: "id-1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({"file_path": "/tmp/test.txt"}),
            }],
        );
        let request = ChatRequest {
            model: "qwen/qwen3-coder-next".into(),
            messages: vec![convert_message(&msg, false)],
            stream: true,
            stream_options: None,
            max_tokens: Some(128),
            tools: vec![],
            parallel_tool_calls: false,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["messages"][0]["role"], "assistant");
        assert_eq!(json["messages"][0]["content"], "");
        assert_eq!(
            json["messages"][0]["tool_calls"][0]["function"]["arguments"],
            "{\"file_path\":\"/tmp/test.txt\"}"
        );
    }

    #[test]
    fn qwen_tool_call_parser_is_only_enabled_for_qwen_models() {
        assert!(use_qwen_tool_call_parser("Qwen3-Coder-Next"));
        assert!(use_qwen_tool_call_parser("qwen2.5"));
        assert!(!use_qwen_tool_call_parser("gpt-4.1"));
        assert!(!use_qwen_tool_call_parser("claude-sonnet-4"));
    }

    #[test]
    fn convert_message_tool_result_can_emit_structured_qwen_error() {
        let msg = Message::tool_result("id-1", "not found: missing.txt", true);
        let chat = convert_message(&msg, true);
        let content = chat.content.expect("tool content");
        let json: serde_json::Value = serde_json::from_str(
            content
                .as_str()
                .expect("structured tool result should serialize as a string"),
        )
        .unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["type"], "not_found");
        assert_eq!(json["error"]["message"], "not found: missing.txt");
    }

    #[test]
    fn convert_message_tool_result_can_emit_structured_qwen_success() {
        let msg = Message::tool_result("id-1", "file contents", false);
        let chat = convert_message(&msg, true);
        let content = chat.content.expect("tool content");
        let json: serde_json::Value = serde_json::from_str(
            content
                .as_str()
                .expect("structured tool result should serialize as a string"),
        )
        .unwrap();
        assert_eq!(json["ok"], true);
        assert_eq!(json["content"], "file contents");
    }

    #[test]
    fn infer_tool_error_type_covers_common_tool_failures() {
        assert_eq!(
            infer_tool_error_type("invalid arguments: missing required parameter: file_path"),
            "invalid_arguments"
        );
        assert_eq!(
            infer_tool_error_type("filesystem error: not found: missing.txt"),
            "not_found"
        );
        assert_eq!(
            infer_tool_error_type("permission denied: bash command rejected"),
            "permission_denied"
        );
        assert_eq!(
            infer_tool_error_type("execution timed out after 30s"),
            "timeout"
        );
        assert_eq!(infer_tool_error_type("recv_message timed out"), "timeout");
        assert_eq!(
            infer_tool_error_type("tool execution cancelled"),
            "cancelled"
        );
        assert_eq!(infer_tool_error_type("unknown tool: nope"), "unknown_tool");
        assert_eq!(
            infer_tool_error_type("internal error: failed to create session"),
            "internal_error"
        );
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
    fn qwen_tool_call_parser_converts_complete_block_to_tool_event() {
        let mut parser = QwenToolCallParser::default();
        let events = parser.push_text(
            "<tool_call><function=read_file><parameter=file_path>/tmp/test.txt</parameter></function></tool_call>",
        );
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(tool_use_id, "qwen-tool-call-1");
                assert_eq!(tool_name, "read_file");
                assert_eq!(arguments["file_path"], "/tmp/test.txt");
            }
            _ => panic!("expected ToolCall event"),
        }
    }

    #[test]
    fn qwen_tool_call_parser_handles_split_blocks() {
        let mut parser = QwenToolCallParser::default();
        let events1 = parser.push_text("<tool_call><function=read_");
        assert!(events1.is_empty());

        let events2 = parser.push_text(
            "file><parameter=file_path>/tmp/test.txt</parameter></function></tool_call>",
        );
        assert_eq!(events2.len(), 1);
        match &events2[0] {
            LlmEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(arguments["file_path"], "/tmp/test.txt");
            }
            _ => panic!("expected ToolCall event"),
        }
    }

    #[test]
    fn qwen_tool_call_parser_preserves_visible_text_outside_blocks() {
        let mut parser = QwenToolCallParser::default();
        let events = parser.push_text(
            "before<tool_call><function=read_file><parameter=file_path>/tmp/test.txt</parameter></function></tool_call>after",
        );
        assert_eq!(events.len(), 3);
        match &events[0] {
            LlmEvent::TextDelta { text } => assert_eq!(text, "before"),
            _ => panic!("expected leading TextDelta"),
        }
        match &events[1] {
            LlmEvent::ToolCall { tool_name, .. } => assert_eq!(tool_name, "read_file"),
            _ => panic!("expected ToolCall"),
        }
        match &events[2] {
            LlmEvent::TextDelta { text } => assert_eq!(text, "after"),
            _ => panic!("expected trailing TextDelta"),
        }
    }

    #[test]
    fn tool_call_accumulator_generates_id_when_provider_omits_one() {
        let mut acc = ToolCallAccumulator::default();
        acc.process_delta(
            &OpenAiToolCallDelta {
                index: Some(0),
                id: None,
                function: Some(OpenAiToolCallFunctionDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"file_path\":\"/tmp/test.txt\"}".into()),
                }),
            },
            false,
        );

        let events = acc.take_completed();
        assert_eq!(events.len(), 1);
        match &events[0] {
            LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            } => {
                assert_eq!(tool_use_id, "openai-tool-call-1");
                assert_eq!(tool_name, "read_file");
                assert_eq!(arguments["file_path"], "/tmp/test.txt");
            }
            _ => panic!("expected ToolCall event"),
        }
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
        acc.process_delta(
            &OpenAiToolCallDelta {
                index: Some(0),
                id: Some("call_1".into()),
                function: Some(OpenAiToolCallFunctionDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"path\":".into()),
                }),
            },
            false,
        );

        // Second delta has more arguments
        acc.process_delta(
            &OpenAiToolCallDelta {
                index: Some(0),
                id: None,
                function: Some(OpenAiToolCallFunctionDelta {
                    name: None,
                    arguments: Some("\"/tmp/test\"}".into()),
                }),
            },
            false,
        );

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

    #[test]
    fn qwen_missing_index_tool_calls_do_not_merge_into_one_call() {
        let mut acc = ToolCallAccumulator::default();

        acc.process_delta(
            &OpenAiToolCallDelta {
                index: None,
                id: None,
                function: Some(OpenAiToolCallFunctionDelta {
                    name: Some("search".into()),
                    arguments: Some("{\"pattern\":\"spawn\"}".into()),
                }),
            },
            true,
        );
        acc.process_delta(
            &OpenAiToolCallDelta {
                index: None,
                id: None,
                function: Some(OpenAiToolCallFunctionDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"file_path\":\"/tmp/test.txt\"}".into()),
                }),
            },
            true,
        );

        let events = acc.take_completed();
        assert_eq!(events.len(), 2);

        match &events[0] {
            LlmEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(tool_name, "search");
                assert_eq!(arguments["pattern"], "spawn");
            }
            _ => panic!("expected ToolCall event"),
        }

        match &events[1] {
            LlmEvent::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(tool_name, "read_file");
                assert_eq!(arguments["file_path"], "/tmp/test.txt");
            }
            _ => panic!("expected ToolCall event"),
        }
    }

    #[test]
    fn non_qwen_missing_index_tool_calls_preserve_existing_merge_behavior() {
        let mut acc = ToolCallAccumulator::default();

        acc.process_delta(
            &OpenAiToolCallDelta {
                index: None,
                id: Some("call_1".into()),
                function: Some(OpenAiToolCallFunctionDelta {
                    name: Some("read_file".into()),
                    arguments: Some("{\"path\":".into()),
                }),
            },
            false,
        );
        acc.process_delta(
            &OpenAiToolCallDelta {
                index: None,
                id: None,
                function: Some(OpenAiToolCallFunctionDelta {
                    name: None,
                    arguments: Some("\"/tmp/test\"}".into()),
                }),
            },
            false,
        );

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
