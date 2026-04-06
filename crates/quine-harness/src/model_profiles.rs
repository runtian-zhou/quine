use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use quine_llm::anthropic::AnthropicConfig;
use quine_llm::config::ProviderConfig;
use quine_llm::openai_compat::OpenAiCompatConfig;
use quine_llm::LlmProvider;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct ResolvedProviderSelection {
    pub provider: Arc<dyn LlmProvider>,
    pub max_context_window: Option<u64>,
    pub model_profile: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
struct ModelProfilesDocument {
    #[serde(default)]
    profiles: HashMap<String, ModelProfileDefinition>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct ModelProfileDefinition {
    provider: String,
    model: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    parallel_tool_calls: Option<bool>,
}

fn model_profiles_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".quine").join("model-profiles.yaml"))
}

fn load_model_profiles_document() -> anyhow::Result<ModelProfilesDocument> {
    let Some(path) = model_profiles_path() else {
        return Ok(ModelProfilesDocument::default());
    };
    if !path.exists() {
        return Ok(ModelProfilesDocument::default());
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    serde_yaml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))
}

fn resolve_api_key(
    explicit: Option<String>,
    env_name: Option<&str>,
    fallback_env_names: &[&str],
) -> anyhow::Result<Option<String>> {
    if let Some(api_key) = explicit {
        return Ok(Some(api_key));
    }
    if let Some(env_name) = env_name {
        return std::env::var(env_name)
            .map(Some)
            .map_err(|_| anyhow::anyhow!("required env var not set: {env_name}"));
    }
    for env_name in fallback_env_names {
        if let Ok(value) = std::env::var(env_name) {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

fn parse_bool_env(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn default_parallel_tool_calls(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("gpt-5.4")
}

pub fn max_context_window_for_provider_config(config: &ProviderConfig) -> Option<u64> {
    match config {
        ProviderConfig::Anthropic(config) => anthropic_context_window(&config.model),
        ProviderConfig::OpenAiCompat(config) => openai_compat_context_window(&config.model),
    }
}

fn anthropic_context_window(model: &str) -> Option<u64> {
    let normalized = model.to_ascii_lowercase();
    if normalized.starts_with("claude") {
        Some(200_000)
    } else {
        None
    }
}

fn openai_compat_context_window(model: &str) -> Option<u64> {
    let normalized = model.to_ascii_lowercase();
    if normalized.starts_with("gpt-4.1") || normalized.starts_with("gpt-4o") {
        Some(128_000)
    } else if normalized.starts_with("o1") || normalized.starts_with("o3") {
        Some(200_000)
    } else if normalized.starts_with("qwen") {
        Some(131_072)
    } else if normalized.starts_with("llama-3.1") || normalized.starts_with("llama3.1") {
        Some(128_000)
    } else {
        Some(250_000)
    }
}

pub fn provider_config_from_env() -> ProviderConfig {
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".into());
    if provider.eq_ignore_ascii_case("anthropic") {
        let api_key = std::env::var("LLM_API_KEY")
            .or_else(|_| std::env::var("ANTHROPIC_API_KEY"))
            .expect("LLM_API_KEY or ANTHROPIC_API_KEY must be set for Anthropic provider");
        ProviderConfig::Anthropic(AnthropicConfig {
            api_key,
            base_url: std::env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "https://api.anthropic.com".into()),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "claude-sonnet-4-20250514".into()),
            max_tokens: 4096,
        })
    } else {
        ProviderConfig::OpenAiCompat(OpenAiCompatConfig {
            base_url: std::env::var("LLM_BASE_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8000/v1".into()),
            api_key: std::env::var("LLM_API_KEY").ok(),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-5.4".into()),
            max_tokens: Some(4096),
            parallel_tool_calls: std::env::var("LLM_PARALLEL_TOOL_CALLS")
                .ok()
                .and_then(|value| parse_bool_env(&value))
                .unwrap_or_else(|| {
                    default_parallel_tool_calls(
                        &std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-5.4".into()),
                    )
                }),
        })
    }
}

pub fn create_provider_from_config(config: ProviderConfig) -> Arc<dyn LlmProvider> {
    Arc::from(quine_llm::config::create_provider(config))
}

pub fn resolve_env_provider_selection() -> ResolvedProviderSelection {
    let provider_config = provider_config_from_env();
    ResolvedProviderSelection {
        provider: create_provider_from_config(provider_config.clone()),
        max_context_window: max_context_window_for_provider_config(&provider_config),
        model_profile: None,
    }
}

pub fn resolve_named_model_profile(
    profile_name: &str,
) -> anyhow::Result<ResolvedProviderSelection> {
    let document = load_model_profiles_document()?;
    let profile = document
        .profiles
        .get(profile_name)
        .ok_or_else(|| anyhow::anyhow!("unknown model profile: {profile_name}"))?;
    let provider_config = provider_config_from_profile(profile)?;
    Ok(ResolvedProviderSelection {
        provider: create_provider_from_config(provider_config.clone()),
        max_context_window: max_context_window_for_provider_config(&provider_config),
        model_profile: Some(profile_name.to_string()),
    })
}

fn provider_config_from_profile(
    profile: &ModelProfileDefinition,
) -> anyhow::Result<ProviderConfig> {
    if profile.provider.eq_ignore_ascii_case("anthropic") {
        let api_key = resolve_api_key(
            profile.api_key.clone(),
            profile.api_key_env.as_deref(),
            &["LLM_API_KEY", "ANTHROPIC_API_KEY"],
        )?
        .ok_or_else(|| anyhow::anyhow!("anthropic profile requires an API key"))?;
        return Ok(ProviderConfig::Anthropic(AnthropicConfig {
            api_key,
            base_url: profile
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.anthropic.com".into()),
            model: profile.model.clone(),
            max_tokens: profile.max_tokens.unwrap_or(4096),
        }));
    }

    if profile.provider.eq_ignore_ascii_case("openai")
        || profile.provider.eq_ignore_ascii_case("openai_compat")
        || profile.provider.eq_ignore_ascii_case("openai-compatible")
    {
        let api_key = resolve_api_key(
            profile.api_key.clone(),
            profile.api_key_env.as_deref(),
            &["LLM_API_KEY", "OPENAI_API_KEY"],
        )?;
        return Ok(ProviderConfig::OpenAiCompat(OpenAiCompatConfig {
            base_url: profile
                .base_url
                .clone()
                .unwrap_or_else(|| "http://127.0.0.1:8000/v1".into()),
            api_key,
            model: profile.model.clone(),
            max_tokens: profile.max_tokens,
            parallel_tool_calls: profile
                .parallel_tool_calls
                .unwrap_or_else(|| default_parallel_tool_calls(&profile.model)),
        }));
    }

    Err(anyhow::anyhow!(
        "unsupported provider '{}' in model profile",
        profile.provider
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{LazyLock, Mutex};
    use tempfile::TempDir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        f()
    }

    #[test]
    fn context_window_inference_matches_provider() {
        let config = ProviderConfig::OpenAiCompat(OpenAiCompatConfig {
            base_url: "http://127.0.0.1:8000/v1".into(),
            api_key: None,
            model: "qwen3-coder".into(),
            max_tokens: Some(2048),
            parallel_tool_calls: false,
        });
        assert_eq!(
            max_context_window_for_provider_config(&config),
            Some(131_072)
        );
    }

    #[test]
    fn resolve_named_openai_profile_from_yaml() {
        with_env_lock(|| {
            let home = TempDir::new().unwrap();
            let config_dir = home.path().join(".quine");
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(
                config_dir.join("model-profiles.yaml"),
                "profiles:\n  local-fast:\n    provider: openai\n    model: qwen3-coder\n    base_url: http://127.0.0.1:1234/v1\n",
            )
            .unwrap();
            let previous_home = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", home.path());
            }

            let selection = resolve_named_model_profile("local-fast").unwrap();
            assert_eq!(selection.model_profile.as_deref(), Some("local-fast"));
            assert_eq!(selection.max_context_window, Some(131_072));

            match previous_home {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        });
    }
}
