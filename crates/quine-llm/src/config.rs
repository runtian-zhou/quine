use crate::anthropic::{AnthropicConfig, AnthropicProvider};
use crate::openai_compat::{OpenAiCompatConfig, OpenAiCompatProvider};
use crate::openai_web::{OpenAiWebConfig, OpenAiWebProvider};
use crate::provider::LlmProvider;
use crate::web::{NoopWebProvider, WebProvider};

/// Configuration for selecting and configuring an LLM provider.
#[derive(Debug, Clone)]
pub enum ProviderConfig {
    /// OpenAI-compatible API (covers OpenAI, LM Studio, ollama, vLLM, etc.).
    OpenAiCompat(OpenAiCompatConfig),
    /// Anthropic Messages API.
    Anthropic(AnthropicConfig),
}

/// Create a provider instance from configuration.
pub fn create_provider(config: ProviderConfig) -> Box<dyn LlmProvider> {
    match config {
        ProviderConfig::OpenAiCompat(c) => Box::new(OpenAiCompatProvider::new(c)),
        ProviderConfig::Anthropic(c) => Box::new(AnthropicProvider::new(c)),
    }
}

/// Configuration for selecting and configuring a web provider.
#[derive(Debug, Clone)]
pub enum WebProviderConfig {
    /// OpenAI Responses API `web_search` tool.
    OpenAi(OpenAiWebConfig),
    /// A disabled placeholder provider.
    None,
}

/// Create a web provider instance from configuration.
pub fn create_web_provider(config: WebProviderConfig) -> Box<dyn WebProvider> {
    match config {
        WebProviderConfig::OpenAi(c) => Box::new(OpenAiWebProvider::new(c)),
        WebProviderConfig::None => Box::new(NoopWebProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_openai_compat_provider() {
        let config = ProviderConfig::OpenAiCompat(OpenAiCompatConfig {
            base_url: "http://127.0.0.1:8000/v1".into(),
            api_key: None,
            model: "gpt-5.4".into(),
            max_tokens: Some(2048),
            parallel_tool_calls: false,
        });
        let _provider = create_provider(config);
    }

    #[test]
    fn create_anthropic_provider() {
        let config = ProviderConfig::Anthropic(AnthropicConfig {
            api_key: "test-key".into(),
            base_url: "https://api.anthropic.com".into(),
            model: "claude-sonnet-4-20250514".into(),
            max_tokens: 4096,
        });
        let _provider = create_provider(config);
    }
}
