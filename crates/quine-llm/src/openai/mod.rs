pub mod api_types;
pub mod stream;

use self::api_types::*;
use crate::provider::LlmProvider;
use crate::types::*;
use async_trait::async_trait;
use futures::stream::BoxStream;
use quine_core::conversation::ToolCall;
use reqwest::Client;

pub struct OpenAiProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    fn build_request(&self, request: &CompletionRequest, stream: bool) -> OpenAiRequest {
        let mut messages = vec![OpenAiMessage {
            role: "system".to_string(),
            content: Some(request.system.clone()),
            tool_calls: None,
            tool_call_id: None,
        }];

        for m in &request.messages {
            match &m.content {
                ChatContent::Text(s) => {
                    messages.push(OpenAiMessage {
                        role: m.role.clone(),
                        content: Some(s.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                    });
                }
                ChatContent::Blocks(blocks) => {
                    let mut text_parts = Vec::new();
                    let mut tool_calls_out = Vec::new();
                    let mut tool_result_id = None;

                    for block in blocks {
                        match block {
                            ContentBlock::Text { text } => text_parts.push(text.clone()),
                            ContentBlock::ToolUse { id, name, input } => {
                                tool_calls_out.push(OpenAiToolCall {
                                    id: id.clone(),
                                    call_type: "function".to_string(),
                                    function: OpenAiFunctionCall {
                                        name: name.clone(),
                                        arguments: serde_json::to_string(input).unwrap_or_default(),
                                    },
                                });
                            }
                            ContentBlock::ToolResult {
                                tool_use_id,
                                content,
                            } => {
                                tool_result_id = Some(tool_use_id.clone());
                                text_parts.push(content.clone());
                            }
                        }
                    }

                    if let Some(id) = tool_result_id {
                        messages.push(OpenAiMessage {
                            role: "tool".to_string(),
                            content: Some(text_parts.join("")),
                            tool_calls: None,
                            tool_call_id: Some(id),
                        });
                    } else {
                        messages.push(OpenAiMessage {
                            role: m.role.clone(),
                            content: if text_parts.is_empty() {
                                None
                            } else {
                                Some(text_parts.join(""))
                            },
                            tool_calls: if tool_calls_out.is_empty() {
                                None
                            } else {
                                Some(tool_calls_out)
                            },
                            tool_call_id: None,
                        });
                    }
                }
            }
        }

        let tools: Vec<OpenAiTool> = request
            .tools
            .iter()
            .filter_map(|t| {
                let name = t["name"].as_str()?;
                let description = t["description"].as_str()?;
                let parameters = t["input_schema"].clone();
                Some(OpenAiTool {
                    tool_type: "function".to_string(),
                    function: OpenAiFunction {
                        name: name.to_string(),
                        description: description.to_string(),
                        parameters,
                    },
                })
            })
            .collect();

        OpenAiRequest {
            model: request.model.clone(),
            messages,
            tools,
            max_tokens: if request.max_tokens > 0 {
                Some(request.max_tokens)
            } else {
                None
            },
            stream: if stream { Some(true) } else { None },
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    fn name(&self) -> &str {
        "openai"
    }

    async fn complete(&self, request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        let api_request = self.build_request(&request, false);

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let debug_request = serde_json::to_string_pretty(&api_request)
                .unwrap_or_else(|_| format!("{:?}", api_request));
            anyhow::bail!(
                "OpenAI API error ({}): {}\n\nRequest URL: {}/chat/completions\nRequest body:\n{}",
                status,
                body,
                self.base_url,
                debug_request
            );
        }

        let api_response: OpenAiResponse = response.json().await?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("No choices in response"))?;

        let content = choice.message.content.unwrap_or_default();

        let tool_calls: Vec<ToolCall> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| ToolCall {
                id: tc.id,
                name: tc.function.name,
                arguments: serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::Value::Null),
            })
            .collect();

        let stop_reason = match choice.finish_reason.as_deref() {
            Some("stop") => StopReason::EndTurn,
            Some("tool_calls") => StopReason::ToolUse,
            Some("length") => StopReason::MaxTokens,
            Some(other) => StopReason::Unknown(other.to_string()),
            None => StopReason::Unknown("none".to_string()),
        };

        let usage = match api_response.usage {
            Some(u) => Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
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
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&api_request)
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let debug_request = serde_json::to_string_pretty(&api_request)
                .unwrap_or_else(|_| format!("{:?}", api_request));
            anyhow::bail!(
                "OpenAI API error ({}): {}\n\nRequest URL: {}/chat/completions\nRequest body:\n{}",
                status,
                body,
                self.base_url,
                debug_request
            );
        }

        Ok(stream::parse_openai_sse(response))
    }
}
