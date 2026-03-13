use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl WebFetchTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
        }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "WebFetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a URL. Returns the page content as plain text (HTML tags stripped). Useful for reading documentation, API responses, or web pages."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch"
                }
            },
            "required": ["url"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let url = arguments["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("url is required"))?;

        // Only allow http:// and https:// schemes to prevent SSRF via file://, etc.
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolOutput {
                success: false,
                output: "Only http:// and https:// URLs are supported.".to_string(),
            });
        }

        let response = match self.client.get(url).send().await {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Failed to fetch URL: {}", e),
                });
            }
        };

        if !response.status().is_success() {
            return Ok(ToolOutput {
                success: false,
                output: format!("HTTP error: {}", response.status()),
            });
        }

        let body = match response.text().await {
            Ok(t) => t,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Failed to read response body: {}", e),
                });
            }
        };

        // Strip HTML tags with a simple regex
        let text = strip_html_tags(&body);

        // Truncate to 100K chars
        let max_len = 100_000;
        let text = if text.len() > max_len {
            format!(
                "{}\n\n... truncated ({} chars total)",
                {
                    let mut end = max_len;
                    while end > 0 && !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    &text[..end]
                },
                text.len()
            )
        } else {
            text
        };

        Ok(ToolOutput {
            success: true,
            output: text,
        })
    }
}

/// Simple HTML tag stripping using regex.
pub fn strip_html_tags(html: &str) -> String {
    // Remove script and style blocks entirely
    let re_script = regex::Regex::new(r"(?is)<script[^>]*>.*?</script>").unwrap();
    let text = re_script.replace_all(html, "");
    let re_style = regex::Regex::new(r"(?is)<style[^>]*>.*?</style>").unwrap();
    let text = re_style.replace_all(&text, "");

    // Remove HTML tags
    let re_tags = regex::Regex::new(r"<[^>]+>").unwrap();
    let text = re_tags.replace_all(&text, "");

    // Decode common HTML entities
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");

    // Collapse multiple blank lines
    let re_blanks = regex::Regex::new(r"\n{3,}").unwrap();
    re_blanks.replace_all(&text, "\n\n").trim().to_string()
}
