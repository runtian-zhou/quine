use std::sync::Arc;

use quine_llm::anthropic::AnthropicProvider;
use quine_llm::openai::OpenAiProvider;
use quine_core::provider::LlmProvider;

pub fn create_provider(
    provider_name: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Arc<dyn LlmProvider>> {
    match provider_name {
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY environment variable not set"))?;
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("ANTHROPIC_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY environment variable not set"))?;
            let mut provider = OpenAiProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("OPENAI_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        _ => anyhow::bail!(
            "Unknown provider: {}. Use 'anthropic' or 'openai'.",
            provider_name
        ),
    }
}
