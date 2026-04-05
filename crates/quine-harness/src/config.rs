use std::path::PathBuf;
use std::sync::Arc;

use quine_core::{MemoryPolicyConfig, PermissionPromptBehavior};
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
    /// How permission prompts should behave for this session.
    #[serde(default = "default_permission_prompt_behavior")]
    pub prompt_behavior: PermissionPromptBehavior,
    /// Seed the session with these messages after the system prompt.
    #[serde(default)]
    pub initial_messages: Vec<Message>,
    /// Optional custom agent memory scope key.
    #[serde(default)]
    pub agent_key: Option<String>,
    /// Optional team memory scope key.
    #[serde(default)]
    pub team_key: Option<String>,
    /// Memory scope and policy configuration for this session.
    #[serde(default)]
    pub memory_policy: MemoryPolicyConfig,
}

fn default_permission_prompt_behavior() -> PermissionPromptBehavior {
    PermissionPromptBehavior::Interactive
}

/// Configuration for the harness daemon.
#[derive(Debug, Clone)]
pub struct HarnessConfig {
    /// Path to the Unix domain socket for IPC.
    pub socket_path: PathBuf,
    /// Root directory for durable harness state such as checkpoints and compacted transcripts.
    pub state_dir: PathBuf,
    /// Root directory for project-scoped persistent memory.
    pub memory_dir: PathBuf,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        let state_dir = default_state_dir();
        Self {
            socket_path: default_socket_path(),
            memory_dir: default_memory_dir_from_state_dir(&state_dir),
            state_dir,
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

/// Returns the default root for durable project-scoped memory.
pub fn default_memory_dir() -> PathBuf {
    default_memory_dir_from_state_dir(&default_state_dir())
}

pub fn default_memory_dir_from_state_dir(state_dir: &std::path::Path) -> PathBuf {
    state_dir.join("memory")
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
                .unwrap_or_else(|_| "http://127.0.0.1:8000/v1".into()),
            api_key: std::env::var("LLM_API_KEY").ok(),
            model: std::env::var("LLM_MODEL").unwrap_or_else(|_| "gpt-5.4".into()),
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
            "gpt-5.4".into()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
            prompt_behavior: PermissionPromptBehavior::Headless,
            initial_messages: Vec::new(),
            agent_key: None,
            team_key: None,
            memory_policy: MemoryPolicyConfig::default(),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: SessionConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(
            deserialized.system_prompt.as_deref(),
            Some("You are helpful.")
        );
        assert_eq!(
            deserialized.prompt_behavior,
            PermissionPromptBehavior::Headless
        );
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

    #[test]
    fn default_memory_dir_extends_state_dir() {
        let state_dir = Path::new("/tmp/quine-state");
        assert_eq!(
            default_memory_dir_from_state_dir(state_dir),
            PathBuf::from("/tmp/quine-state/memory")
        );
    }
}
