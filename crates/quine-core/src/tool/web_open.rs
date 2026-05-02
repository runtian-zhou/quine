use std::sync::Arc;

use async_trait::async_trait;
use quine_llm::{WebOpenRequest, WebProvider};

use super::web_search::render_web_result;
use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Tool for opening and summarizing a specific URL.
pub(crate) struct WebOpenTool {
    provider: Arc<dyn WebProvider>,
}

impl WebOpenTool {
    pub(crate) fn new(provider: Arc<dyn WebProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl Tool for WebOpenTool {
    fn name(&self) -> &str {
        "web_open"
    }

    fn description(&self) -> &str {
        "Open a specific URL on the web and return a cited summary of the page content."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The absolute URL to inspect."
                },
                "prompt": {
                    "type": "string",
                    "description": "Optional focus prompt describing what to extract from the page."
                },
                "external_web_access": {
                    "type": "boolean",
                    "description": "Whether to permit live internet access. Defaults to true."
                }
            },
            "required": ["url"]
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
        let url = arguments
            .get("url")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: url".into(),
            })?;

        let result = self
            .provider
            .open(WebOpenRequest {
                url: url.to_string(),
                prompt: arguments
                    .get("prompt")
                    .and_then(|value| value.as_str())
                    .map(ToOwned::to_owned),
                external_web_access: arguments
                    .get("external_web_access")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
            })
            .await
            .map_err(|error| ToolError::Internal {
                message: format!("web open failed: {error}"),
            })?;

        Ok(ToolOutput::success(render_web_result(&result)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::NullFilesystem;
    use crate::session::SessionId;

    struct StubWebProvider;

    #[async_trait]
    impl WebProvider for StubWebProvider {
        async fn search(
            &self,
            _request: quine_llm::WebSearchRequest,
        ) -> anyhow::Result<quine_llm::WebResult> {
            anyhow::bail!("not used")
        }

        async fn open(&self, request: WebOpenRequest) -> anyhow::Result<quine_llm::WebResult> {
            Ok(quine_llm::WebResult {
                text: format!("Opened {}", request.url),
                citations: Vec::new(),
                sources: Vec::new(),
            })
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
            permission_runtime: None,
            cancellation: crate::tool::CancellationChannel::never(),
        }
    }

    #[tokio::test]
    async fn web_open_requires_url() {
        let tool = WebOpenTool::new(Arc::new(StubWebProvider));
        let error = tool.execute(serde_json::json!({}), &make_context()).await;
        assert!(matches!(error, Err(ToolError::InvalidArguments { .. })));
    }
}
