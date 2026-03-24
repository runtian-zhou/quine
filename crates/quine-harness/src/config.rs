use std::path::PathBuf;

use quine_core::permission::composite::CompositeChecker;
use quine_core::permission::llm_checker::LlmChecker;
use quine_core::permission::rule_checker::RuleBasedChecker;
use quine_core::PermissionChecker;
use quine_llm::config::ProviderConfig;
use quine_llm::openai_compat::OpenAiCompatConfig;
use serde::{Deserialize, Serialize};

/// Configuration for a single agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Optional system prompt override.
    pub system_prompt: Option<String>,
    /// Optional working directory for the session filesystem.
    pub working_directory: Option<std::path::PathBuf>,
}

/// Configuration for the harness daemon.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Path to the Unix domain socket for IPC.
    pub socket_path: PathBuf,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
        }
    }
}

/// Returns the default path for the harness Unix domain socket.
///
/// Uses `$XDG_RUNTIME_DIR/quine/harness.sock` if available,
/// otherwise falls back to `/tmp/quine-harness.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(runtime_dir).join("quine");
        dir.join("harness.sock")
    } else {
        PathBuf::from("/tmp/quine-harness.sock")
    }
}

/// Build an LLM `ProviderConfig` from environment variables.
///
/// Uses `LLM_BASE_URL`, `LLM_API_KEY`, and `LLM_MODEL` env vars,
/// defaulting to a local OpenAI-compatible endpoint.
/// Build an LLM `ProviderConfig` from environment variables.
fn config_from_env() -> ProviderConfig {
    ProviderConfig::OpenAiCompat(OpenAiCompatConfig {
        base_url: std::env::var("LLM_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into()),
        api_key: std::env::var("LLM_API_KEY").ok(),
        model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "qwen-3.5".into()),
        max_tokens: Some(4096),
    })
}

/// Create a boxed LLM provider from environment variables.
///
/// Convenience function that combines `config_from_env` with
/// `quine_llm::config::create_provider`.
pub fn create_provider_from_env() -> Box<dyn quine_llm::LlmProvider> {
    quine_llm::config::create_provider(config_from_env())
}

/// Create the default permission checker from environment configuration.
///
/// Always includes `RuleBasedChecker`. Optionally includes `LlmChecker` when
/// `PERMISSION_LLM_ENABLED=true` is set in the environment.
pub fn create_default_permission_checker() -> Box<dyn PermissionChecker> {
    let mut checkers: Vec<Box<dyn PermissionChecker>> = vec![Box::new(RuleBasedChecker::new())];

    if std::env::var("PERMISSION_LLM_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        let provider = create_provider_from_env();
        checkers.push(Box::new(LlmChecker::new(provider)));
    }

    Box::new(CompositeChecker::new(checkers))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_socket_path() {
        let config = HarnessConfig::default();
        assert!(!config.socket_path.as_os_str().is_empty());
    }

    #[test]
    fn session_config_default() {
        let config = SessionConfig::default();
        assert!(config.system_prompt.is_none());
    }

    #[test]
    fn session_config_serialization() {
        let config = SessionConfig {
            system_prompt: Some("You are helpful.".into()),
            working_directory: None,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SessionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.system_prompt.as_deref(),
            Some("You are helpful.")
        );
    }
}
