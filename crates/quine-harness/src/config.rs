use std::path::PathBuf;
use std::sync::Arc;

use quine_core::permission::composite::CompositeChecker;
use quine_core::permission::llm_checker::LlmChecker;
use quine_core::permission::rule_checker::RuleBasedChecker;
use quine_core::PermissionChecker;
use quine_llm::anthropic::AnthropicConfig;
use quine_llm::config::ProviderConfig;
use quine_llm::openai_compat::OpenAiCompatConfig;
use quine_llm::Message;
use serde::{Deserialize, Serialize};

/// Configuration for a single agent session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Optional system prompt override.
    pub system_prompt: Option<String>,
    /// Optional working directory for the session filesystem.
    pub working_directory: Option<std::path::PathBuf>,
    /// Skill names to load for this session.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Whether this session operates in read-only plan mode.
    #[serde(default)]
    pub plan_mode: bool,
    /// Seed the session with these messages after the system prompt.
    #[serde(default)]
    pub initial_messages: Vec<Message>,
    /// Whether bash permission prompts should be auto-approved for this session.
    #[serde(default)]
    pub auto_approve_permissions: bool,
}

/// Configuration for the harness daemon.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Path to the Unix domain socket for IPC.
    pub socket_path: PathBuf,
    /// Root directory for durable harness state such as checkpoints and compacted transcripts.
    pub state_dir: PathBuf,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            state_dir: default_state_dir(),
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

/// Returns the default path for durable harness state.
pub fn default_state_dir() -> PathBuf {
    if let Ok(state_home) = std::env::var("XDG_STATE_HOME") {
        PathBuf::from(state_home).join("state")
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".quine").join("state")
    } else {
        PathBuf::from("/tmp").join("quine-state")
    }
}

/// Build an LLM `ProviderConfig` from environment variables.
///
/// Uses `LLM_PROVIDER` to select the backend (`"anthropic"` or `"openai"`,
/// default `"openai"`), plus `LLM_BASE_URL`, `LLM_API_KEY`, and `LLM_MODEL`.
fn config_from_env() -> ProviderConfig {
    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".into());
    let config = if provider.eq_ignore_ascii_case("anthropic") {
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
                .unwrap_or_else(|_| "http://127.0.0.1:1234/v1".into()),
            api_key: std::env::var("LLM_API_KEY").ok(),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "qwen-3.5".into()),
            max_tokens: Some(4096),
        })
    };
    match &config {
        ProviderConfig::Anthropic(c) => {
            eprintln!(
                "[daemon] LLM provider: Anthropic, model={}, base_url={}",
                c.model, c.base_url
            );
        }
        ProviderConfig::OpenAiCompat(c) => {
            eprintln!(
                "[daemon] LLM provider: OpenAI-compat, model={}, base_url={}",
                c.model, c.base_url
            );
        }
    }
    config
}

/// Create an LLM provider from environment variables.
///
/// Convenience function that combines `config_from_env` with
/// `quine_llm::config::create_provider`.
pub fn create_provider_from_env() -> Arc<dyn quine_llm::LlmProvider> {
    Arc::from(quine_llm::config::create_provider(config_from_env()))
}

/// Resolve the configured model's max context window.
///
/// Prefers `LLM_CONTEXT_WINDOW` when set. Otherwise falls back to a small
/// built-in model lookup for known defaults.
pub fn max_context_window_from_env() -> Option<u64> {
    if let Ok(value) = std::env::var("LLM_CONTEXT_WINDOW") {
        if let Ok(parsed) = value.parse::<u64>() {
            return Some(parsed);
        }
    }

    let provider = std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "openai".into());
    let model = std::env::var("LLM_MODEL").unwrap_or_else(|_| {
        if provider.eq_ignore_ascii_case("anthropic") {
            "claude-sonnet-4-20250514".into()
        } else {
            "qwen-3.5".into()
        }
    });

    if provider.eq_ignore_ascii_case("anthropic") {
        return anthropic_context_window(&model);
    }

    openai_compat_context_window(&model)
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

/// Create the default permission checker from environment configuration.
///
/// Uses `LlmChecker` first when `PERMISSION_LLM_ENABLED=true` is set in the
/// environment. If the LLM marks a command as dangerous, a manual low-risk
/// allowlist in `RuleBasedChecker` may override that decision to allow it.
pub fn create_default_permission_checker() -> Arc<dyn PermissionChecker> {
    let llm_checker = if std::env::var("PERMISSION_LLM_ENABLED")
        .map(|v| v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
    {
        let provider = create_provider_from_env();
        Some(LlmChecker::new(provider))
    } else {
        None
    };

    Arc::new(CompositeChecker::new(llm_checker, RuleBasedChecker::new()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_socket_path() {
        let config = HarnessConfig::default();
        assert!(!config.socket_path.as_os_str().is_empty());
        assert!(!config.state_dir.as_os_str().is_empty());
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
            skills: Vec::new(),
            plan_mode: false,
            initial_messages: Vec::new(),
            auto_approve_permissions: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SessionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.system_prompt.as_deref(),
            Some("You are helpful.")
        );
        assert!(deserialized.auto_approve_permissions);
    }

    #[test]
    fn default_state_dir_prefers_quine_home_layout() {
        std::env::remove_var("XDG_STATE_HOME");
        std::env::set_var("HOME", "/tmp/quine-home");
        assert_eq!(
            default_state_dir(),
            PathBuf::from("/tmp/quine-home/.quine/state")
        );
        std::env::remove_var("HOME");
    }

    #[test]
    fn explicit_context_window_override_is_used() {
        std::env::set_var("LLM_CONTEXT_WINDOW", "65536");
        assert_eq!(max_context_window_from_env(), Some(65_536));
        std::env::remove_var("LLM_CONTEXT_WINDOW");
    }

    #[test]
    fn default_state_dir_is_non_empty() {
        assert!(!default_state_dir().as_os_str().is_empty());
    }
}
