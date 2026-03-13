use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct WebSearchTool {
    client: reqwest::Client,
}

impl WebSearchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "WebSearch"
    }

    fn description(&self) -> &str {
        "Search the web using Brave Search. Returns titles, URLs, and snippets. Requires BRAVE_SEARCH_API_KEY environment variable."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "count": {
                    "type": "integer",
                    "description": "Number of results to return (default: 5, max: 20)"
                }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let query = arguments["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("query is required"))?;

        let count = arguments["count"].as_u64().unwrap_or(5).min(20);

        let api_key = match std::env::var("BRAVE_SEARCH_API_KEY") {
            Ok(key) => key,
            Err(_) => {
                return Ok(ToolOutput {
                    success: false,
                    output: "BRAVE_SEARCH_API_KEY environment variable not set. Set it to use web search.".to_string(),
                });
            }
        };

        let response = match self
            .client
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", &api_key)
            .query(&[("q", query), ("count", &count.to_string())])
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Search request failed: {}", e),
                });
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Ok(ToolOutput {
                success: false,
                output: format!("Brave Search API error ({}): {}", status, body),
            });
        }

        let body: BraveSearchResponse = match response.json().await {
            Ok(b) => b,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Failed to parse search response: {}", e),
                });
            }
        };

        let results = body
            .web
            .map(|w| w.results)
            .unwrap_or_default();

        if results.is_empty() {
            return Ok(ToolOutput {
                success: true,
                output: "No results found.".to_string(),
            });
        }

        let mut output = String::new();
        for (i, result) in results.iter().enumerate() {
            output.push_str(&format!(
                "{}. {}\n   {}\n   {}\n\n",
                i + 1,
                result.title,
                result.url,
                result.description.as_deref().unwrap_or(""),
            ));
        }

        Ok(ToolOutput {
            success: true,
            output: output.trim_end().to_string(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct BraveSearchResponse {
    web: Option<BraveWebResults>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResults {
    results: Vec<BraveWebResult>,
}

#[derive(Debug, Deserialize)]
struct BraveWebResult {
    title: String,
    url: String,
    description: Option<String>,
}
