use std::path::PathBuf;
use std::sync::Arc;

use quine_core::{
    default_status_report_min_tool_rounds, MemoryPolicyConfig, PermissionPromptBehavior,
    PermissionRule, PermissionRuleEffect, PermissionRuleSet, PermissionRuleSource,
    PermissionTarget, RuleScope, SessionLlmConfig,
};
use quine_llm::config::WebProviderConfig;
use quine_llm::openai_web::OpenAiWebConfig;
use quine_llm::Message;
use serde::{Deserialize, Serialize};

use crate::model_profiles::{
    max_context_window_for_provider_config, provider_config_from_env,
    resolve_env_provider_selection, resolve_named_model_profile,
};

/// Configuration for a single agent session.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Optional named model profile for this session.
    #[serde(default)]
    pub model_profile: Option<String>,
    /// Optional python session group for shared Python state.
    #[serde(default)]
    pub session_group: Option<String>,
    /// Auto-compaction threshold as a percentage of the model context window.
    #[serde(default = "default_auto_compact_threshold_percent")]
    pub auto_compact_threshold_percent: u8,
    /// Minimum number of tool rounds before status reporting begins.
    #[serde(default = "default_status_report_min_tool_rounds")]
    pub status_report_min_tool_rounds: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            system_prompt: None,
            working_directory: None,
            skills: Vec::new(),
            plan_mode: false,
            prompt_behavior: default_permission_prompt_behavior(),
            initial_messages: Vec::new(),
            agent_key: None,
            team_key: None,
            memory_policy: MemoryPolicyConfig::default(),
            model_profile: None,
            session_group: None,
            auto_compact_threshold_percent: default_auto_compact_threshold_percent(),
            status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
        }
    }
}

const DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT: u8 = 60;

fn default_auto_compact_threshold_percent() -> u8 {
    DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT
}

fn default_permission_prompt_behavior() -> PermissionPromptBehavior {
    PermissionPromptBehavior::Interactive
}

pub fn auto_compact_threshold_percent_from_env() -> u8 {
    std::env::var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .filter(|value| (1..=100).contains(value))
        .unwrap_or_else(default_auto_compact_threshold_percent)
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

#[derive(Debug, Deserialize)]
struct PermissionRuleDocument {
    #[serde(default)]
    rules: Vec<ConfiguredPermissionRule>,
}

#[derive(Debug, Deserialize, Default)]
struct UserConfigDocument {
    #[serde(default)]
    web_search: Option<ConfiguredWebSearch>,
}

#[derive(Debug, Clone, Deserialize)]
struct ConfiguredWebSearch {
    #[serde(default)]
    enabled: Option<bool>,
    #[serde(default)]
    provider: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ConfiguredPermissionRule {
    effect: PermissionRuleEffect,
    scope: quine_core::PermissionScope,
    target: PermissionTarget,
}

fn user_config_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".quine").join("config.yaml"))
}

fn user_permission_rules_path() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".quine").join("permissions.yaml"))
}

fn load_user_config_document() -> anyhow::Result<UserConfigDocument> {
    let Some(path) = user_config_path() else {
        return Ok(UserConfigDocument::default());
    };
    if !path.exists() {
        return Ok(UserConfigDocument::default());
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    serde_yaml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))
}

fn load_user_web_search_config() -> Option<ConfiguredWebSearch> {
    match load_user_config_document() {
        Ok(document) => document.web_search,
        Err(error) => {
            eprintln!("[daemon] ignoring user config: {error}");
            None
        }
    }
}

pub fn project_permission_rules_path(working_directory: &std::path::Path) -> PathBuf {
    working_directory.join(".quine").join("permissions.yaml")
}

fn parse_permission_rules_from_path(
    path: &std::path::Path,
    source: PermissionRuleSource,
) -> anyhow::Result<Vec<PermissionRule>> {
    let contents = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    let document: PermissionRuleDocument = serde_yaml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))?;

    let scope = match source {
        PermissionRuleSource::BuiltIn => RuleScope::Global,
        PermissionRuleSource::Session => RuleScope::Session,
        PermissionRuleSource::User => RuleScope::Global,
        PermissionRuleSource::Workspace => RuleScope::Workspace,
    };

    Ok(document
        .rules
        .into_iter()
        .map(|rule| PermissionRule {
            effect: rule.effect,
            scope,
            request_scope: Some(rule.scope),
            target: rule.target,
            source_path: Some(path.to_path_buf()),
        })
        .collect())
}

pub fn load_persisted_permission_rules(
    working_directory: &std::path::Path,
) -> anyhow::Result<PermissionRuleSet> {
    let mut rules = PermissionRuleSet::default();

    if let Some(user_path) = user_permission_rules_path().filter(|path| path.exists()) {
        rules.user = parse_permission_rules_from_path(&user_path, PermissionRuleSource::User)?;
    }

    let project_path = project_permission_rules_path(working_directory);
    if project_path.exists() {
        rules.workspace =
            parse_permission_rules_from_path(&project_path, PermissionRuleSource::Workspace)?;
    }

    Ok(rules)
}

/// Build an LLM `ProviderConfig` from environment variables.
///
/// Uses `LLM_PROVIDER` to select the backend (`"anthropic"` or `"openai"`,
/// default `"openai"`), plus `LLM_BASE_URL`, `LLM_API_KEY`, and `LLM_MODEL`.
fn log_provider_config(config: &quine_llm::config::ProviderConfig) {
    match &config {
        quine_llm::config::ProviderConfig::Anthropic(c) => {
            eprintln!(
                "[daemon] LLM provider: Anthropic, model={}, base_url={}",
                c.model, c.base_url
            );
        }
        quine_llm::config::ProviderConfig::OpenAiCompat(c) => {
            eprintln!(
                "[daemon] LLM provider: OpenAI-compat, model={}, base_url={}",
                c.model, c.base_url
            );
        }
    }
}

/// Create an LLM provider from environment variables.
///
/// Convenience function that combines `config_from_env` with
/// `quine_llm::config::create_provider`.
pub fn create_provider_from_env() -> Arc<dyn quine_llm::LlmProvider> {
    let selection = resolve_env_provider_selection();
    log_provider_config(&provider_config_from_env());
    selection.provider
}

pub fn resolve_session_llm_config(model_profile: Option<&str>) -> anyhow::Result<SessionLlmConfig> {
    let selection = match model_profile {
        Some(profile) => resolve_named_model_profile(profile)?,
        None => resolve_env_provider_selection(),
    };
    Ok(SessionLlmConfig {
        provider: selection.provider,
        max_context_window: selection.max_context_window,
        model_profile: selection.model_profile,
    })
}

fn web_config_from_env() -> WebProviderConfig {
    let user_config = load_user_web_search_config();
    let explicit_provider = std::env::var("WEB_PROVIDER").ok();
    if explicit_provider.is_none()
        && user_config
            .as_ref()
            .and_then(|config| config.enabled)
            .is_some_and(|enabled| !enabled)
    {
        return WebProviderConfig::None;
    }

    let explicit_web_base_url = std::env::var("WEB_BASE_URL").ok();
    let configured_web_base_url = user_config
        .as_ref()
        .and_then(|config| config.base_url.clone());
    let provider = explicit_provider
        .clone()
        .or_else(|| {
            user_config
                .as_ref()
                .and_then(|config| config.provider.clone())
        })
        .map(Ok)
        .unwrap_or_else(|| std::env::var("LLM_PROVIDER"))
        .unwrap_or_else(|_| "openai".into());
    if provider.eq_ignore_ascii_case("none") || provider.eq_ignore_ascii_case("disabled") {
        return WebProviderConfig::None;
    }

    if provider.eq_ignore_ascii_case("anthropic") {
        return WebProviderConfig::None;
    }

    let base_url = explicit_web_base_url
        .clone()
        .or_else(|| configured_web_base_url.clone())
        .map(Ok)
        .unwrap_or_else(|| std::env::var("LLM_BASE_URL"))
        .or_else(|_| std::env::var("OPENAI_BASE_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8000/v1".into());

    let api_key = std::env::var("WEB_API_KEY")
        .ok()
        .or_else(|| {
            user_config
                .as_ref()
                .and_then(|config| config.api_key.clone())
        })
        .or_else(|| {
            user_config
                .as_ref()
                .and_then(|config| config.api_key_env.as_deref())
                .and_then(|env_name| std::env::var(env_name).ok())
        })
        .or_else(|| std::env::var("LLM_API_KEY").ok())
        .or_else(|| std::env::var("OPENAI_API_KEY").ok());

    let explicitly_configured_web_endpoint =
        explicit_web_base_url.is_some() || configured_web_base_url.is_some();
    let explicitly_configured_web_provider = explicit_provider.is_some() || user_config.is_some();

    let api_key = match api_key {
        Some(api_key) => api_key,
        None if explicitly_configured_web_provider
            && explicitly_configured_web_endpoint
            && is_loopback_base_url(&base_url) =>
        {
            String::new()
        }
        None => {
            if explicit_provider.is_some() || user_config.is_some() {
                eprintln!(
                    "[daemon] web provider `{provider}` disabled because WEB_API_KEY, OPENAI_API_KEY, or LLM_API_KEY is not set"
                );
            }
            return WebProviderConfig::None;
        }
    };

    let model = std::env::var("WEB_MODEL")
        .ok()
        .or_else(|| user_config.as_ref().and_then(|config| config.model.clone()))
        .or_else(|| std::env::var("LLM_MODEL").ok())
        .unwrap_or_else(|| "gpt-5.4".into());

    WebProviderConfig::OpenAi(OpenAiWebConfig {
        base_url,
        api_key,
        model,
    })
}

fn is_loopback_base_url(base_url: &str) -> bool {
    let lower = base_url.to_ascii_lowercase();
    lower.starts_with("http://127.0.0.1")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
}

pub fn create_web_provider_from_env() -> Arc<dyn quine_llm::WebProvider> {
    Arc::from(quine_llm::config::create_web_provider(web_config_from_env()))
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
    max_context_window_for_provider_config(&provider_config_from_env())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::sync::{LazyLock, Mutex};
    use tempfile::TempDir;

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        f()
    }

    fn snapshot_env(names: &[&'static str]) -> Vec<(&'static str, Option<std::ffi::OsString>)> {
        names
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect()
    }

    fn restore_env(snapshot: Vec<(&'static str, Option<std::ffi::OsString>)>) {
        for (name, value) in snapshot {
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }

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
        assert_eq!(
            config.auto_compact_threshold_percent,
            default_auto_compact_threshold_percent()
        );
        assert_eq!(
            config.status_report_min_tool_rounds,
            default_status_report_min_tool_rounds()
        );
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
            model_profile: None,
            session_group: None,
            auto_compact_threshold_percent: default_auto_compact_threshold_percent(),
            status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
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
        assert_eq!(
            deserialized.auto_compact_threshold_percent,
            default_auto_compact_threshold_percent()
        );
    }

    #[test]
    fn default_state_dir_prefers_quine_home_layout() {
        with_env_lock(|| {
            let previous_xdg_state_home = std::env::var_os("XDG_STATE_HOME");
            let previous_home = std::env::var_os("HOME");
            unsafe {
                std::env::remove_var("XDG_STATE_HOME");
                std::env::set_var("HOME", "/tmp/quine-home");
            }
            assert_eq!(
                default_state_dir(),
                PathBuf::from("/tmp/quine-home/.quine/state")
            );
            match previous_xdg_state_home {
                Some(value) => unsafe { std::env::set_var("XDG_STATE_HOME", value) },
                None => unsafe { std::env::remove_var("XDG_STATE_HOME") },
            }
            match previous_home {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        });
    }

    #[test]
    fn explicit_context_window_override_is_used() {
        with_env_lock(|| {
            let previous_context_window = std::env::var_os("LLM_CONTEXT_WINDOW");
            unsafe {
                std::env::set_var("LLM_CONTEXT_WINDOW", "65536");
            }
            assert_eq!(max_context_window_from_env(), Some(65_536));
            match previous_context_window {
                Some(value) => unsafe { std::env::set_var("LLM_CONTEXT_WINDOW", value) },
                None => unsafe { std::env::remove_var("LLM_CONTEXT_WINDOW") },
            }
        });
    }

    #[test]
    fn auto_compact_threshold_percent_from_env_uses_default_when_unset_or_invalid() {
        with_env_lock(|| {
            let previous = std::env::var_os("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT");
            unsafe { std::env::remove_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT") };
            assert_eq!(
                auto_compact_threshold_percent_from_env(),
                default_auto_compact_threshold_percent()
            );

            unsafe { std::env::set_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT", "0") };
            assert_eq!(
                auto_compact_threshold_percent_from_env(),
                default_auto_compact_threshold_percent()
            );

            unsafe { std::env::set_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT", "101") };
            assert_eq!(
                auto_compact_threshold_percent_from_env(),
                default_auto_compact_threshold_percent()
            );

            match previous {
                Some(value) => unsafe {
                    std::env::set_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT", value)
                },
                None => unsafe { std::env::remove_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT") },
            }
        });
    }

    #[test]
    fn auto_compact_threshold_percent_from_env_uses_valid_value() {
        with_env_lock(|| {
            let previous = std::env::var_os("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT");
            unsafe { std::env::set_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT", "75") };
            assert_eq!(auto_compact_threshold_percent_from_env(), 75);
            match previous {
                Some(value) => unsafe {
                    std::env::set_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT", value)
                },
                None => unsafe { std::env::remove_var("QUINE_AUTO_COMPACT_THRESHOLD_PERCENT") },
            }
        });
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

    #[test]
    fn config_parses_user_and_project_permission_rules() {
        with_env_lock(|| {
            let home = TempDir::new().unwrap();
            let project = TempDir::new().unwrap();
            let user_config_dir = home.path().join(".quine");
            let project_config_dir = project.path().join(".quine");
            std::fs::create_dir_all(&user_config_dir).unwrap();
            std::fs::create_dir_all(&project_config_dir).unwrap();
            std::fs::write(
                user_config_dir.join("permissions.yaml"),
                r#"
rules:
  - effect: allow
    scope: read
    target:
      kind: tool
      name: read_file
"#,
            )
            .unwrap();
            std::fs::write(
                project_config_dir.join("permissions.yaml"),
                r#"
rules:
  - effect: deny
    scope: execute
    target:
      kind: tool
      name: bash
  - effect: ask
    scope: write
    target:
      kind: path
      path: src
"#,
            )
            .unwrap();

            let previous_home = std::env::var_os("HOME");
            unsafe {
                std::env::set_var("HOME", home.path());
            }
            let rules = load_persisted_permission_rules(project.path()).unwrap();

            assert_eq!(rules.user.len(), 1);
            assert_eq!(rules.workspace.len(), 2);
            assert_eq!(rules.user[0].effect, PermissionRuleEffect::Allow);
            assert_eq!(
                rules.user[0].request_scope,
                Some(quine_core::PermissionScope::Read)
            );
            assert_eq!(
                rules.workspace[0].request_scope,
                Some(quine_core::PermissionScope::Execute)
            );
            assert_eq!(rules.workspace[1].effect, PermissionRuleEffect::Ask);
            assert!(rules.workspace[1]
                .source_path
                .as_ref()
                .is_some_and(|path| path.ends_with("permissions.yaml")));

            match previous_home {
                Some(value) => unsafe { std::env::set_var("HOME", value) },
                None => unsafe { std::env::remove_var("HOME") },
            }
        });
    }

    #[test]
    fn config_rejects_invalid_permission_rule_documents() {
        let project = TempDir::new().unwrap();
        let config_dir = project.path().join(".quine");
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(
            config_dir.join("permissions.yaml"),
            r#"
rules:
  - effect: allow
    scope: definitely_not_valid
    target:
      kind: tool
      name: bash
"#,
        )
        .unwrap();

        let error = load_persisted_permission_rules(project.path()).unwrap_err();
        assert!(error.to_string().contains("permissions.yaml"));
        assert!(error.to_string().contains("definitely_not_valid"));
    }

    #[test]
    fn web_config_defaults_to_none_when_no_key_is_available() {
        with_env_lock(|| {
            let snapshot = snapshot_env(&[
                "HOME",
                "WEB_PROVIDER",
                "LLM_PROVIDER",
                "WEB_API_KEY",
                "LLM_API_KEY",
                "OPENAI_API_KEY",
            ]);
            let home = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::remove_var("WEB_PROVIDER");
                std::env::remove_var("LLM_PROVIDER");
                std::env::remove_var("WEB_API_KEY");
                std::env::remove_var("LLM_API_KEY");
                std::env::remove_var("OPENAI_API_KEY");
            }

            assert!(matches!(web_config_from_env(), WebProviderConfig::None));

            restore_env(snapshot);
        });
    }

    #[test]
    fn web_config_uses_openai_when_key_is_available() {
        with_env_lock(|| {
            let snapshot = snapshot_env(&[
                "HOME",
                "WEB_PROVIDER",
                "LLM_PROVIDER",
                "WEB_API_KEY",
                "WEB_BASE_URL",
                "WEB_MODEL",
            ]);
            let home = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("WEB_PROVIDER", "openai");
                std::env::remove_var("LLM_PROVIDER");
                std::env::set_var("WEB_API_KEY", "test-key");
                std::env::set_var("WEB_BASE_URL", "https://example.test/v1");
                std::env::set_var("WEB_MODEL", "gpt-test");
            }

            match web_config_from_env() {
                WebProviderConfig::OpenAi(config) => {
                    assert_eq!(config.api_key, "test-key");
                    assert_eq!(config.base_url, "https://example.test/v1");
                    assert_eq!(config.model, "gpt-test");
                }
                WebProviderConfig::None => panic!("expected OpenAI web config"),
            }

            restore_env(snapshot);
        });
    }

    #[test]
    fn web_config_allows_explicit_loopback_endpoint_without_key() {
        with_env_lock(|| {
            let snapshot = snapshot_env(&[
                "HOME",
                "WEB_PROVIDER",
                "LLM_PROVIDER",
                "WEB_API_KEY",
                "LLM_API_KEY",
                "OPENAI_API_KEY",
                "WEB_BASE_URL",
                "LLM_BASE_URL",
                "OPENAI_BASE_URL",
                "WEB_MODEL",
                "LLM_MODEL",
            ]);
            let home = TempDir::new().unwrap();
            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::set_var("WEB_PROVIDER", "openai");
                std::env::remove_var("LLM_PROVIDER");
                std::env::remove_var("WEB_API_KEY");
                std::env::remove_var("LLM_API_KEY");
                std::env::remove_var("OPENAI_API_KEY");
                std::env::set_var("WEB_BASE_URL", "http://127.0.0.1:8000/v1");
                std::env::remove_var("LLM_BASE_URL");
                std::env::remove_var("OPENAI_BASE_URL");
                std::env::set_var("WEB_MODEL", "local-search");
                std::env::remove_var("LLM_MODEL");
            }

            match web_config_from_env() {
                WebProviderConfig::OpenAi(config) => {
                    assert_eq!(config.api_key, "");
                    assert_eq!(config.base_url, "http://127.0.0.1:8000/v1");
                    assert_eq!(config.model, "local-search");
                }
                WebProviderConfig::None => panic!("expected OpenAI web config"),
            }

            restore_env(snapshot);
        });
    }

    #[test]
    fn web_config_uses_user_config_file_for_loopback_without_key() {
        with_env_lock(|| {
            let snapshot = snapshot_env(&[
                "HOME",
                "WEB_PROVIDER",
                "LLM_PROVIDER",
                "WEB_API_KEY",
                "LLM_API_KEY",
                "OPENAI_API_KEY",
                "WEB_BASE_URL",
                "LLM_BASE_URL",
                "OPENAI_BASE_URL",
                "WEB_MODEL",
                "LLM_MODEL",
            ]);
            let home = TempDir::new().unwrap();
            let config_dir = home.path().join(".quine");
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(
                config_dir.join("config.yaml"),
                r#"
web_search:
  enabled: true
  provider: openai
  base_url: http://127.0.0.1:8000/v1
  model: local-search
"#,
            )
            .unwrap();

            unsafe {
                std::env::set_var("HOME", home.path());
                std::env::remove_var("WEB_PROVIDER");
                std::env::remove_var("LLM_PROVIDER");
                std::env::remove_var("WEB_API_KEY");
                std::env::remove_var("LLM_API_KEY");
                std::env::remove_var("OPENAI_API_KEY");
                std::env::remove_var("WEB_BASE_URL");
                std::env::remove_var("LLM_BASE_URL");
                std::env::remove_var("OPENAI_BASE_URL");
                std::env::remove_var("WEB_MODEL");
                std::env::remove_var("LLM_MODEL");
            }

            match web_config_from_env() {
                WebProviderConfig::OpenAi(config) => {
                    assert_eq!(config.api_key, "");
                    assert_eq!(config.base_url, "http://127.0.0.1:8000/v1");
                    assert_eq!(config.model, "local-search");
                }
                WebProviderConfig::None => panic!("expected OpenAI web config"),
            }

            restore_env(snapshot);
        });
    }
}
