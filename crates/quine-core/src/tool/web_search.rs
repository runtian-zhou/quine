use std::sync::Arc;

use async_trait::async_trait;
use quine_llm::{WebProvider, WebSearchRequest, WebUserLocation};

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Tool for live web search with citations.
pub(crate) struct WebSearchTool {
    provider: Arc<dyn WebProvider>,
}

impl WebSearchTool {
    pub(crate) fn new(provider: Arc<dyn WebProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web and return a cited answer with the consulted sources. \
         Use this for current events, live facts, or external documentation."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The web search query."
                },
                "q": {
                    "type": "string",
                    "description": "Alias for `query`."
                },
                "allowed_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional allow-list of domains to constrain search results."
                },
                "country": {
                    "type": "string",
                    "description": "Optional two-letter ISO country code for result localization."
                },
                "city": {
                    "type": "string",
                    "description": "Optional city for localized results."
                },
                "region": {
                    "type": "string",
                    "description": "Optional region/state for localized results."
                },
                "timezone": {
                    "type": "string",
                    "description": "Optional IANA timezone for localized results."
                },
                "external_web_access": {
                    "type": "boolean",
                    "description": "Whether to permit live internet access. Defaults to true."
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_idempotent(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let query = arguments
            .get("query")
            .or_else(|| arguments.get("q"))
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: query (or q)".into(),
            })?;

        let allowed_domains = parse_allowed_domains(&arguments)?;
        let request = WebSearchRequest {
            query: query.to_string(),
            allowed_domains,
            user_location: parse_user_location(&arguments),
            external_web_access: arguments
                .get("external_web_access")
                .and_then(|value| value.as_bool())
                .unwrap_or(true),
        };

        let result = self
            .provider
            .search(request)
            .await
            .map_err(|error| ToolError::Internal {
                message: format!("web search failed: {error}"),
            })?;

        Ok(ToolOutput::success(render_web_result(&result)))
    }
}

fn parse_allowed_domains(arguments: &serde_json::Value) -> Result<Vec<String>, ToolError> {
    let Some(domains) = arguments.get("allowed_domains") else {
        return Ok(Vec::new());
    };
    let values = domains
        .as_array()
        .ok_or_else(|| ToolError::InvalidArguments {
            message: "allowed_domains must be an array of strings".into(),
        })?;

    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: "allowed_domains must contain only non-empty strings".into(),
                })
        })
        .collect()
}

fn parse_user_location(arguments: &serde_json::Value) -> Option<WebUserLocation> {
    let country = arguments
        .get("country")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let city = arguments
        .get("city")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let region = arguments
        .get("region")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    let timezone = arguments
        .get("timezone")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);

    if country.is_none() && city.is_none() && region.is_none() && timezone.is_none() {
        None
    } else {
        Some(WebUserLocation {
            country,
            city,
            region,
            timezone,
        })
    }
}

pub(crate) fn render_web_result(result: &quine_llm::WebResult) -> String {
    let mut output = String::new();
    output.push_str(result.text.trim());

    if !result.citations.is_empty() {
        if !output.is_empty() {
            output.push_str("\n\n");
        }
        output.push_str("Citations:\n");
        for citation in &result.citations {
            let title = citation.title.as_deref().unwrap_or("Untitled");
            output.push_str(&format!("- {title}: {}\n", citation.url));
        }
    }

    if !result.sources.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str("Sources:\n");
        for source in &result.sources {
            let title = source.title.as_deref().unwrap_or("Untitled");
            output.push_str(&format!("- {title}: {}\n", source.url));
        }
    }

    output.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::NullFilesystem;
    use crate::session::SessionId;
    use quine_llm::{WebCitation, WebResult, WebSource};

    struct StubWebProvider {
        result: WebResult,
    }

    #[async_trait]
    impl WebProvider for StubWebProvider {
        async fn search(&self, _request: WebSearchRequest) -> anyhow::Result<WebResult> {
            Ok(self.result.clone())
        }

        async fn open(&self, _request: quine_llm::WebOpenRequest) -> anyhow::Result<WebResult> {
            anyhow::bail!("not used")
        }
    }

    fn make_context() -> ExecutionContext {
        ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(NullFilesystem),
            working_directory: std::path::PathBuf::new(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        }
    }

    #[tokio::test]
    async fn web_search_formats_result() {
        let tool = WebSearchTool::new(Arc::new(StubWebProvider {
            result: WebResult {
                text: "Fresh answer [1]".into(),
                citations: vec![WebCitation {
                    title: Some("Example".into()),
                    url: "https://example.com".into(),
                    start_index: None,
                    end_index: None,
                }],
                sources: vec![WebSource {
                    title: Some("Example".into()),
                    url: "https://example.com".into(),
                }],
            },
        }));

        let output = tool
            .execute(
                serde_json::json!({ "query": "latest example" }),
                &make_context(),
            )
            .await
            .unwrap();

        assert!(output.content.contains("Fresh answer"));
        assert!(output.content.contains("Citations:"));
        assert!(output.content.contains("Sources:"));
    }
}
