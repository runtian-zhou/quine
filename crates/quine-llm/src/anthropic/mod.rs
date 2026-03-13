pub mod api_types;
pub mod stream;

use async_trait::async_trait;
use futures::stream::BoxStream;
use quine_core::conversation::ToolCall;
use reqwest::Client;
use self::api_types::*;
use crate::provider::LlmProvider;
use crate::types::*;

pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    fn build_request(&self, request: &CompletionRequest, stream: bool) -> AnthropicRequest {
        // System prompt as a cached block — it's static for the whole session.
        let system = vec![SystemBlock::text_cached(request.system.clone())];

        // Mark the last tool with cache_control so the tools array is cached.
        let mut tools = request.tools.clone();
        if let Some(last) = tools.last_mut() {
            if let Some(obj) = last.as_object_mut() {
                obj.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
            }
        }

        // Convert messages, then mark the last content block of the second-to-last
        // user message with cache_control so the conversation history is cached.
        let mut messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .map(|m| AnthropicMessage {
                role: m.role.clone(),
                content: match &m.content {
                    ChatContent::Text(s) => AnthropicContent::Text(s.clone()),
                    ChatContent::Blocks(blocks) => {
                        let ab: Vec<AnthropicBlock> = blocks
                            .iter()
                            .map(|b| match b {
                                ContentBlock::Text { text } => AnthropicBlock::Text {
                                    text: text.clone(),
                                    cache_control: None,
                                },
                                ContentBlock::ToolUse { id, name, input } => {
                                    AnthropicBlock::ToolUse {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: input.clone(),
                                    }
                                }
                                ContentBlock::ToolResult { tool_use_id, content } => {
                                    AnthropicBlock::ToolResult {
                                        tool_use_id: tool_use_id.clone(),
                                        content: content.clone(),
                                        cache_control: None,
                                    }
                                }
                            })
                            .collect();
                        AnthropicContent::Blocks(ab)
                    }
                },
            })
            .collect();

        // Find the second-to-last user message (the last one is the current turn;
        // caching it would be wasteful since it changes every request).
        // We cache the turn before it so repeated tool-call rounds reuse the prefix.
        let user_indices: Vec<usize> = messages
            .iter()
            .enumerate()
            .filter(|(_, m)| m.role == "user")
            .map(|(i, _)| i)
            .collect();

        if user_indices.len() >= 2 {
            let target = user_indices[user_indices.len() - 2];
            set_last_block_cached(&mut messages[target].content);
        }

        AnthropicRequest {
            model: request.model.clone(),
            max_tokens: if request.max_tokens > 0 { request.max_tokens } else { 8192 },
            system,
            messages,
            tools,
            stream: if stream { Some(true) } else { None },
        }
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_request = self.build_request(&request, false);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "prompt-caching-2024-07-31")
            .header("content-type", "application/json")
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let debug_request = serde_json::to_string_pretty(&api_request)
                .unwrap_or_else(|_| format!("{:?}", api_request));
            anyhow::bail!(
                "Anthropic API error ({}): {}\n\nRequest URL: {}/v1/messages\nRequest body:\n{}",
                status,
                body,
                self.base_url,
                debug_request
            );
        }

        let api_response: AnthropicResponse = response.json().await?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in api_response.content {
            match block {
                AnthropicBlock::Text { text, .. } => {
                    content.push_str(&text);
                }
                AnthropicBlock::ToolUse { id, name, input } => {
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: input,
                    });
                }
                _ => {}
            }
        }

        let stop_reason = match api_response.stop_reason.as_deref() {
            Some("end_turn") => StopReason::EndTurn,
            Some("tool_use") => StopReason::ToolUse,
            Some("max_tokens") => StopReason::MaxTokens,
            Some(other) => StopReason::Unknown(other.to_string()),
            _ => StopReason::Unknown("none".to_string()),
        };

        let usage = match api_response.usage {
            Some(u) => Usage {
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0),
                cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0),
            },
            None => Usage::default(),
        };

        Ok(CompletionResponse {
            content,
            tool_calls,
            stop_reason,
            usage,
        })
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        let api_request = self.build_request(&request, true);

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("anthropic-beta", "prompt-caching-2024-07-31")
            .header("content-type", "application/json")
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let debug_request = serde_json::to_string_pretty(&api_request)
                .unwrap_or_else(|_| format!("{:?}", api_request));
            anyhow::bail!(
                "Anthropic API error ({}): {}\n\nRequest URL: {}/v1/messages\nRequest body:\n{}",
                status,
                body,
                self.base_url,
                debug_request
            );
        }

        Ok(stream::parse_anthropic_sse(response))
    }
}

/// Set a cache breakpoint on the last content block of a message.
fn set_last_block_cached(content: &mut AnthropicContent) {
    match content {
        AnthropicContent::Blocks(blocks) => {
            if let Some(last) = blocks.last_mut() {
                match last {
                    AnthropicBlock::Text { cache_control, .. } => {
                        *cache_control = Some(CacheControl::ephemeral());
                    }
                    AnthropicBlock::ToolResult { cache_control, .. } => {
                        *cache_control = Some(CacheControl::ephemeral());
                    }
                    // ToolUse blocks can't carry cache_control; leave as-is.
                    AnthropicBlock::ToolUse { .. } => {}
                }
            }
        }
        // Plain text content — wrap it in a cached block.
        AnthropicContent::Text(s) => {
            *content = AnthropicContent::Blocks(vec![AnthropicBlock::Text {
                text: s.clone(),
                cache_control: Some(CacheControl::ephemeral()),
            }]);
        }
    }
}
