pub mod api_types;
pub mod stream;

use async_trait::async_trait;
use futures::stream::BoxStream;
use quine_core::conversation::ToolCall;
use reqwest::Client;
use reqwest_eventsource::EventSource;

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
        let messages: Vec<AnthropicMessage> = request
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
                                ContentBlock::Text { text } => {
                                    AnthropicBlock::Text { text: text.clone() }
                                }
                                ContentBlock::ToolUse { id, name, input } => {
                                    AnthropicBlock::ToolUse {
                                        id: id.clone(),
                                        name: name.clone(),
                                        input: input.clone(),
                                    }
                                }
                                ContentBlock::ToolResult {
                                    tool_use_id,
                                    content,
                                } => AnthropicBlock::ToolResult {
                                    tool_use_id: tool_use_id.clone(),
                                    content: content.clone(),
                                },
                            })
                            .collect();
                        AnthropicContent::Blocks(ab)
                    }
                },
            })
            .collect();

        AnthropicRequest {
            model: request.model.clone(),
            max_tokens: if request.max_tokens > 0 {
                request.max_tokens
            } else {
                8192
            },
            system: request.system.clone(),
            messages,
            tools: request.tools.clone(),
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
            .header("content-type", "application/json")
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Anthropic API error ({}): {}", status, body);
        }

        let api_response: AnthropicResponse = response.json().await?;

        let mut content = String::new();
        let mut tool_calls = Vec::new();

        for block in api_response.content {
            match block {
                AnthropicBlock::Text { text } => {
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

        Ok(CompletionResponse {
            content,
            tool_calls,
            stop_reason,
        })
    }

    async fn complete_stream(
        &self,
        request: CompletionRequest,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        let api_request = self.build_request(&request, true);

        let req = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&api_request);

        let es = EventSource::new(req)?;
        Ok(stream::parse_anthropic_stream(es))
    }
}
