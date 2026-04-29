use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::LlmError;
use crate::retry::send_with_retry;
use crate::web::{
    WebCitation, WebOpenRequest, WebProvider, WebResult, WebSearchRequest, WebSource,
    WebUserLocation,
};

/// Configuration for the OpenAI Responses API-backed web provider.
#[derive(Debug, Clone)]
pub struct OpenAiWebConfig {
    /// Base URL for the Responses API root, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    /// API key used for authentication. Leave empty to omit bearer auth.
    pub api_key: String,
    /// Model identifier used for web-backed requests.
    pub model: String,
}

/// Web provider backed by OpenAI's `web_search` Responses API tool.
pub struct OpenAiWebProvider {
    config: OpenAiWebConfig,
    client: Client,
}

impl OpenAiWebProvider {
    pub fn new(config: OpenAiWebConfig) -> Self {
        let client = Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .unwrap_or_default();
        Self { config, client }
    }

    fn post_json(&self, url: String) -> reqwest::RequestBuilder {
        let req = self.client.post(url);
        if self.config.api_key.is_empty() {
            req
        } else {
            req.bearer_auth(&self.config.api_key)
        }
    }

    async fn run_request(
        &self,
        input: String,
        allowed_domains: Vec<String>,
        user_location: Option<WebUserLocation>,
        external_web_access: bool,
    ) -> anyhow::Result<WebResult> {
        let responses_url = format!("{}/responses", self.config.base_url.trim_end_matches('/'));
        let response_request = ResponsesRequest {
            model: self.config.model.clone(),
            reasoning: Some(ReasoningConfig {
                effort: "low".into(),
            }),
            tools: vec![WebSearchTool {
                r#type: "web_search".into(),
                filters: if allowed_domains.is_empty() {
                    None
                } else {
                    Some(WebSearchFilters { allowed_domains })
                },
                user_location: user_location.map(UserLocation::from),
                external_web_access: Some(external_web_access),
            }],
            tool_choice: Some("auto".into()),
            include: vec!["web_search_call.action.sources".into()],
            input,
        };
        let req = self.post_json(responses_url).json(&response_request);
        let response = send_with_retry(req, "openai_web").await?;
        let status = response.status();
        let body = response.text().await.map_err(LlmError::from)?;

        if status.is_success() {
            let response: ResponsesResponse =
                serde_json::from_str(&body).map_err(|error| LlmError::ParseError {
                    message: error.to_string(),
                })?;
            return Ok(response.into_web_result());
        }

        // Some OpenAI-compatible local servers expose web search through
        // `/chat/completions` using `responses_tools` rather than `/responses`.
        if should_fallback_to_chat_completions(status, &body) {
            return self
                .run_chat_completions_request(
                    response_request.model,
                    response_request.input,
                    response_request.tools,
                )
                .await;
        }

        Err(LlmError::ProviderHttp {
            status: status.as_u16(),
            body,
        }
        .into())
    }

    async fn run_chat_completions_request(
        &self,
        model: String,
        input: String,
        tools: Vec<WebSearchTool>,
    ) -> anyhow::Result<WebResult> {
        let url = format!(
            "{}/chat/completions",
            self.config.base_url.trim_end_matches('/')
        );
        let request = ChatCompletionsWebRequest {
            model,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: input,
            }],
            responses_tools: chat_completions_web_tools(&tools),
            responses_tool_choice: "auto".into(),
            stream: false,
        };

        let req = self.post_json(url).json(&request);
        let response = send_with_retry(req, "openai_web_chat_compat").await?;
        let status = response.status();
        let body = response.text().await.map_err(LlmError::from)?;

        if !status.is_success() {
            return Err(LlmError::ProviderHttp {
                status: status.as_u16(),
                body,
            }
            .into());
        }

        let response: ChatCompletionsWebResponse =
            serde_json::from_str(&body).map_err(|error| LlmError::ParseError {
                message: error.to_string(),
            })?;
        Ok(response.into_web_result())
    }
}

#[async_trait]
impl WebProvider for OpenAiWebProvider {
    async fn search(&self, request: WebSearchRequest) -> anyhow::Result<WebResult> {
        self.run_request(
            request.query,
            request.allowed_domains,
            request.user_location,
            request.external_web_access,
        )
        .await
    }

    async fn open(&self, request: WebOpenRequest) -> anyhow::Result<WebResult> {
        let url = request.url.trim();
        if url.is_empty() {
            anyhow::bail!("missing url")
        }

        let input = match request.prompt {
            Some(prompt) if !prompt.trim().is_empty() => format!(
                "Open and inspect this URL: {url}\n\nFocus request: {}",
                prompt.trim()
            ),
            _ => format!(
                "Open and inspect this URL: {url}\n\nSummarize the page and include citations."
            ),
        };

        self.run_request(input, Vec::new(), None, request.external_web_access)
            .await
    }
}

#[derive(Serialize)]
struct ResponsesRequest {
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningConfig>,
    tools: Vec<WebSearchTool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    input: String,
}

#[derive(Serialize)]
struct ChatCompletionsWebRequest {
    model: String,
    messages: Vec<ChatMessage>,
    responses_tools: Vec<ChatCompletionsWebTool>,
    responses_tool_choice: String,
    stream: bool,
}

#[derive(Serialize)]
struct ChatCompletionsWebTool {
    r#type: String,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ReasoningConfig {
    effort: String,
}

#[derive(Serialize)]
struct WebSearchTool {
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    filters: Option<WebSearchFilters>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_location: Option<UserLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    external_web_access: Option<bool>,
}

#[derive(Serialize)]
struct WebSearchFilters {
    allowed_domains: Vec<String>,
}

#[derive(Serialize)]
struct UserLocation {
    r#type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    city: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
}

impl From<WebUserLocation> for UserLocation {
    fn from(value: WebUserLocation) -> Self {
        Self {
            r#type: "approximate".into(),
            country: value.country,
            city: value.city,
            region: value.region,
            timezone: value.timezone,
        }
    }
}

#[derive(Deserialize)]
struct ResponsesResponse {
    #[serde(default)]
    output: Vec<ResponseItem>,
}

#[derive(Deserialize)]
struct ChatCompletionsWebResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
}

impl ChatCompletionsWebResponse {
    fn into_web_result(self) -> WebResult {
        let mut result = WebResult::default();
        for choice in self.choices {
            if !result.text.is_empty() && !choice.message.content.is_empty() {
                result.text.push('\n');
            }
            result.text.push_str(&choice.message.content);
        }
        result
    }
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatChoiceMessage,
}

#[derive(Deserialize)]
struct ChatChoiceMessage {
    #[serde(default)]
    content: String,
}

impl ResponsesResponse {
    fn into_web_result(self) -> WebResult {
        let mut result = WebResult::default();

        for item in self.output {
            match item {
                ResponseItem::Message { content, .. } => {
                    for part in content {
                        if part.r#type == "output_text" {
                            if !result.text.is_empty() && !part.text.is_empty() {
                                result.text.push('\n');
                            }
                            result.text.push_str(&part.text);
                            result
                                .citations
                                .extend(part.annotations.into_iter().filter_map(|annotation| {
                                    if annotation.r#type == "url_citation" {
                                        annotation.url_citation.map(|citation| WebCitation {
                                            title: citation.title,
                                            url: citation.url,
                                            start_index: citation.start_index,
                                            end_index: citation.end_index,
                                        })
                                    } else {
                                        None
                                    }
                                }));
                        }
                    }
                }
                ResponseItem::WebSearchCall { action, .. } => {
                    if let Some(action) = action {
                        result
                            .sources
                            .extend(action.sources.into_iter().map(|source| WebSource {
                                title: source.title,
                                url: source.url,
                            }));
                    }
                }
                ResponseItem::Other => {}
            }
        }

        dedupe_citations(&mut result.citations);
        dedupe_sources(&mut result.sources);
        result
    }
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum ResponseItem {
    #[serde(rename = "message")]
    Message { content: Vec<OutputContentPart> },
    #[serde(rename = "web_search_call")]
    WebSearchCall {
        #[serde(default)]
        action: Option<WebSearchAction>,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct OutputContentPart {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    annotations: Vec<OutputAnnotation>,
}

#[derive(Deserialize)]
struct OutputAnnotation {
    #[serde(rename = "type")]
    r#type: String,
    #[serde(default)]
    url_citation: Option<UrlCitation>,
}

#[derive(Deserialize)]
struct UrlCitation {
    #[serde(default)]
    title: Option<String>,
    url: String,
    #[serde(default)]
    start_index: Option<u64>,
    #[serde(default)]
    end_index: Option<u64>,
}

#[derive(Deserialize, Default)]
struct WebSearchAction {
    #[serde(default)]
    sources: Vec<WebSearchSource>,
}

#[derive(Deserialize)]
struct WebSearchSource {
    #[serde(default)]
    title: Option<String>,
    url: String,
}

fn dedupe_citations(citations: &mut Vec<WebCitation>) {
    let mut seen = std::collections::HashSet::new();
    citations.retain(|citation| seen.insert(citation.url.clone()));
}

fn dedupe_sources(sources: &mut Vec<WebSource>) {
    let mut seen = std::collections::HashSet::new();
    sources.retain(|source| seen.insert(source.url.clone()));
}

fn chat_completions_web_tools(tools: &[WebSearchTool]) -> Vec<ChatCompletionsWebTool> {
    tools
        .iter()
        .map(|_| ChatCompletionsWebTool {
            r#type: "web_search_preview".into(),
        })
        .collect()
}

fn should_fallback_to_chat_completions(status: reqwest::StatusCode, body: &str) -> bool {
    if matches!(
        status,
        reqwest::StatusCode::NOT_FOUND
            | reqwest::StatusCode::METHOD_NOT_ALLOWED
            | reqwest::StatusCode::NOT_IMPLEMENTED
    ) {
        return true;
    }

    status == reqwest::StatusCode::BAD_REQUEST
        && response_error_code_is(body, "RESPONSES_TOOLS_REJECTED")
}

fn response_error_code_is(body: &str, expected: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("error")
                .and_then(|error| error.get("code"))
                .and_then(|code| code.as_str())
                .map(|code| code.eq_ignore_ascii_case(expected))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_message_and_sources_from_responses_payload() {
        let payload = serde_json::json!({
            "output": [
                {
                    "type": "web_search_call",
                    "action": {
                        "sources": [
                            { "title": "Example", "url": "https://example.com" }
                        ]
                    }
                },
                {
                    "type": "message",
                    "content": [
                        {
                            "type": "output_text",
                            "text": "Answer [1]",
                            "annotations": [
                                {
                                    "type": "url_citation",
                                    "url_citation": {
                                        "title": "Example",
                                        "url": "https://example.com",
                                        "start_index": 7,
                                        "end_index": 10
                                    }
                                }
                            ]
                        }
                    ]
                }
            ]
        });

        let response: ResponsesResponse = serde_json::from_value(payload).unwrap();
        let result = response.into_web_result();

        assert_eq!(result.text, "Answer [1]");
        assert_eq!(result.citations.len(), 1);
        assert_eq!(result.sources.len(), 1);
        assert_eq!(result.sources[0].url, "https://example.com");
    }

    #[test]
    fn parses_chat_completions_compat_payload() {
        let payload = serde_json::json!({
            "choices": [
                {
                    "message": {
                        "role": "assistant",
                        "content": "Current news summary"
                    }
                }
            ]
        });

        let response: ChatCompletionsWebResponse = serde_json::from_value(payload).unwrap();
        let result = response.into_web_result();
        assert_eq!(result.text, "Current news summary");
        assert!(result.citations.is_empty());
        assert!(result.sources.is_empty());
    }

    #[test]
    fn chat_completions_tool_payload_uses_preview_type() {
        let tools = chat_completions_web_tools(&[WebSearchTool {
            r#type: "web_search".into(),
            filters: None,
            user_location: None,
            external_web_access: Some(true),
        }]);

        let value = serde_json::to_value(tools).unwrap();
        assert_eq!(value, serde_json::json!([{ "type": "web_search_preview" }]));
    }

    #[test]
    fn chat_completions_fallback_is_limited_to_missing_responses_endpoint() {
        assert!(should_fallback_to_chat_completions(
            reqwest::StatusCode::NOT_FOUND,
            ""
        ));
        assert!(should_fallback_to_chat_completions(
            reqwest::StatusCode::METHOD_NOT_ALLOWED,
            ""
        ));
        assert!(should_fallback_to_chat_completions(
            reqwest::StatusCode::NOT_IMPLEMENTED,
            ""
        ));
        assert!(should_fallback_to_chat_completions(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"RESPONSES_TOOLS_REJECTED","message":"Upstream error"}}"#
        ));
        assert!(!should_fallback_to_chat_completions(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"code":"OTHER_BAD_REQUEST","message":"invalid request"}}"#
        ));
        assert!(!should_fallback_to_chat_completions(
            reqwest::StatusCode::UNAUTHORIZED,
            ""
        ));
    }
}
