use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use chrono::Utc;
use futures::StreamExt;
use quine_llm::{
    LlmEvent, LlmProvider, Message, MessageContent, NoopWebProvider, PromptCacheUsage,
    ToolDefinition, WebProvider,
};
use serde::Deserialize;
use tokio::sync::{mpsc, oneshot, Notify, RwLock};

use tokio::task::JoinSet;
use tokio::time::{Duration, Instant};

use crate::channel::{
    CoreHandle, CoreInput, CoreOutput, MailboxMessage, MessageSource, ToolOutcome, TurnStatus,
};
use crate::compaction::{self, CompactionTrigger, DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT};
use crate::error::CoreError;
use crate::filesystem::OverlayFilesystem;
use crate::memory::{
    build_prompt_memory_injection, default_turn_diagnostics, project_root_for_prompt_memory,
    refresh_summary_from_history, resolve_scoped_memory_paths, restore_memory_state,
    should_refresh_summary, snapshot_memory_state, snapshot_scoped_persistent_memory_state,
    splice_prompt_memory_messages, CompactionSourceDiagnostics, MemoryDecisionReason,
    MemoryDiagnostics, MemoryPolicyConfig, MemoryStatus, MemoryTurnDiagnostics,
    ScopedMemoryResolution, ScopedPersistentMemoryState, SessionMemoryState,
};
use crate::permission::{
    analyze_command, build_permission_approval_request, evaluate_permission,
    exit_plan_mode as exit_permission_plan_mode, parse_permission_approval_response,
    PendingPermissionApproval, PermissionApprovalChoice, PermissionContext, PermissionOutcome,
    PermissionPromptBehavior, PermissionRequest, PermissionResource, PermissionRuleSet,
    PermissionScope, ToolLocalDecision,
};
use crate::persistence::{
    CoreCheckpoint, PersistedPromptMemoryState, PersistedSession, PersistedSessionConfig,
    PersistedSessionState, PromptMemoryMode,
};
use crate::planner::scheduler::{get_ready_actions, render_plan};
use crate::python::{
    PythonExecTool, PythonInspectGlobalTool, PythonListGlobalsTool, PythonRuntime,
};
use crate::scheduler::spawn_scheduler;
use crate::session::{ExitStatus, SessionId, SessionSignal, SessionState};
use crate::session_tree::SessionTree;
use crate::skill::Skill;
use crate::status_report::{default_status_report_min_tool_rounds, SessionStatusReport};
use crate::tool::{
    ask_user::AskUserTool, bash::BashTool, find::FindTool, plan::PlanTool, read::ReadTool,
    recv_message::RecvMessageTool, send_message::SendMessageTool, signal::SignalTool,
    skill_template::SkillTemplateTool, spawn::SpawnTool, subagent::SubagentTool,
    wait_child::WaitChildTool, web_open::WebOpenTool, web_search::WebSearchTool, write::WriteTool,
    CancellationChannel, ExecutionContext, InteractionChannel, InteractionRequest,
    InteractionResponse, ToolError, ToolRegistry,
};

/// Default system prompt used when no CLAUDE.md and no explicit prompt is provided.
const DEFAULT_SYSTEM_PROMPT: &str = "\
You are a helpful coding assistant. You help users with software engineering tasks \
using the tools available to you. Each message from the user is a new request — \
respond to it directly. Use tools when needed to read files, run commands, or \
write code. Be concise and accurate.";

#[cfg_attr(not(test), allow(dead_code))]
const CONCURRENT_TOOL_BATCH_ALLOWLIST: &[&str] = &["find", "read_file"];

fn debug_enabled() -> bool {
    std::env::var("QUINE_DEBUG").is_ok()
}

fn debug_log(message: impl AsRef<str>) {
    if debug_enabled() {
        eprintln!("[core] {}", message.as_ref());
    }
}

fn debug_log_session(session_id: SessionId, message: impl AsRef<str>) {
    if debug_enabled() {
        eprintln!("[core][session={session_id:?}] {}", message.as_ref());
    }
}

async fn emit_failed_turn(
    output: &mpsc::Sender<CoreOutput>,
    session_id: SessionId,
    error: CoreError,
    duration_us: u64,
    usage: Option<quine_llm::TokenUsage>,
    cache_usage: Option<PromptCacheUsage>,
) {
    let _ = output
        .send(CoreOutput::SessionError { session_id, error })
        .await;
    let _ = output
        .send(CoreOutput::TurnComplete {
            session_id,
            duration_us,
            status: TurnStatus::Failed,
            usage,
            cache_usage,
        })
        .await;
}

fn schedule_session_memory_refresh(
    session: &mut SessionContext,
    session_id: SessionId,
    provider: Arc<dyn LlmProvider>,
    input_tx: mpsc::Sender<CoreInput>,
) {
    let refresh_outcome = if !session.session_memory.enabled {
        Some((
            false,
            MemoryStatus::Skipped,
            Some(MemoryDecisionReason::Disabled),
        ))
    } else if session.session_memory.refresh_in_flight {
        Some((
            false,
            MemoryStatus::Skipped,
            Some(MemoryDecisionReason::NotAttempted),
        ))
    } else if session.history.is_empty() {
        Some((
            false,
            MemoryStatus::Skipped,
            Some(MemoryDecisionReason::NoActivityYet),
        ))
    } else if !should_refresh_summary(&session.session_memory, &session.history) {
        Some((
            false,
            MemoryStatus::Skipped,
            Some(MemoryDecisionReason::RefreshNotNeeded),
        ))
    } else {
        None
    };

    if let Some((attempted, status, reason)) = refresh_outcome {
        let diagnostics = ensure_turn_diagnostics(session);
        diagnostics.session_memory.refresh.attempted = attempted;
        diagnostics.session_memory.refresh.status = status;
        diagnostics.session_memory.refresh.reason = reason;
        return;
    }

    let diagnostics = ensure_turn_diagnostics(session);
    diagnostics.session_memory.refresh.attempted = true;
    diagnostics.session_memory.refresh.status = MemoryStatus::NotRun;
    diagnostics.session_memory.refresh.reason = None;

    let history = session.history.clone();
    let memory_state = session.session_memory.clone();
    let refresh_handle = memory_state.refresh_handle.clone();
    session.session_memory.refresh_in_flight = true;
    tokio::spawn(async move {
        let _guard = refresh_handle.lock.lock().await;
        let refresh_result = tokio::task::spawn_blocking(move || {
            refresh_summary_from_history(&memory_state, &history)
        })
        .await;

        let (last_summarized_message_index, refreshed_at, listing_summary) = match refresh_result {
            Ok(Ok(Some(update))) => {
                let listing_summary = generate_session_listing_summary(
                    provider.as_ref(),
                    session_id,
                    &update.document.render_markdown(),
                )
                .await
                .ok()
                .flatten();
                (
                    Some(update.metadata.last_summarized_message_index),
                    Some(update.metadata.updated_at),
                    listing_summary,
                )
            }
            _ => (None, None, None),
        };

        let _ = input_tx
            .send(CoreInput::SessionMemoryRefreshFinished {
                session_id,
                last_summarized_message_index,
                refreshed_at,
                listing_summary,
            })
            .await;
        debug_log_session(session_id, "session memory refresh finished");
    });
}

fn default_turn_diagnostics_for_session(session: &SessionContext) -> MemoryTurnDiagnostics {
    default_turn_diagnostics(
        &session.session_memory.paths.summary_path,
        &session.session_memory.paths.metadata_path,
        session.persisted_config.prompt_memory_mode,
        project_root_for_prompt_memory(&session.persisted_config.working_directory),
        session.session_memory.persistent_enabled,
        Some(&session.scoped_persistent_memory_state),
    )
}

fn ensure_turn_diagnostics(session: &mut SessionContext) -> &mut MemoryTurnDiagnostics {
    let default = default_turn_diagnostics_for_session(session);
    session.last_memory_diagnostics.get_or_insert(default)
}

fn refresh_scoped_memory_state(session: &mut SessionContext) {
    session.scoped_memory_resolution = resolve_scoped_memory_paths(
        &session.archive_root.join("memory"),
        &session.persisted_config.memory_policy,
        &session.persisted_config.working_directory,
        session.persisted_config.agent_key.as_deref(),
        session.persisted_config.team_key.as_deref(),
    );
    session.scoped_persistent_memory_state =
        snapshot_scoped_persistent_memory_state(&session.scoped_memory_resolution);
}

/// System prompt prepended in plan mode to restrict the agent to planning-only work.
const PLAN_MODE_SYSTEM_PROMPT: &str = "\
You are a software architect and planning specialist. Your role is to explore the \
codebase and create detailed implementation plans. You are in planning-only mode.

CRITICAL CONSTRAINTS:
- You MUST NOT create, edit, delete, or modify any files
- You MUST NOT run commands that alter system state (no writes, no installs, no git commits)
- You may use tools to inspect the codebase and gather context, but only in ways that preserve the current system state

PROCESS:
1. Understand the user's requirements
2. Explore the codebase thoroughly using available tools that do not change system state
3. Analyze existing patterns, architecture, and conventions
4. Design a solution that fits the existing codebase
5. Produce a detailed step-by-step implementation plan

YOUR PLAN MUST INCLUDE:
- Overview of the approach
- Specific files to create or modify (with paths)
- Code sketches or type signatures where helpful
- Dependencies between steps
- Critical files for implementation (3-5 key files with justifications)

Remember: You can ONLY explore and plan. You CANNOT modify files or run state-changing commands.";

/// Walk up from `start` looking for CLAUDE.md, returning the first one found.
fn find_claude_md(start: &std::path::Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("CLAUDE.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Per-session context held by the core event loop.
struct SessionContext {
    state: SessionState,
    /// Active LLM provider for this session.
    provider: Arc<dyn LlmProvider>,
    #[allow(dead_code)]
    system_prompt: Option<String>,
    /// Conversation history for this session.
    history: Vec<Message>,
    /// Tool definitions available to the LLM.
    tools: Vec<ToolDefinition>,
    /// Registry of tool implementations.
    tool_registry: ToolRegistry,
    /// Session filesystem.
    filesystem: Arc<dyn crate::filesystem::SessionFilesystem>,
    /// Working directory for this session.
    working_directory: PathBuf,
    /// Effective python session-group key for this session.
    python_group: String,
    /// Shared runtime for session-group python execution.
    python_runtime: Arc<PythonRuntime>,
    /// Sender for pending interaction responses (tool_use_id -> sender).
    pending_interaction: Option<oneshot::Sender<InteractionResponse>>,
    /// Shared plan store for this session.
    plan_store: crate::tool::plan::PlanStore,
    /// Per-turn cancellation sender for the currently running tool or prompt.
    cancel_tx: Option<tokio::sync::watch::Sender<bool>>,
    /// Latches an interrupt until the next user message resumes the session.
    interrupted: bool,
    /// Last known prompt token count reported by the provider.
    last_input_tokens: Option<u64>,
    /// Archive root for compacted transcripts.
    archive_root: PathBuf,
    /// Max context window for the configured model, if known.
    max_context_window: Option<u64>,
    /// Archive generation counter for this session.
    compaction_generation: u64,
    /// Auto-compaction threshold as a percentage of the model context window.
    auto_compact_threshold_percent: u8,
    /// Serializable creation-time configuration needed for restore.
    persisted_config: PersistedSessionConfig,
    /// Stable creation timestamp used for checkpoint listings.
    created_at: chrono::DateTime<Utc>,
    /// Delivered mailbox messages that have not yet been consumed.
    mailbox: VecDeque<MailboxMessage>,
    /// Suspended wait, if this session is waiting on an event-driven dependency.
    suspended_wait: Option<SuspendedWait>,
    /// Session memory bookkeeping and paths.
    session_memory: SessionMemoryState,
    #[allow(dead_code)]
    /// Session memory diagnostics.
    memory_diagnostics: MemoryDiagnostics,
    last_memory_diagnostics: Option<MemoryTurnDiagnostics>,
    /// Last prompt-time memory injection summary.
    last_prompt_memory: PersistedPromptMemoryState,
    /// Latest user-message index used for prompt-memory de-duplication.
    last_prompt_memory_user_index: Option<usize>,
    /// Resolved durable-memory scope state for this session.
    scoped_memory_resolution: ScopedMemoryResolution,
    scoped_persistent_memory_state: ScopedPersistentMemoryState,
    /// Internal permission bootstrap state for this session.
    permission_context: PermissionContext,
    /// Latest evaluator result retained for future diagnostics work.
    last_permission_outcome: Option<PermissionOutcome>,
    /// Pending permission approval request, if any.
    pending_permission_approval: Option<PendingPermissionApproval>,
    /// Canonical prompt tokens for the most recent provider request.
    last_prompt_cache_tokens: Option<Vec<String>>,
    /// Latest status report for this session's most recent tool loop.
    status_report: Option<SessionStatusReport>,
    /// Number of assistant-emitted tool rounds observed in the current user turn.
    current_turn_tool_rounds: u32,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone)]
enum SuspendedWait {
    Mailbox {
        tool_use_id: String,
        source: MessageSource,
        timeout_at: Option<Instant>,
    },
    ChildExit {
        tool_use_id: String,
        child_id: SessionId,
        timeout_at: Option<Instant>,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
impl SuspendedWait {
    fn depends_on(&self) -> Option<SessionId> {
        match self {
            Self::Mailbox {
                source: MessageSource::Session(session_id),
                ..
            } => Some(*session_id),
            Self::Mailbox {
                source: MessageSource::Any,
                ..
            } => None,
            Self::ChildExit { child_id, .. } => Some(*child_id),
        }
    }

    fn timeout_at(&self) -> Option<Instant> {
        match self {
            Self::Mailbox { timeout_at, .. } | Self::ChildExit { timeout_at, .. } => *timeout_at,
        }
    }

    fn timeout_message(&self) -> &'static str {
        match self {
            Self::Mailbox { .. } => "recv_message timed out",
            Self::ChildExit { .. } => "wait_child timed out",
        }
    }

    fn tool_use_id(&self) -> &str {
        match self {
            Self::Mailbox { tool_use_id, .. } | Self::ChildExit { tool_use_id, .. } => tool_use_id,
        }
    }
}

struct SessionInit {
    system_prompt: Option<String>,
    skills: Vec<Skill>,
    working_directory: PathBuf,
    plan_mode: bool,
    prompt_behavior: PermissionPromptBehavior,
    initial_messages: Vec<Message>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
    prompt_memory_mode: PromptMemoryMode,
    agent_key: Option<String>,
    team_key: Option<String>,
    memory_policy: MemoryPolicyConfig,
    model_profile: Option<String>,
    session_group: Option<String>,
    auto_compact_threshold_percent: u8,
    status_report_min_tool_rounds: u32,
}

impl SessionInit {
    fn to_persisted_config(&self) -> PersistedSessionConfig {
        PersistedSessionConfig {
            system_prompt: self.system_prompt.clone(),
            skill_names: self
                .skills
                .iter()
                .map(|skill| skill.meta.name.clone())
                .collect(),
            working_directory: self.working_directory.clone(),
            plan_mode: self.plan_mode,
            prompt_behavior: self.prompt_behavior,
            prompt_memory_mode: self.prompt_memory_mode,
            agent_key: self.agent_key.clone(),
            team_key: self.team_key.clone(),
            memory_policy: self.memory_policy.clone(),
            model_profile: self.model_profile.clone(),
            session_group: self.session_group.clone(),
            auto_compact_threshold_percent: self.auto_compact_threshold_percent.clamp(1, 100),
            status_report_min_tool_rounds: self.status_report_min_tool_rounds.max(1),
        }
    }
}

fn sanitize_restored_history(session_id: SessionId, history: Vec<Message>) -> Vec<Message> {
    let mut tool_use_positions = HashMap::new();
    let mut tool_result_positions = HashMap::new();

    for (index, message) in history.iter().enumerate() {
        match &message.content {
            MessageContent::ToolUse { tool_calls, .. } => {
                for call in tool_calls {
                    tool_use_positions
                        .entry(call.tool_use_id.clone())
                        .or_insert(index);
                }
            }
            MessageContent::ToolResult { tool_use_id, .. } => {
                tool_result_positions.insert(tool_use_id.clone(), index);
            }
            MessageContent::Text(_) => {}
        }
    }

    let valid_tool_use_ids = tool_use_positions
        .iter()
        .filter_map(|(tool_use_id, tool_use_index)| {
            tool_result_positions
                .get(tool_use_id)
                .filter(|tool_result_index| **tool_result_index > *tool_use_index)
                .map(|_| tool_use_id.clone())
        })
        .collect::<HashSet<_>>();

    let mut removed_tool_calls = 0usize;
    let mut removed_tool_results = 0usize;
    let mut sanitized = Vec::with_capacity(history.len());

    for message in history {
        let role = message.role.clone();
        match message.content {
            MessageContent::ToolUse { text, tool_calls } => {
                let original_count = tool_calls.len();
                let retained_tool_calls = tool_calls
                    .into_iter()
                    .filter(|call| valid_tool_use_ids.contains(&call.tool_use_id))
                    .collect::<Vec<_>>();
                removed_tool_calls += original_count.saturating_sub(retained_tool_calls.len());

                if retained_tool_calls.is_empty() {
                    if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
                        sanitized.push(Message {
                            role,
                            content: MessageContent::Text(text),
                        });
                    }
                } else {
                    sanitized.push(Message {
                        role,
                        content: MessageContent::ToolUse {
                            text,
                            tool_calls: retained_tool_calls,
                        },
                    });
                }
            }
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                if valid_tool_use_ids.contains(&tool_use_id) {
                    sanitized.push(Message {
                        role,
                        content: MessageContent::ToolResult {
                            tool_use_id,
                            output,
                            is_error,
                        },
                    });
                } else {
                    removed_tool_results += 1;
                }
            }
            MessageContent::Text(text) => sanitized.push(Message {
                role,
                content: MessageContent::Text(text),
            }),
        }
    }

    if removed_tool_calls > 0 || removed_tool_results > 0 {
        debug_log_session(
            session_id,
            format!(
                "sanitized restored history: removed {removed_tool_calls} zombie tool calls and {removed_tool_results} orphan tool results"
            ),
        );
    }

    sanitized
}

fn effective_session_group(session_id: SessionId, session_group: Option<&str>) -> String {
    session_group
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| session_id.to_string())
}

fn build_tool_registry_for_session(
    provider: &Arc<dyn LlmProvider>,
    web_provider: &Arc<dyn WebProvider>,
    plan_store: crate::tool::plan::PlanStore,
    persisted_config: &PersistedSessionConfig,
    skills: &[Skill],
) -> ToolRegistry {
    let mut tool_registry = ToolRegistry::new();
    tool_registry.register(Arc::new(ReadTool));
    tool_registry.register(Arc::new(BashTool));
    tool_registry.register(Arc::new(FindTool));
    tool_registry.register(Arc::new(AskUserTool));
    tool_registry.register(Arc::new(PlanTool::new(plan_store)));
    tool_registry.register(Arc::new(WebSearchTool::new(Arc::clone(web_provider))));
    tool_registry.register(Arc::new(WebOpenTool::new(Arc::clone(web_provider))));
    tool_registry.register(Arc::new(PythonExecTool));
    tool_registry.register(Arc::new(PythonListGlobalsTool));
    tool_registry.register(Arc::new(PythonInspectGlobalTool));

    if !persisted_config.plan_mode {
        tool_registry.register(Arc::new(WriteTool));
        tool_registry.register(Arc::new(SubagentTool::new(
            Arc::clone(provider),
            Arc::clone(web_provider),
        )));
        tool_registry.register(Arc::new(SpawnTool));
        tool_registry.register(Arc::new(WaitChildTool));
        tool_registry.register(Arc::new(SignalTool));
        tool_registry.register(Arc::new(SendMessageTool));
        tool_registry.register(Arc::new(RecvMessageTool));
    }

    for skill in skills {
        for tool_def in &skill.tool_definitions {
            tool_registry.register(Arc::new(SkillTemplateTool::new(tool_def.clone())));
        }
    }

    tool_registry
}

fn advertised_tool_definitions(
    tool_registry: &ToolRegistry,
    web_provider: &Arc<dyn WebProvider>,
) -> Vec<ToolDefinition> {
    let mut tools = tool_registry.tool_definitions();
    if !web_provider.is_configured() {
        tools.retain(|tool| !matches!(tool.name.as_str(), "web_search" | "web_open"));
    }
    tools
}

fn prompt_memory_mode_from_env() -> PromptMemoryMode {
    match std::env::var("QUINE_PROMPT_MEMORY_MODE")
        .unwrap_or_else(|_| "disabled".into())
        .to_ascii_lowercase()
        .as_str()
    {
        "index_only" | "index" => PromptMemoryMode::IndexOnly,
        "targeted_recall" | "targeted" | "recall" => PromptMemoryMode::TargetedRecall,
        _ => PromptMemoryMode::Disabled,
    }
}

fn build_combined_system_prompt(
    working_directory: &std::path::Path,
    persisted_config: &PersistedSessionConfig,
    skills: &[Skill],
    tools: &[ToolDefinition],
    prompt_memory_suffix: Option<&str>,
) -> Option<String> {
    let mut prompt_parts = Vec::new();

    if persisted_config.plan_mode {
        prompt_parts.push(PLAN_MODE_SYSTEM_PROMPT.to_string());
    }

    prompt_parts.push(DEFAULT_SYSTEM_PROMPT.to_string());
    prompt_parts.push(render_available_tool_descriptions(tools));

    if let Some(claude_md_path) = find_claude_md(working_directory) {
        if let Ok(content) = std::fs::read_to_string(&claude_md_path) {
            prompt_parts.push(format!(
                "# Project Instructions (from CLAUDE.md)\n\n{content}"
            ));
        }
    }

    if let Some(base) = &persisted_config.system_prompt {
        prompt_parts.push(base.clone());
    }
    for skill in skills {
        if let Some(sp) = &skill.system_prompt {
            prompt_parts.push(format!("\n## Skill: {}\n{}", skill.meta.name, sp));
        }
    }

    if let Some(suffix) = prompt_memory_suffix {
        prompt_parts.push(suffix.to_string());
    }

    Some(prompt_parts.join("\n\n"))
}

fn render_available_tool_descriptions(tools: &[ToolDefinition]) -> String {
    if tools.is_empty() {
        return "## Available Tools\n\nNo tools are currently available.".to_string();
    }

    let mut lines = Vec::with_capacity(tools.len() + 2);
    lines.push("## Available Tools".to_string());
    lines.push(String::new());
    for tool in tools {
        let mut qualifiers = Vec::new();
        if tool.read_only {
            qualifiers.push("read-only");
        }
        if tool.idempotent {
            qualifiers.push("idempotent");
        }
        let qualifier_suffix = if qualifiers.is_empty() {
            String::new()
        } else {
            format!(" ({})", qualifiers.join(", "))
        };
        lines.push(format!(
            "- `{}`{}: {}",
            tool.name, qualifier_suffix, tool.description
        ));
    }
    lines.join("\n")
}

fn canonicalize_prompt_cache_tokens(messages: &[Message], tools: &[ToolDefinition]) -> Vec<String> {
    fn push_text_tokens(tokens: &mut Vec<String>, text: &str) {
        tokens.extend(
            text.split_whitespace()
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .map(|token| token.to_ascii_lowercase()),
        );
    }

    let mut tokens = Vec::new();
    for message in messages {
        let role = match message.role {
            quine_llm::Role::System => "system",
            quine_llm::Role::User => "user",
            quine_llm::Role::Assistant => "assistant",
            quine_llm::Role::Tool => "tool",
        };
        tokens.push(format!("role:{role}"));
        match &message.content {
            MessageContent::Text(text) => push_text_tokens(&mut tokens, text),
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                tokens.push(format!("tool_use_id:{tool_use_id}"));
                tokens.push(format!("tool_error:{is_error}"));
                push_text_tokens(&mut tokens, output);
            }
            MessageContent::ToolUse { text, tool_calls } => {
                if let Some(text) = text {
                    push_text_tokens(&mut tokens, text);
                }
                for call in tool_calls {
                    tokens.push(format!("tool_call:{}", call.tool_name));
                    tokens.push(format!("tool_use_id:{}", call.tool_use_id));
                    push_text_tokens(&mut tokens, &call.arguments.to_string());
                }
            }
        }
    }

    let mut sorted_tools = tools.to_vec();
    sorted_tools.sort_by(|a, b| a.name.cmp(&b.name));
    for tool in sorted_tools {
        tokens.push(format!("tool:{}", tool.name));
        tokens.push(format!("tool_ro:{}", tool.read_only));
        tokens.push(format!("tool_idempotent:{}", tool.idempotent));
        push_text_tokens(&mut tokens, &tool.description);
        push_text_tokens(&mut tokens, &tool.parameters.to_string());
    }

    tokens
}

fn estimate_prompt_cache_usage(
    previous_tokens: Option<&[String]>,
    current_tokens: &[String],
) -> PromptCacheUsage {
    let estimated_hit_tokens = previous_tokens
        .map(|previous| {
            previous
                .iter()
                .zip(current_tokens.iter())
                .take_while(|(left, right)| left == right)
                .count() as u64
        })
        .unwrap_or(0);

    PromptCacheUsage {
        estimated_hit_tokens,
        estimated_miss_tokens: current_tokens.len() as u64 - estimated_hit_tokens,
    }
}

impl SessionContext {
    async fn new(
        session_id: SessionId,
        init: SessionInit,
        provider: &Arc<dyn LlmProvider>,
    ) -> Result<Self, CoreError> {
        let web_provider: Arc<dyn WebProvider> = Arc::new(NoopWebProvider);
        Self::new_with_web_provider(
            session_id,
            init,
            provider,
            &web_provider,
            PythonRuntime::new(),
        )
        .await
    }

    async fn new_with_web_provider(
        session_id: SessionId,
        init: SessionInit,
        provider: &Arc<dyn LlmProvider>,
        web_provider: &Arc<dyn WebProvider>,
        python_runtime: Arc<PythonRuntime>,
    ) -> Result<Self, CoreError> {
        let persisted_config = init.to_persisted_config();
        let python_group =
            effective_session_group(session_id, persisted_config.session_group.as_deref());
        let SessionInit {
            system_prompt: _,
            skills,
            working_directory,
            plan_mode: _,
            prompt_behavior,
            initial_messages,
            archive_root,
            max_context_window,
            agent_key,
            team_key,
            memory_policy,
            ..
        } = init;
        let filesystem = Arc::new(
            OverlayFilesystem::new(working_directory.clone(), working_directory.clone())
                .await
                .map_err(|e| CoreError::Internal {
                    message: format!("failed to create session filesystem: {e}"),
                })?,
        );

        let plan_store = crate::tool::plan::new_plan_store();
        let session_memory = restore_memory_state(&archive_root, session_id, None);
        let persistent_enabled = session_memory.persistent_enabled;
        let summary_path = session_memory.paths.summary_path.clone();
        let metadata_path = session_memory.paths.metadata_path.clone();
        let scoped_memory_resolution = resolve_scoped_memory_paths(
            &archive_root.join("memory"),
            &memory_policy,
            &working_directory,
            agent_key.as_deref(),
            team_key.as_deref(),
        );
        let scoped_persistent_memory_state =
            snapshot_scoped_persistent_memory_state(&scoped_memory_resolution);

        let tool_registry = build_tool_registry_for_session(
            provider,
            web_provider,
            plan_store.clone(),
            &persisted_config,
            &skills,
        );
        let permission_context = PermissionContext::new(
            working_directory.clone(),
            persisted_config.plan_mode,
            prompt_behavior,
        );
        let tools = advertised_tool_definitions(&tool_registry, web_provider);

        let combined_prompt = build_combined_system_prompt(
            &working_directory,
            &persisted_config,
            &skills,
            &tools,
            None,
        );

        let mut history = Vec::new();
        if let Some(prompt) = &combined_prompt {
            history.push(Message::system(prompt.clone()));
        }
        history.extend(initial_messages);

        Ok(Self {
            state: SessionState::Idle,
            provider: Arc::clone(provider),
            system_prompt: combined_prompt,
            history,
            tools,
            tool_registry,
            filesystem,
            working_directory,
            python_group,
            python_runtime,
            pending_interaction: None,
            plan_store,
            cancel_tx: None,
            interrupted: false,
            last_input_tokens: None,
            archive_root,
            max_context_window,
            compaction_generation: 0,
            auto_compact_threshold_percent: persisted_config.auto_compact_threshold_percent,
            persisted_config: persisted_config.clone(),
            created_at: Utc::now(),
            mailbox: VecDeque::new(),
            suspended_wait: None,
            session_memory,
            memory_diagnostics: MemoryDiagnostics::default(),
            last_memory_diagnostics: Some(default_turn_diagnostics(
                &summary_path,
                &metadata_path,
                persisted_config.prompt_memory_mode,
                project_root_for_prompt_memory(&persisted_config.working_directory),
                persistent_enabled,
                Some(&scoped_persistent_memory_state),
            )),
            last_prompt_memory: PersistedPromptMemoryState {
                mode: persisted_config.prompt_memory_mode,
                ..PersistedPromptMemoryState::default()
            },
            last_prompt_memory_user_index: None,
            scoped_memory_resolution,
            scoped_persistent_memory_state,
            permission_context,
            last_permission_outcome: None,
            pending_permission_approval: None,
            last_prompt_cache_tokens: None,
            status_report: None,
            current_turn_tool_rounds: 0,
        })
    }

    async fn from_persisted_with_web_provider(
        persisted: PersistedSession,
        provider: &Arc<dyn LlmProvider>,
        web_provider: &Arc<dyn WebProvider>,
        archive_root: PathBuf,
        max_context_window: Option<u64>,
        python_runtime: Arc<PythonRuntime>,
    ) -> Result<(SessionId, Self), CoreError> {
        let PersistedSession {
            session_id,
            created_at,
            state,
            config,
            history,
            plan_store,
            memory_state,
            permission_state,
            status_report,
            python_state,
        } = persisted;
        let skills =
            crate::skill::load_session_skills(&config.working_directory, &config.skill_names).await;
        let restored_skill_names = skills
            .iter()
            .map(|skill| skill.meta.name.clone())
            .collect::<Vec<_>>();
        let mut session = Self::new_with_web_provider(
            session_id,
            SessionInit {
                system_prompt: config.system_prompt.clone(),
                skills: skills.clone(),
                working_directory: config.working_directory.clone(),
                plan_mode: config.plan_mode,
                prompt_behavior: config.prompt_behavior,
                initial_messages: Vec::new(),
                archive_root,
                max_context_window,
                prompt_memory_mode: config.prompt_memory_mode,
                agent_key: config.agent_key.clone(),
                team_key: config.team_key.clone(),
                memory_policy: config.memory_policy.clone(),
                model_profile: config.model_profile.clone(),
                session_group: config.session_group.clone(),
                auto_compact_threshold_percent: config.auto_compact_threshold_percent,
                status_report_min_tool_rounds: config.status_report_min_tool_rounds,
            },
            provider,
            web_provider,
            Arc::clone(&python_runtime),
        )
        .await?;
        if let Some(python_state) = python_state.as_ref() {
            python_runtime
                .restore_group(&session.python_group, python_state)
                .await
                .map_err(|error| CoreError::Internal {
                    message: format!("failed to restore python group: {error}"),
                })?;
        }
        session.state = state.into();
        session.history = sanitize_restored_history(session_id, history);
        session.auto_compact_threshold_percent = config.auto_compact_threshold_percent;
        session.plan_store = crate::tool::plan::restore_plan_store(plan_store).await;
        session.tool_registry = build_tool_registry_for_session(
            provider,
            web_provider,
            session.plan_store.clone(),
            &session.persisted_config,
            &skills,
        );
        session.tools = advertised_tool_definitions(&session.tool_registry, web_provider);
        session.persisted_config = config;
        session.persisted_config.skill_names = restored_skill_names;
        session.created_at = created_at;
        session.session_memory =
            restore_memory_state(&session.archive_root, session_id, memory_state.as_ref());
        session.last_prompt_memory = memory_state
            .as_ref()
            .and_then(|state| state.prompt_memory.clone())
            .unwrap_or(PersistedPromptMemoryState {
                mode: session.persisted_config.prompt_memory_mode,
                ..PersistedPromptMemoryState::default()
            });
        session.last_memory_diagnostics = memory_state
            .as_ref()
            .and_then(|state| state.memory_diagnostics.clone());
        if let Some(scope_state) = memory_state
            .as_ref()
            .and_then(|state| state.persistent_memory.as_ref())
            .and_then(|state| state.scope_state.clone())
        {
            session.scoped_persistent_memory_state = scope_state;
        } else {
            refresh_scoped_memory_state(&mut session);
        }
        if let Some(permission_state) = permission_state.as_ref() {
            session.permission_context = PermissionContext::from_snapshot(permission_state);
            session.last_permission_outcome = permission_state.last_decision.clone();
            session.pending_permission_approval = permission_state.pending_approval.clone();
        }
        session.status_report = status_report;
        Ok((session_id, session))
    }

    async fn snapshot(&self, session_id: SessionId) -> Option<PersistedSession> {
        let state = PersistedSessionState::from_runtime(self.state)?;
        let python_state = self
            .python_runtime
            .snapshot_group(&self.python_group)
            .await
            .ok();
        Some(PersistedSession {
            session_id,
            created_at: self.created_at,
            state,
            config: self.persisted_config.clone(),
            history: self.history.clone(),
            plan_store: crate::tool::plan::snapshot_plan_store(&self.plan_store).await,
            memory_state: Some({
                let mut memory_state = snapshot_memory_state(&self.session_memory);
                memory_state.prompt_memory = Some(self.last_prompt_memory.clone());
                memory_state.memory_diagnostics = self.last_memory_diagnostics.clone();
                if let Some(persistent) = memory_state.persistent_memory.as_mut() {
                    persistent.scope_state = Some(self.scoped_persistent_memory_state.clone());
                }
                memory_state
            }),
            permission_state: Some(self.permission_context.snapshot(
                self.last_permission_outcome.clone(),
                self.pending_permission_approval.clone(),
            )),
            status_report: self.status_report.clone(),
            python_state,
        })
    }

    async fn rebuild_session_config_with_web_provider(
        &mut self,
        web_provider: &Arc<dyn WebProvider>,
    ) -> Result<(), CoreError> {
        let skills = crate::skill::load_skills(
            &self.persisted_config.working_directory,
            &self.persisted_config.skill_names,
        )
        .await;
        self.tool_registry = build_tool_registry_for_session(
            &self.provider,
            web_provider,
            self.plan_store.clone(),
            &self.persisted_config,
            &skills,
        );
        self.tools = advertised_tool_definitions(&self.tool_registry, web_provider);
        self.system_prompt = build_combined_system_prompt(
            &self.persisted_config.working_directory,
            &self.persisted_config,
            &skills,
            &self.tools,
            None,
        );
        refresh_scoped_memory_state(self);

        match &self.system_prompt {
            Some(prompt) => {
                if matches!(
                    self.history.first(),
                    Some(Message {
                        role: quine_llm::Role::System,
                        ..
                    })
                ) {
                    self.history[0] = Message::system(prompt.clone());
                } else {
                    self.history.insert(0, Message::system(prompt.clone()));
                }
            }
            None => {
                if matches!(
                    self.history.first(),
                    Some(Message {
                        role: quine_llm::Role::System,
                        ..
                    })
                ) {
                    self.history.remove(0);
                }
            }
        }

        Ok(())
    }

    #[cfg(test)]
    async fn exit_plan_mode(&mut self, provider: &Arc<dyn LlmProvider>) -> Result<(), CoreError> {
        self.provider = Arc::clone(provider);
        let web_provider: Arc<dyn WebProvider> = Arc::new(NoopWebProvider);
        self.exit_plan_mode_with_web_provider(&web_provider).await
    }

    async fn exit_plan_mode_with_web_provider(
        &mut self,
        web_provider: &Arc<dyn WebProvider>,
    ) -> Result<(), CoreError> {
        if !self.persisted_config.plan_mode {
            return Ok(());
        }
        self.persisted_config.plan_mode = false;
        let _ = exit_permission_plan_mode(&mut self.permission_context);
        self.rebuild_session_config_with_web_provider(web_provider)
            .await
    }

    async fn update_llm_provider_with_web_provider(
        &mut self,
        provider: Arc<dyn LlmProvider>,
        model_profile: Option<String>,
        max_context_window: Option<u64>,
        web_provider: &Arc<dyn WebProvider>,
    ) -> Result<(), CoreError> {
        self.provider = provider;
        self.persisted_config.model_profile = model_profile;
        self.auto_compact_threshold_percent = self.persisted_config.auto_compact_threshold_percent;
        self.max_context_window = max_context_window;
        self.rebuild_session_config_with_web_provider(web_provider)
            .await
    }
}

#[derive(Clone)]
struct SessionHandle {
    command_tx: mpsc::Sender<SessionCommand>,
    provider: Arc<dyn LlmProvider>,
    max_context_window: Option<u64>,
    model_profile: Option<String>,
    session_group: Option<String>,
}

enum SessionCommand {
    UserMessage {
        content: String,
        turn_id: String,
    },
    ExitPlanMode {
        reply: oneshot::Sender<Result<(), String>>,
    },
    UpdateSessionLlm {
        session_llm: crate::channel::SessionLlmConfig,
        reply: oneshot::Sender<Result<(), String>>,
    },
    CompactSession {
        reply: oneshot::Sender<Result<(), String>>,
    },
    ToolResult {
        tool_use_id: String,
        result: ToolOutcome,
    },
    InteractionResponse(InteractionResponse),
    Cancel,
    Signal(SessionSignal),
    MailboxMessage(MailboxMessage),
    Snapshot {
        reply: oneshot::Sender<Option<PersistedSession>>,
    },
    SessionMemoryRefreshFinished {
        last_summarized_message_index: Option<usize>,
        refreshed_at: Option<chrono::DateTime<chrono::Utc>>,
        listing_summary: Option<String>,
    },
    QueryMailbox {
        source: MessageSource,
        reply: oneshot::Sender<Option<MailboxMessage>>,
    },
    ChildExited {
        child_id: SessionId,
        status: ExitStatus,
    },
    Shutdown,
}

enum RuntimeEvent {
    ChildSessionFinished {
        session_id: SessionId,
        parent_id: SessionId,
        status: ExitStatus,
    },
    CheckpointHint,
}

type SessionRegistry = Arc<RwLock<HashMap<SessionId, SessionHandle>>>;
type SharedSessionTree = Arc<RwLock<SessionTree>>;

fn mailbox_matches_source(message: &MailboxMessage, source: &MessageSource) -> bool {
    match source {
        MessageSource::Any => true,
        MessageSource::Session(session_id) => message.from == *session_id,
    }
}

fn pop_mailbox_message(
    mailbox: &mut VecDeque<MailboxMessage>,
    source: &MessageSource,
) -> Option<MailboxMessage> {
    let index = mailbox
        .iter()
        .position(|message| mailbox_matches_source(message, source))?;
    mailbox.remove(index)
}

#[cfg_attr(not(test), allow(dead_code))]
fn waiting_on_session(session: &SessionContext) -> Option<SessionId> {
    session
        .suspended_wait
        .as_ref()
        .and_then(SuspendedWait::depends_on)
}

#[cfg_attr(not(test), allow(dead_code))]
fn waiting_would_cycle(
    sessions: &HashMap<SessionId, SessionContext>,
    waiter: SessionId,
    dependency: SessionId,
) -> bool {
    let mut current = dependency;
    let mut visited = HashSet::new();
    while visited.insert(current) {
        if current == waiter {
            return true;
        }
        let Some(next) = sessions.get(&current).and_then(waiting_on_session) else {
            return false;
        };
        current = next;
    }
    false
}

#[cfg_attr(not(test), allow(dead_code))]
fn waiting_would_cycle_including_session_tree(
    sessions: &HashMap<SessionId, SessionContext>,
    session_tree: &SessionTree,
    waiter: SessionId,
    dependency: SessionId,
) -> bool {
    waiting_would_cycle(sessions, waiter, dependency)
        || session_tree.wait_would_cycle(waiter, dependency)
}

#[cfg_attr(not(test), allow(dead_code))]
async fn emit_session_waiting(
    sessions: &HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    session_tree: &SessionTree,
) {
    let _ = output
        .send(CoreOutput::SessionStateChanged {
            session_id,
            state: SessionState::Waiting,
        })
        .await;
    emit_checkpoint_request(sessions, session_tree, output).await;
}

#[cfg_attr(not(test), allow(dead_code))]
async fn resume_session_from_wait(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    tool_result: ToolOutcome,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) {
    let Some(tool_use_id) = sessions.get(&session_id).and_then(|session| {
        session
            .suspended_wait
            .as_ref()
            .map(|wait| wait.tool_use_id().to_string())
    }) else {
        return;
    };

    let (output_text, is_error) = match &tool_result {
        ToolOutcome::Success { output } => (output.clone(), false),
        ToolOutcome::Error { message } => (message.clone(), true),
        ToolOutcome::Cancelled => ("Tool execution was cancelled".to_string(), true),
    };

    let history_output = {
        let Some(session) = sessions.get(&session_id) else {
            return;
        };
        match prepare_tool_result_for_history(
            session,
            session_id,
            &tool_use_id,
            "suspended_wait",
            &output_text,
            is_error,
        )
        .await
        {
            Ok(output) => output,
            Err(error) => {
                let _ = io
                    .output
                    .send(CoreOutput::SessionError { session_id, error })
                    .await;
                return;
            }
        }
    };

    if let Some(session) = sessions.get_mut(&session_id) {
        session.suspended_wait = None;
        engine.session_tree.clear_active_wait(session_id);
        session.history.push(Message::tool_result(
            &tool_use_id,
            &history_output,
            is_error,
        ));
        session.state = SessionState::Streaming;
    }

    let _ = io
        .output
        .send(CoreOutput::ToolResult {
            session_id,
            tool_use_id: tool_use_id.clone(),
            tool_name: "suspended_wait".into(),
            content: output_text,
            is_error,
            duration_us: 0,
        })
        .await;
    let _ = io
        .output
        .send(CoreOutput::SessionStateChanged {
            session_id,
            state: SessionState::Streaming,
        })
        .await;

    let turn_outcome = handle_llm_turn(sessions, session_id, io, engine).await;
    if engine.session_tree.parent_of(session_id).is_some() {
        finalize_child_session(
            sessions,
            engine.session_tree,
            session_id,
            turn_outcome,
            io.output,
            io.deferred_inputs,
        )
        .await;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn next_wait_deadline(sessions: &HashMap<SessionId, SessionContext>) -> Option<Instant> {
    sessions
        .values()
        .filter_map(|session| {
            session
                .suspended_wait
                .as_ref()
                .and_then(SuspendedWait::timeout_at)
        })
        .min()
}

#[cfg_attr(not(test), allow(dead_code))]
async fn drain_wait_timeouts(
    sessions: &mut HashMap<SessionId, SessionContext>,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) {
    let now = Instant::now();
    let expired: Vec<(SessionId, &'static str)> = sessions
        .iter()
        .filter_map(|(session_id, session)| {
            let wait = session.suspended_wait.as_ref()?;
            let deadline = wait.timeout_at()?;
            (deadline <= now).then_some((*session_id, wait.timeout_message()))
        })
        .collect();

    for (session_id, message) in expired {
        resume_session_from_wait(
            sessions,
            session_id,
            ToolOutcome::Error {
                message: message.to_string(),
            },
            io,
            engine,
        )
        .await;
    }
}

#[cfg_attr(not(test), allow(dead_code))]
async fn handle_send_message_input(
    sessions: &mut HashMap<SessionId, SessionContext>,
    output: &mpsc::Sender<CoreOutput>,
    from: SessionId,
    to: SessionId,
    content: String,
    deferred_inputs: &mut VecDeque<CoreInput>,
    wake_waits: &Notify,
) {
    debug_log_session(from, format!("received SendMessage to {to:?}"));
    if let Some(target_session) = sessions.get_mut(&to) {
        let message = MailboxMessage {
            from,
            content: content.clone(),
        };
        let should_resume = matches!(
            target_session.suspended_wait.as_ref(),
            Some(SuspendedWait::Mailbox { source, .. }) if mailbox_matches_source(&message, source)
        );
        target_session.mailbox.push_back(message);
        if should_resume {
            deferred_inputs.push_back(CoreInput::ToolResult {
                session_id: to,
                tool_use_id: "__resume_recv_message__".into(),
                result: ToolOutcome::Success {
                    output: String::new(),
                },
            });
            wake_waits.notify_one();
        }
        let _ = output
            .send(CoreOutput::MessageReceived {
                session_id: to,
                from,
                content,
            })
            .await;
    }
}

/// Send the current conversation to the LLM and stream the response.
#[allow(dead_code)]
async fn call_llm(
    provider: &dyn LlmProvider,
    session: &mut SessionContext,
    session_id: SessionId,
    output: &tokio::sync::mpsc::Sender<CoreOutput>,
) -> Result<LlmCallResult, CoreError> {
    let messages = build_provider_messages(session).await?;
    call_llm_with_messages(
        provider,
        &messages,
        &session.tools,
        session_id,
        Some(output),
    )
    .await
}

async fn build_provider_messages(session: &mut SessionContext) -> Result<Vec<Message>, CoreError> {
    refresh_scoped_memory_state(session);
    let previous_selection =
        if session.last_prompt_memory_user_index == latest_user_message_index(&session.history) {
            session.last_prompt_memory.selected_entry_ids.clone()
        } else {
            Vec::new()
        };
    let injection = build_prompt_memory_injection(
        &session.scoped_memory_resolution.readable_scopes,
        session.persisted_config.prompt_memory_mode,
        &session.history,
        &previous_selection,
        session.scoped_memory_resolution.conflict_resolution,
    )
    .await
    .map_err(|error| CoreError::Internal {
        message: format!("failed to build prompt memory injection: {error}"),
    })?;
    session.last_prompt_memory = injection.summary.clone();
    session.last_prompt_memory_user_index = injection.latest_user_index;
    let mut diagnostics = session
        .last_memory_diagnostics
        .clone()
        .unwrap_or_else(|| default_turn_diagnostics_for_session(session));
    diagnostics.prompt_memory.mode = injection.diagnostics.mode;
    diagnostics.prompt_memory.injection_ran = injection.diagnostics.injection_ran;
    diagnostics.prompt_memory.status = injection.diagnostics.status;
    diagnostics.prompt_memory.reason = injection.diagnostics.reason;
    diagnostics.prompt_memory.selected_entries = injection.diagnostics.selected_entries.clone();
    diagnostics.prompt_memory.skipped_entries = injection.diagnostics.skipped_entries.clone();
    diagnostics.prompt_memory.truncated = injection.diagnostics.truncated;
    diagnostics.persistent_memory.readable_scopes = session
        .scoped_persistent_memory_state
        .readable_scopes
        .clone();
    diagnostics.persistent_memory.writable_scope = session
        .scoped_persistent_memory_state
        .writable_scope
        .clone();
    diagnostics.persistent_memory.conflict_resolution =
        Some(session.scoped_persistent_memory_state.conflict_resolution);
    diagnostics.persistent_memory.conflict_winner_scope =
        injection.diagnostics.conflict_winner_scope.clone();
    session.last_memory_diagnostics = Some(diagnostics);

    let mut messages = session.history.clone();
    if let Some(system_suffix) = injection.system_prompt_suffix.as_deref() {
        if let Some(first) = messages.first_mut() {
            if first.role == quine_llm::Role::System {
                if let MessageContent::Text(text) = &mut first.content {
                    text.push_str("\n\n");
                    text.push_str(system_suffix);
                }
            }
        }
    }
    Ok(splice_prompt_memory_messages(&messages, &injection))
}

fn latest_user_message_index(history: &[Message]) -> Option<usize> {
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| (message.role == quine_llm::Role::User).then_some(index))
}

async fn call_llm_with_messages(
    provider: &dyn LlmProvider,
    messages: &[Message],
    tools: &[ToolDefinition],
    session_id: SessionId,
    output: Option<&tokio::sync::mpsc::Sender<CoreOutput>>,
) -> Result<LlmCallResult, CoreError> {
    let stream_result = provider
        .send(messages, tools)
        .await
        .map_err(|e| CoreError::LlmError {
            message: format!("{e:#}"),
        })?;

    let mut stream = stream_result;
    let mut full_text = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = None;

    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(LlmEvent::ReasoningDelta { text }) => {
                if let Some(output) = output {
                    let _ = output
                        .send(CoreOutput::ReasoningDelta {
                            session_id,
                            delta: text,
                        })
                        .await;
                }
            }
            Ok(LlmEvent::TextDelta { text }) => {
                full_text.push_str(&text);
                if let Some(output) = output {
                    let _ = output
                        .send(CoreOutput::StreamDelta {
                            session_id,
                            delta: text,
                        })
                        .await;
                }
            }
            Ok(LlmEvent::ToolCall {
                tool_use_id,
                tool_name,
                arguments,
            }) => {
                tool_calls.push(PendingToolCall {
                    tool_use_id,
                    tool_name,
                    arguments,
                });
            }
            Ok(LlmEvent::Done { usage: u }) => {
                usage = u;
                break;
            }
            Err(e) => {
                return Err(CoreError::LlmError {
                    message: format!("{e:#}"),
                });
            }
        }
    }

    let tool_calls = deduplicate_pending_tool_calls(session_id, tool_calls);
    let turn = if tool_calls.is_empty() {
        LlmTurnResult::Text(full_text)
    } else {
        LlmTurnResult::ToolCalls {
            text_before: if full_text.is_empty() {
                None
            } else {
                Some(full_text)
            },
            calls: tool_calls,
        }
    };
    Ok(LlmCallResult { turn, usage })
}

#[cfg_attr(not(test), allow(dead_code))]
async fn call_llm_interruptible(
    provider: &dyn LlmProvider,
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_tree: &mut SessionTree,
) -> Result<Option<LlmCallResult>, CoreError> {
    let send_future = provider.send(&history, &tools);
    tokio::pin!(send_future);

    let mut stream = loop {
        tokio::select! {
            stream_result = &mut send_future => {
                break stream_result.map_err(|e| CoreError::LlmError {
                    message: format!("{e:#}"),
                })?;
            }
            maybe_input = io.input.recv() => {
                match maybe_input {
                    Some(input) => {
                        if matches!(
                            handle_session_control_input(
                                input,
                                session_id,
                                sessions,
                                session_tree,
                                io.deferred_inputs,
                            ),
                            SessionControlFlow::Interrupted
                        ) {
                            debug_log_session(session_id, "LLM request interrupted before stream opened");
                            return Ok(None);
                        }
                    }
                    None => {
                        return Err(CoreError::Internal {
                            message: "input channel closed while awaiting LLM response".into(),
                        });
                    }
                }
            }
        }
    };

    let mut full_text = String::new();
    let mut tool_calls = Vec::new();
    let mut usage = None;

    loop {
        tokio::select! {
            event_result = stream.next() => {
                let Some(event_result) = event_result else {
                    break;
                };
                match event_result {
                    Ok(LlmEvent::ReasoningDelta { text }) => {
                        let _ = io
                            .output
                            .send(CoreOutput::ReasoningDelta {
                                session_id,
                                delta: text,
                            })
                            .await;
                    }
                    Ok(LlmEvent::TextDelta { text }) => {
                        full_text.push_str(&text);
                        let _ = io
                            .output
                            .send(CoreOutput::StreamDelta {
                                session_id,
                                delta: text,
                            })
                            .await;
                    }
                    Ok(LlmEvent::ToolCall {
                        tool_use_id,
                        tool_name,
                        arguments,
                    }) => {
                        tool_calls.push(PendingToolCall {
                            tool_use_id,
                            tool_name,
                            arguments,
                        });
                    }
                    Ok(LlmEvent::Done { usage: u }) => {
                        usage = u;
                        break;
                    }
                    Err(e) => {
                        return Err(CoreError::LlmError {
                            message: format!("{e:#}"),
                        });
                    }
                }
            }
            maybe_input = io.input.recv() => {
                match maybe_input {
                    Some(input) => {
                        if matches!(
                            handle_session_control_input(
                                input,
                                session_id,
                                sessions,
                                session_tree,
                                io.deferred_inputs,
                            ),
                            SessionControlFlow::Interrupted
                        ) {
                            debug_log_session(session_id, "LLM stream interrupted");
                            return Ok(None);
                        }
                    }
                    None => {
                        return Err(CoreError::Internal {
                            message: "input channel closed while streaming LLM response".into(),
                        });
                    }
                }
            }
        }
    }

    let tool_calls = deduplicate_pending_tool_calls(session_id, tool_calls);
    let turn = if tool_calls.is_empty() {
        LlmTurnResult::Text(full_text)
    } else {
        LlmTurnResult::ToolCalls {
            text_before: if full_text.is_empty() {
                None
            } else {
                Some(full_text)
            },
            calls: tool_calls,
        }
    };
    Ok(Some(LlmCallResult { turn, usage }))
}

async fn summarize_history(
    provider: &dyn LlmProvider,
    session_id: SessionId,
    archive_ref: &str,
    trigger: CompactionTrigger,
    history: &[Message],
) -> Result<String, CoreError> {
    let messages = compaction::summarizer_messages(archive_ref, trigger, history);
    match call_llm_with_messages(provider, &messages, &[], session_id, None).await? {
        LlmCallResult {
            turn: LlmTurnResult::Text(summary),
            ..
        } => Ok(summary.trim().to_string()),
        LlmCallResult {
            turn: LlmTurnResult::ToolCalls { .. },
            ..
        } => Err(CoreError::LlmError {
            message: "summarizer unexpectedly requested tool calls".into(),
        }),
    }
}

async fn generate_session_listing_summary(
    provider: &dyn LlmProvider,
    session_id: SessionId,
    session_summary_markdown: &str,
) -> Result<Option<String>, CoreError> {
    let messages = [
        Message::system(
            "You generate session list summaries for a coding-agent UI. Reply with exactly one concise sentence describing the current session. Do not use bullets, prefixes, or markdown.",
        ),
        Message::user(format!(
            "Session memory summary:\n\n{session_summary_markdown}\n\nReturn one sentence, 6 to 18 words, focused on the current task and most relevant progress."
        )),
    ];
    match call_llm_with_messages(provider, &messages, &[], session_id, None).await? {
        LlmCallResult {
            turn: LlmTurnResult::Text(summary),
            ..
        } => Ok(normalize_listing_summary(&summary)),
        LlmCallResult {
            turn: LlmTurnResult::ToolCalls { .. },
            ..
        } => Err(CoreError::LlmError {
            message: "session listing summarizer unexpectedly requested tool calls".into(),
        }),
    }
}

fn normalize_listing_summary(summary: &str) -> Option<String> {
    let trimmed = summary.trim().trim_matches(|c| matches!(c, '"' | '\''));
    if trimmed.is_empty() {
        return None;
    }

    let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }

    Some(collapsed.chars().take(200).collect())
}

#[derive(Clone)]
struct PendingToolCall {
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
}

fn deduplicate_pending_tool_calls(
    session_id: SessionId,
    calls: Vec<PendingToolCall>,
) -> Vec<PendingToolCall> {
    let mut seen = HashSet::new();
    let mut deduped = Vec::with_capacity(calls.len());

    for call in calls {
        if seen.insert(call.tool_use_id.clone()) {
            deduped.push(call);
        } else {
            debug_log_session(
                session_id,
                format!(
                    "dropped duplicate tool call id={} name={}",
                    call.tool_use_id, call.tool_name
                ),
            );
        }
    }

    deduped
}

struct LlmCallResult {
    turn: LlmTurnResult,
    usage: Option<quine_llm::TokenUsage>,
}

enum LlmTurnResult {
    Text(String),
    ToolCalls {
        #[allow(dead_code)]
        text_before: Option<String>,
        calls: Vec<PendingToolCall>,
    },
}

#[cfg_attr(not(test), allow(dead_code))]
struct CoreIo<'a> {
    output: &'a mpsc::Sender<CoreOutput>,
    input: &'a mut mpsc::Receiver<CoreInput>,
    input_tx: &'a mpsc::Sender<CoreInput>,
    deferred_inputs: &'a mut VecDeque<CoreInput>,
}

#[cfg_attr(not(test), allow(dead_code))]
struct EngineState<'a> {
    provider: &'a Arc<dyn LlmProvider>,
    session_tree: &'a mut SessionTree,
}

enum TurnOutcome {
    Completed(Option<String>),
    Failed(String),
    Cancelled,
    Suspended,
}

#[cfg_attr(not(test), allow(dead_code))]
enum SessionControlFlow {
    Continue,
    Interrupted,
}

#[cfg_attr(not(test), allow(dead_code))]
struct PreparedConcurrentToolCall {
    index: usize,
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
    tool: Arc<dyn crate::tool::Tool>,
    filesystem: Arc<dyn crate::filesystem::SessionFilesystem>,
    working_directory: PathBuf,
    plan_store: crate::tool::plan::PlanStore,
    session_group: String,
    python_runtime: Arc<PythonRuntime>,
}

struct CompletedConcurrentToolCall {
    index: usize,
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
    result: ToolOutcome,
    duration_us: u64,
}

fn extract_plan_id_from_tool_output(output: &str) -> Option<String> {
    for line in output.lines() {
        let trimmed = line.trim();
        if let Some(plan_id) = trimmed.strip_prefix("plan_id:") {
            let plan_id = plan_id.trim();
            if !plan_id.is_empty() {
                return Some(plan_id.to_string());
            }
        }
        if let Some(rest) = trimmed.strip_prefix("Plan created (ID:") {
            let plan_id = rest.trim().strip_suffix(')')?.trim();
            if !plan_id.is_empty() {
                return Some(plan_id.to_string());
            }
        }
    }
    None
}

fn resolve_plan_tool_call_id(session: &SessionContext, tool_use_id: &str) -> Option<String> {
    session.history.iter().rev().find_map(|message| {
        let quine_llm::MessageContent::ToolResult {
            tool_use_id: result_tool_use_id,
            output,
            is_error,
        } = &message.content
        else {
            return None;
        };

        if *is_error || result_tool_use_id != tool_use_id {
            return None;
        }

        extract_plan_id_from_tool_output(output)
    })
}

fn normalize_plan_tool_arguments(
    session: &SessionContext,
    call: &PendingToolCall,
) -> PendingToolCall {
    if call.tool_name != "plan" {
        return call.clone();
    }
    if call
        .arguments
        .get("operation")
        .and_then(|value| value.as_str())
        != Some("update_plan")
    {
        return call.clone();
    }
    let Some(raw_plan_id) = call
        .arguments
        .get("plan_id")
        .and_then(|value| value.as_str())
    else {
        return call.clone();
    };
    if !raw_plan_id.starts_with("call_") {
        return call.clone();
    }
    let Some(resolved_plan_id) = resolve_plan_tool_call_id(session, raw_plan_id) else {
        return call.clone();
    };

    let mut normalized = call.clone();
    normalized.arguments["plan_id"] = serde_json::Value::String(resolved_plan_id);
    normalized
}

fn resolved_permission_path(
    session: &SessionContext,
    raw_path: Option<&str>,
    fallback: &str,
) -> PermissionResource {
    let candidate = raw_path.unwrap_or(fallback);
    let candidate_path = Path::new(candidate);
    match session.filesystem.resolve_path(candidate_path) {
        Ok(path) => PermissionResource::Path { path },
        Err(_) => PermissionResource::Path {
            path: if candidate_path.is_absolute() {
                candidate_path.to_path_buf()
            } else {
                session.working_directory.join(candidate_path)
            },
        },
    }
}

fn build_permission_request(
    session: &SessionContext,
    call: &PendingToolCall,
    tool: &dyn crate::tool::Tool,
) -> (PermissionRequest, Option<ToolLocalDecision>) {
    let scope = match call.tool_name.as_str() {
        "read_file" | "find" => PermissionScope::Read,
        "apply_patch" => PermissionScope::Write,
        "bash" => PermissionScope::Execute,
        "signal" => PermissionScope::ProcessControl,
        "spawn" | "subagent" | "wait_child" => PermissionScope::AgentControl,
        "send_message" | "recv_message" => PermissionScope::Read,
        "ask_user" | "plan" => PermissionScope::Read,
        _ => {
            if tool.is_read_only() {
                PermissionScope::Read
            } else {
                PermissionScope::Write
            }
        }
    };

    let resource = match call.tool_name.as_str() {
        "read_file" | "apply_patch" => resolved_permission_path(
            session,
            call.arguments.get("file_path").and_then(|v| v.as_str()),
            ".",
        ),
        "find" => resolved_permission_path(
            session,
            call.arguments.get("path").and_then(|v| v.as_str()),
            ".",
        ),
        "bash" => call
            .arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|command| PermissionResource::Command {
                descriptor: analyze_command(command),
            })
            .unwrap_or(PermissionResource::None),
        "signal" => call
            .arguments
            .get("session_id")
            .and_then(|v| v.as_str())
            .map(|target| PermissionResource::Process {
                target: target.to_string(),
            })
            .unwrap_or(PermissionResource::None),
        "spawn" | "subagent" | "wait_child" => PermissionResource::Agent {
            target: call.tool_name.clone(),
        },
        "send_message" | "recv_message" => PermissionResource::None,
        "ask_user" | "plan" => PermissionResource::None,
        _ => PermissionResource::None,
    };

    let local = if call.tool_name == "bash" {
        call.arguments
            .get("command")
            .and_then(|v| v.as_str())
            .map(|command| {
                let trimmed = command.trim();
                if trimmed == "rm -rf /" || trimmed.starts_with("rm -rf /") {
                    ToolLocalDecision {
                        decision: crate::permission::types::PermissionDecision::Deny,
                        reason: Some("dangerous destructive shell command".into()),
                    }
                } else {
                    ToolLocalDecision {
                        decision: crate::permission::types::PermissionDecision::Defer,
                        reason: Some(
                            "bash delegates final permission decision to the shared engine".into(),
                        ),
                    }
                }
            })
    } else {
        Some(ToolLocalDecision {
            decision: crate::permission::types::PermissionDecision::Defer,
            reason: Some("tool defers to the shared permission engine".into()),
        })
    };

    (
        PermissionRequest {
            tool_name: call.tool_name.clone(),
            action: None,
            scope,
            resource,
        },
        local,
    )
}

fn session_output(session: &SessionContext) -> Option<String> {
    session.history.iter().rev().find_map(|message| {
        if message.role != quine_llm::Role::Assistant {
            return None;
        }

        match &message.content {
            MessageContent::Text(text) => Some(text.clone()),
            MessageContent::ToolUse { text, .. } => text.clone(),
            MessageContent::ToolResult { .. } => None,
        }
    })
}

#[cfg_attr(not(test), allow(dead_code))]
fn interrupt_session(
    sessions: &mut HashMap<SessionId, SessionContext>,
    deferred_inputs: &mut VecDeque<CoreInput>,
    session_id: SessionId,
) {
    if let Some(session) = sessions.get_mut(&session_id) {
        session.state = SessionState::Idle;
        session.interrupted = true;
        if let Some(cancel_tx) = &session.cancel_tx {
            let _ = cancel_tx.send(true);
        }
        session.cancel_tx = None;
        session.pending_interaction.take();
        session.pending_permission_approval = None;
    }

    deferred_inputs.retain(|input| {
        !matches!(
            input,
            CoreInput::InteractionResponse { session_id: sid, .. }
                | CoreInput::Cancel { session_id: sid }
                | CoreInput::Signal { session_id: sid, .. }
                | CoreInput::ToolResult { session_id: sid, .. }
                if *sid == session_id
        )
    });
}

#[cfg_attr(not(test), allow(dead_code))]
fn handle_session_control_input(
    input: CoreInput,
    session_id: SessionId,
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_tree: &mut SessionTree,
    deferred_inputs: &mut VecDeque<CoreInput>,
) -> SessionControlFlow {
    match input {
        CoreInput::Cancel {
            session_id: cancel_sid,
        } if cancel_sid == session_id => {
            clear_suspended_wait(sessions, session_tree, session_id);
            interrupt_session(sessions, deferred_inputs, session_id);
            SessionControlFlow::Interrupted
        }
        CoreInput::Signal {
            session_id: signal_sid,
            signal,
        } if signal_sid == session_id
            && matches!(
                signal,
                SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill
            ) =>
        {
            clear_suspended_wait(sessions, session_tree, session_id);
            interrupt_session(sessions, deferred_inputs, session_id);
            SessionControlFlow::Interrupted
        }
        other => {
            deferred_inputs.push_back(other);
            SessionControlFlow::Continue
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
fn tool_call_is_concurrency_eligible(session: &SessionContext, call: &PendingToolCall) -> bool {
    CONCURRENT_TOOL_BATCH_ALLOWLIST.contains(&call.tool_name.as_str())
        && session
            .tool_registry
            .get(&call.tool_name)
            .is_some_and(|tool| {
                !tool.is_interactive() && tool.is_read_only() && tool.is_idempotent()
            })
}

#[cfg_attr(not(test), allow(dead_code))]
fn tool_batch_is_concurrency_eligible(session: &SessionContext, calls: &[PendingToolCall]) -> bool {
    calls.len() > 1
        && calls
            .iter()
            .all(|call| tool_call_is_concurrency_eligible(session, call))
}

#[cfg_attr(not(test), allow(dead_code))]
fn partition_tool_calls_by_concurrency<'a>(
    session: &SessionContext,
    calls: &'a [PendingToolCall],
) -> Vec<(bool, &'a [PendingToolCall])> {
    let mut partitions = Vec::new();
    let mut start = 0;

    while start < calls.len() {
        let is_concurrent = tool_call_is_concurrency_eligible(session, &calls[start]);
        let mut end = start + 1;
        while end < calls.len()
            && tool_call_is_concurrency_eligible(session, &calls[end]) == is_concurrent
        {
            end += 1;
        }
        partitions.push((is_concurrent, &calls[start..end]));
        start = end;
    }

    partitions
}

#[cfg_attr(not(test), allow(dead_code))]
fn prepare_concurrent_tool_calls(
    session: &SessionContext,
    calls: &[PendingToolCall],
) -> Option<Vec<PreparedConcurrentToolCall>> {
    calls
        .iter()
        .enumerate()
        .map(|(index, call)| {
            let tool = session.tool_registry.get(&call.tool_name)?;
            Some(PreparedConcurrentToolCall {
                index,
                tool_use_id: call.tool_use_id.clone(),
                tool_name: call.tool_name.clone(),
                arguments: call.arguments.clone(),
                tool: Arc::clone(tool),
                filesystem: Arc::clone(&session.filesystem),
                working_directory: session.working_directory.clone(),
                plan_store: session.plan_store.clone(),
                session_group: session.python_group.clone(),
                python_runtime: Arc::clone(&session.python_runtime),
            })
        })
        .collect()
}

#[cfg_attr(not(test), allow(dead_code))]
async fn execute_concurrent_tool_batch(
    calls: Vec<PreparedConcurrentToolCall>,
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
) -> Vec<CompletedConcurrentToolCall> {
    let (cancel_tx, cancellation) = CancellationChannel::new_pair();
    let Some(session) = sessions.get_mut(&session_id) else {
        return Vec::new();
    };
    if session.interrupted {
        return Vec::new();
    }
    session.cancel_tx = Some(cancel_tx.clone());

    let mut results: Vec<Option<CompletedConcurrentToolCall>> =
        std::iter::repeat_with(|| None).take(calls.len()).collect();
    let mut join_set = JoinSet::new();
    for call in calls {
        let cancellation = cancellation.clone();
        let input_tx = io.input_tx.clone();
        join_set.spawn(async move {
            let ctx = ExecutionContext {
                session_id,
                filesystem: call.filesystem,
                working_directory: call.working_directory,
                interaction_channel: None,
                plan_store: call.plan_store,
                session_group: call.session_group,
                python_runtime: call.python_runtime,
                core_input: Some(input_tx),
                cancellation,
            };
            let started_at = std::time::Instant::now();
            let result = match call.tool.execute(call.arguments.clone(), &ctx).await {
                Ok(tool_output) if tool_output.is_error => ToolOutcome::Error {
                    message: tool_output.content,
                },
                Ok(tool_output) => ToolOutcome::Success {
                    output: tool_output.content,
                },
                Err(ToolError::Cancelled) => ToolOutcome::Cancelled,
                Err(tool_err) => ToolOutcome::Error {
                    message: tool_err.to_string(),
                },
            };
            CompletedConcurrentToolCall {
                index: call.index,
                tool_use_id: call.tool_use_id,
                tool_name: call.tool_name,
                arguments: call.arguments,
                result,
                duration_us: started_at.elapsed().as_micros() as u64,
            }
        });
    }

    let mut remaining = results.len();
    while remaining > 0 {
        tokio::select! {
            joined = join_set.join_next() => {
                match joined {
                    Some(Ok(result)) => {
                        let index = result.index;
                        results[index] = Some(result);
                        remaining -= 1;
                    }
                    Some(Err(join_err)) => {
                        debug_log_session(
                            session_id,
                            format!("concurrent tool task panicked: {join_err}"),
                        );
                        remaining -= 1;
                    }
                    None => break,
                }
            }
            maybe_input = io.input.recv() => {
                match maybe_input {
                    Some(CoreInput::Cancel { session_id: cancel_sid }) if cancel_sid == session_id => {
                        debug_log_session(session_id, "concurrent tool batch cancelled by core input");
                        interrupt_session(sessions, io.deferred_inputs, session_id);
                        let _ = cancel_tx.send(true);
                    }
                    Some(CoreInput::Signal { session_id: signal_sid, signal })
                        if signal_sid == session_id
                            && matches!(signal, SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill) =>
                    {
                        debug_log_session(
                            session_id,
                            "concurrent tool batch interrupted by session signal",
                        );
                        interrupt_session(sessions, io.deferred_inputs, session_id);
                        let _ = cancel_tx.send(true);
                    }
                    Some(other) => {
                        io.deferred_inputs.push_back(other);
                    }
                    None => {
                        debug_log_session(
                            session_id,
                            "concurrent tool batch failed: input channel closed during execution",
                        );
                        let _ = cancel_tx.send(true);
                    }
                }
            }
        }
    }

    if let Some(session) = sessions.get_mut(&session_id) {
        if let Some(active_cancel_tx) = session.cancel_tx.as_ref() {
            let _ = active_cancel_tx.send(true);
        }
        session.cancel_tx = None;
    }

    results.into_iter().flatten().collect()
}

async fn compact_session_history(
    provider: &dyn LlmProvider,
    session: &mut SessionContext,
    session_id: SessionId,
    trigger: CompactionTrigger,
) -> Result<bool, CoreError> {
    let (prefix, _) = compaction::split_history_for_compaction(&session.history);
    let non_system_messages = prefix
        .iter()
        .filter(|message| message.role != quine_llm::Role::System)
        .count();
    if non_system_messages == 0 {
        return Ok(false);
    }

    let session_id_str = session_id_string(session_id);
    let generation = session.compaction_generation + 1;
    let archived = compaction::archive_history(
        &session.archive_root,
        &session_id_str,
        generation,
        trigger,
        &session.history,
    )
    .await
    .map_err(|error| CoreError::Internal {
        message: format!("failed to archive transcript: {error}"),
    })?;
    let archive_ref = archived.path.display().to_string();
    let plan = if let Some(plan) =
        compaction::session_memory_compaction_plan(&session.session_memory, &session.history).await
    {
        let diagnostics = ensure_turn_diagnostics(session);
        diagnostics.session_memory.compaction.status = MemoryStatus::Succeeded;
        diagnostics.session_memory.compaction.source =
            Some(CompactionSourceDiagnostics::SessionMemory);
        diagnostics.session_memory.compaction.reason = None;
        diagnostics.session_memory.compaction.tail_start = Some(plan.tail_start);
        plan
    } else {
        let summary =
            summarize_history(provider, session_id, &archive_ref, trigger, &prefix).await?;
        let plan = compaction::legacy_compaction_plan(&session.history, summary);
        let diagnostics = ensure_turn_diagnostics(session);
        diagnostics.session_memory.compaction.status = MemoryStatus::Skipped;
        diagnostics.session_memory.compaction.source =
            Some(CompactionSourceDiagnostics::LegacySummarizer);
        diagnostics.session_memory.compaction.reason = Some(MemoryDecisionReason::Fallback);
        diagnostics.session_memory.compaction.tail_start = Some(plan.tail_start);
        plan
    };
    if plan.source == compaction::CompactionSource::LegacySummarizer {
        debug_log_session(session_id, "compaction used legacy summarizer");
    } else {
        debug_log_session(session_id, "compaction used session memory");
    }
    session.history = compaction::apply_compaction_plan(&session.history, &archive_ref, &plan);
    session.last_input_tokens = None;
    session.compaction_generation = archived.generation;
    Ok(true)
}

#[cfg_attr(not(test), allow(dead_code))]
fn clear_suspended_wait(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_tree: &mut SessionTree,
    session_id: SessionId,
) {
    session_tree.clear_active_wait(session_id);
    if let Some(session) = sessions.get_mut(&session_id) {
        session.suspended_wait = None;
        if session.state == SessionState::Waiting {
            session.state = SessionState::Idle;
        }
    }
}

fn session_id_string(session_id: SessionId) -> String {
    serde_json::to_value(session_id)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown-session".to_string())
}

#[derive(Clone, Copy)]
enum StatusReportStage {
    ReviewingResults,
    Completed,
}

fn tool_call_count_label(count: usize) -> String {
    if count == 1 {
        "1 tool call".to_string()
    } else {
        format!("{count} tool calls")
    }
}

fn should_emit_periodic_status_report(tool_rounds_observed: u32, threshold: u32) -> bool {
    let threshold = threshold.max(1);
    tool_rounds_observed >= threshold && tool_rounds_observed.is_multiple_of(threshold)
}

fn should_emit_completed_status_report(
    tool_rounds_observed: u32,
    threshold: u32,
    prior_report_exists: bool,
) -> bool {
    prior_report_exists || should_emit_periodic_status_report(tool_rounds_observed, threshold)
}

fn status_report_progress(
    tool_rounds_observed: u32,
    threshold: u32,
    stage: StatusReportStage,
) -> u8 {
    if matches!(stage, StatusReportStage::Completed) {
        return 100;
    }

    let threshold = threshold.max(1);
    let estimated_total_rounds = tool_rounds_observed
        .saturating_add((threshold / 2).max(2))
        .max(tool_rounds_observed);
    let total_units = estimated_total_rounds.saturating_mul(2).max(1);
    let completed_units = match stage {
        StatusReportStage::ReviewingResults | StatusReportStage::Completed => {
            tool_rounds_observed.saturating_mul(2)
        }
    };
    let raw_percent = completed_units.saturating_mul(100) / total_units;
    let bounded = match stage {
        StatusReportStage::ReviewingResults => raw_percent.clamp(10, 95),
        StatusReportStage::Completed => 100,
    };
    u8::try_from(bounded).unwrap_or(100)
}

fn status_report_confidence(stage: StatusReportStage, prior_confidence: Option<u8>) -> u8 {
    match stage {
        StatusReportStage::Completed => prior_confidence.unwrap_or(85).clamp(1, 100),
        StatusReportStage::ReviewingResults => prior_confidence.unwrap_or(60).clamp(1, 95),
    }
}

fn build_status_report(
    tool_rounds_observed: u32,
    threshold: u32,
    current_round_tool_calls: usize,
    stage: StatusReportStage,
    prior_confidence: Option<u8>,
) -> SessionStatusReport {
    let tool_calls = tool_call_count_label(current_round_tool_calls);
    let completed_summary = match stage {
        StatusReportStage::ReviewingResults => format!(
            "Completed {tool_rounds_observed} tool rounds so far; the latest round finished {tool_calls}."
        ),
        StatusReportStage::Completed => format!(
            "Completed {tool_rounds_observed} tool rounds and finished the response for this turn."
        ),
    };
    let remaining_summary = match stage {
        StatusReportStage::ReviewingResults => "Review the latest tool results, decide whether another tool round is needed, and then produce the final response.".to_string(),
        StatusReportStage::Completed => {
            "Nothing remains in this turn; wait for the next user request.".to_string()
        }
    };

    SessionStatusReport::new(
        !matches!(stage, StatusReportStage::Completed),
        status_report_progress(tool_rounds_observed, threshold, stage),
        status_report_confidence(stage, prior_confidence),
        completed_summary,
        remaining_summary,
        tool_rounds_observed,
    )
}

#[cfg_attr(not(test), allow(dead_code))]
async fn set_session_status_report(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    report: Option<SessionStatusReport>,
) {
    let Some(session) = sessions.get_mut(&session_id) else {
        return;
    };
    if session.status_report == report {
        return;
    }
    session.status_report = report.clone();
    let _ = output
        .send(CoreOutput::SessionStatusReport { session_id, report })
        .await;
}

#[derive(Deserialize)]
struct ModelStatusReportPayload {
    progress_percent: u8,
    confidence_percent: u8,
    completed_summary: String,
    remaining_summary: String,
}

fn current_turn_messages(history: &[Message]) -> &[Message] {
    latest_user_message_index(history)
        .map(|index| &history[index..])
        .unwrap_or(history)
}

fn latest_user_request_text(history: &[Message]) -> Option<&str> {
    current_turn_messages(history).iter().find_map(|message| {
        if message.role != quine_llm::Role::User {
            return None;
        }
        match &message.content {
            MessageContent::Text(text) => {
                let trimmed = text.trim();
                (!trimmed.is_empty()).then_some(trimmed)
            }
            MessageContent::ToolUse { .. } | MessageContent::ToolResult { .. } => None,
        }
    })
}

fn truncate_status_report_text(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }

    let mut truncated = String::with_capacity(max_chars + 1);
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= max_chars.saturating_sub(1) {
            break;
        }
        truncated.push(ch);
    }
    truncated.push('…');
    truncated
}

fn render_status_report_transcript(history: &[Message]) -> String {
    current_turn_messages(history)
        .iter()
        .map(|message| match &message.content {
            MessageContent::Text(text) => format!(
                "{}: {}",
                match message.role {
                    quine_llm::Role::System => "system",
                    quine_llm::Role::User => "user",
                    quine_llm::Role::Assistant => "assistant",
                    quine_llm::Role::Tool => "tool",
                },
                truncate_status_report_text(text, 400)
            ),
            MessageContent::ToolUse { text, tool_calls } => {
                let tool_names = tool_calls
                    .iter()
                    .map(|call| call.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let prefix = text
                    .as_deref()
                    .map(|value| truncate_status_report_text(value, 240))
                    .filter(|value| !value.is_empty())
                    .unwrap_or_else(|| "assistant requested tools".to_string());
                format!(
                    "assistant: {prefix} [tool round with {}: {tool_names}]",
                    tool_call_count_label(tool_calls.len())
                )
            }
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => format!(
                "tool: {} result for {tool_use_id}: {}",
                if *is_error { "error" } else { "ok" },
                truncate_status_report_text(output, 400)
            ),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_status_report_payload(raw: &str) -> Option<ModelStatusReportPayload> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<ModelStatusReportPayload>(trimmed) {
        return Some(parsed);
    }

    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end)
        .then(|| serde_json::from_str::<ModelStatusReportPayload>(&trimmed[start..=end]).ok())
        .flatten()
}

async fn generate_status_report_with_model(
    session: &SessionContext,
    session_id: SessionId,
    current_round_tool_calls: usize,
    stage: StatusReportStage,
) -> SessionStatusReport {
    let transcript = render_status_report_transcript(&session.history);
    let latest_user_request =
        latest_user_request_text(&session.history).unwrap_or("No explicit user request captured.");
    let tool_rounds_observed = session.current_turn_tool_rounds;
    let threshold = session
        .persisted_config
        .status_report_min_tool_rounds
        .max(1);
    let completion_hint = match stage {
        StatusReportStage::Completed => "The turn has completed. Set progress_percent to 100.",
        StatusReportStage::ReviewingResults => {
            "The turn is still active after a tool round. Set progress_percent between 1 and 95."
        }
    };
    let messages = [
        Message::system(
            "You generate internal status reports for a coding-agent UI. Return JSON only with keys progress_percent, confidence_percent, completed_summary, and remaining_summary. completed_summary and remaining_summary must each be exactly one sentence. Evaluate progress_percent against the full amount of work required by the latest user request, not against tool counts, elapsed time, or transcript length. Set confidence_percent to your confidence that the agent can fully complete the latest user request from the current trajectory and evidence. Do not include markdown fences or extra commentary.",
        ),
        Message::user(format!(
            "Generate a concise status report for this in-progress coding turn.\n\
             Latest user request:\n{latest_user_request}\n\n\
             Tool rounds observed in this turn: {tool_rounds_observed}\n\
             Reporting threshold: {threshold}\n\
             Current round tool calls: {current_round_tool_calls}\n\
             Estimate confidence_percent as your confidence that the agent can fully complete the latest user request from the current trajectory and evidence.\n\
             Do not use tool rounds, tool calls, or transcript length as a proxy for progress.\n\
             Use tool activity only as evidence for what is done and what still needs work.\n\
             {completion_hint}\n\n\
             Current user-turn transcript:\n{transcript}"
        )),
    ];

    match call_llm_with_messages(session.provider.as_ref(), &messages, &[], session_id, None).await
    {
        Ok(LlmCallResult {
            turn: LlmTurnResult::Text(text),
            ..
        }) => {
            if let Some(payload) = parse_status_report_payload(&text) {
                let progress_percent = match stage {
                    StatusReportStage::Completed => 100,
                    _ => payload.progress_percent.clamp(1, 95),
                };
                let confidence_percent = payload.confidence_percent.clamp(1, 100);
                SessionStatusReport::new(
                    !matches!(stage, StatusReportStage::Completed),
                    progress_percent,
                    confidence_percent,
                    payload
                        .completed_summary
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    payload
                        .remaining_summary
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    tool_rounds_observed,
                )
            } else {
                build_status_report(
                    tool_rounds_observed,
                    threshold,
                    current_round_tool_calls,
                    stage,
                    session
                        .status_report
                        .as_ref()
                        .map(|report| report.confidence_percent),
                )
            }
        }
        _ => build_status_report(
            tool_rounds_observed,
            threshold,
            current_round_tool_calls,
            stage,
            session
                .status_report
                .as_ref()
                .map(|report| report.confidence_percent),
        ),
    }
}

async fn prepare_tool_result_for_history(
    session: &SessionContext,
    session_id: SessionId,
    tool_use_id: &str,
    tool_name: &str,
    tool_output: &str,
    is_error: bool,
) -> Result<String, CoreError> {
    if tool_output.chars().count() <= compaction::MAX_TOOL_RESULT_CHARS_IN_HISTORY {
        return Ok(tool_output.to_string());
    }

    let session_id_str = session_id_string(session_id);
    let archived = compaction::archive_tool_result(
        &session.archive_root,
        &session_id_str,
        tool_use_id,
        tool_output,
    )
    .await
    .map_err(|error| CoreError::Internal {
        message: format!("failed to archive oversized tool result: {error}"),
    })?;
    let archive_ref = archived.display().to_string();
    debug_log_session(
        session_id,
        format!(
            "archived oversized tool result for `{tool_name}` ({} chars) to {archive_ref}",
            tool_output.chars().count()
        ),
    );
    Ok(compaction::render_initial_archived_tool_result(
        tool_name,
        tool_use_id,
        is_error,
        tool_output,
        &archive_ref,
    ))
}

async fn archive_old_tool_results_in_history(
    session: &mut SessionContext,
    session_id: SessionId,
) -> Result<(), CoreError> {
    let session_id_str = session_id_string(session_id);
    session.history = compaction::archive_old_tool_results(
        &session.archive_root,
        &session_id_str,
        &session.history,
    )
    .await
    .map_err(|error| CoreError::Internal {
        message: format!("failed to archive old tool results: {error}"),
    })?;
    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
async fn snapshot_sessions(
    sessions: &HashMap<SessionId, SessionContext>,
    session_tree: &SessionTree,
) -> CoreCheckpoint {
    let mut persisted_sessions = Vec::new();
    for (session_id, session) in sessions {
        if let Some(persisted) = session.snapshot(*session_id).await {
            persisted_sessions.push(persisted);
        }
    }
    CoreCheckpoint::new(persisted_sessions, session_tree.snapshot())
}

#[cfg_attr(not(test), allow(dead_code))]
async fn emit_checkpoint_request(
    sessions: &HashMap<SessionId, SessionContext>,
    session_tree: &SessionTree,
    output: &mpsc::Sender<CoreOutput>,
) {
    let checkpoint = snapshot_sessions(sessions, session_tree).await;
    let _ = output
        .send(CoreOutput::CheckpointRequested { checkpoint })
        .await;
}

#[cfg_attr(not(test), allow(dead_code))]
async fn finalize_child_session(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_tree: &mut SessionTree,
    session_id: SessionId,
    turn_outcome: TurnOutcome,
    output: &mpsc::Sender<CoreOutput>,
    deferred_inputs: &mut VecDeque<CoreInput>,
) {
    let Some(session) = sessions.get_mut(&session_id) else {
        return;
    };
    session.state = SessionState::Destroyed;
    let status = match turn_outcome {
        TurnOutcome::Completed(text) => ExitStatus::Success {
            output: text.or_else(|| session_output(session)).unwrap_or_default(),
        },
        TurnOutcome::Failed(error) => ExitStatus::Failed { error },
        TurnOutcome::Cancelled => ExitStatus::Cancelled,
        TurnOutcome::Suspended => return,
    };

    let parent_id = session_tree.parent_of(session_id);
    session_tree.record_exit(session_id, status.clone());
    session_tree.clear_active_wait(session_id);

    let waiting_parents: Vec<SessionId> = sessions
        .iter()
        .filter_map(
            |(candidate_id, candidate)| match candidate.suspended_wait.as_ref() {
                Some(SuspendedWait::ChildExit { child_id, .. }) if *child_id == session_id => {
                    Some(*candidate_id)
                }
                _ => None,
            },
        )
        .collect();

    let _ = output
        .send(CoreOutput::SessionStateChanged {
            session_id,
            state: SessionState::Destroyed,
        })
        .await;

    if let Some(parent_id) = parent_id {
        let _ = output
            .send(CoreOutput::ChildExited {
                parent_id,
                child_id: session_id,
                status,
            })
            .await;
    }

    sessions.remove(&session_id);
    for waiting_parent in waiting_parents {
        deferred_inputs.push_back(CoreInput::ToolResult {
            session_id: waiting_parent,
            tool_use_id: "__resume_wait_child__".into(),
            result: ToolOutcome::Success {
                output: String::new(),
            },
        });
    }
    emit_checkpoint_request(sessions, session_tree, output).await;
}

#[allow(clippy::too_many_arguments)]
#[cfg_attr(not(test), allow(dead_code))]
async fn start_child_session(
    sessions: &mut HashMap<SessionId, SessionContext>,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
    parent_id: SessionId,
    child_id: SessionId,
    task: String,
    system_prompt: Option<String>,
    prompt_behavior: PermissionPromptBehavior,
    permission_rules: PermissionRuleSet,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
) -> Result<(), String> {
    if sessions.contains_key(&child_id) {
        return Err("session already exists".into());
    }

    let work_dir = std::env::current_dir().unwrap_or_default();
    let work_dir_display = work_dir.display().to_string();
    let inherited_provider = sessions
        .get(&parent_id)
        .map(|parent| Arc::clone(&parent.provider))
        .unwrap_or_else(|| Arc::clone(engine.provider));
    let inherited_model_profile = sessions
        .get(&parent_id)
        .and_then(|parent| parent.persisted_config.model_profile.clone());
    let inherited_max_context_window = sessions
        .get(&parent_id)
        .and_then(|parent| parent.max_context_window)
        .or(max_context_window);
    let inherited_session_group = sessions
        .get(&parent_id)
        .and_then(|parent| parent.persisted_config.session_group.clone());
    let inherited_python_runtime = sessions
        .get(&parent_id)
        .map(|parent| Arc::clone(&parent.python_runtime))
        .unwrap_or_default();

    let ctx = SessionContext::new(
        child_id,
        SessionInit {
            system_prompt,
            skills: Vec::new(),
            working_directory: work_dir,
            plan_mode: false,
            prompt_behavior,
            initial_messages: Vec::new(),
            archive_root,
            max_context_window: inherited_max_context_window,
            prompt_memory_mode: PromptMemoryMode::Disabled,
            agent_key: None,
            team_key: None,
            memory_policy: MemoryPolicyConfig::default(),
            model_profile: inherited_model_profile,
            session_group: inherited_session_group.clone(),
            auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
            status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
        },
        &inherited_provider,
    )
    .await
    .map_err(|e| e.to_string())?;
    let mut ctx = ctx;
    ctx.python_runtime = inherited_python_runtime;
    ctx.python_group = effective_session_group(child_id, inherited_session_group.as_deref());
    ctx.persisted_config.session_group = inherited_session_group;
    ctx.permission_context.set_rules(permission_rules);

    debug_log_session(
        child_id,
        format!(
            "child session created with working_directory={}",
            work_dir_display
        ),
    );

    engine.session_tree.add_child(parent_id, child_id);
    sessions.insert(child_id, ctx);

    let _ = io
        .output
        .send(CoreOutput::ChildSpawned {
            parent_id,
            child_id,
        })
        .await;
    let _ = io
        .output
        .send(CoreOutput::SessionStateChanged {
            session_id: child_id,
            state: SessionState::Idle,
        })
        .await;
    emit_checkpoint_request(sessions, engine.session_tree, io.output).await;

    // Schedule the child's first task for the next core-loop iteration so
    // `spawn` can acknowledge immediately instead of blocking on child completion.
    io.deferred_inputs.push_back(CoreInput::UserMessage {
        session_id: child_id,
        content: task,
        turn_id: uuid::Uuid::new_v4().to_string(),
    });
    Ok(())
}

/// Execute a tool call directly within the core.
///
/// For interactive tools, sets up a channel and emits `InteractionNeeded`.
/// Returns the tool result as a `ToolOutcome`.
#[cfg_attr(not(test), allow(dead_code))]
async fn execute_tool_call(
    call: &PendingToolCall,
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) -> ToolOutcome {
    let (cancel_tx, cancellation) = CancellationChannel::new_pair();
    {
        let Some(session) = sessions.get_mut(&session_id) else {
            return ToolOutcome::Error {
                message: "session not found".into(),
            };
        };
        if session.interrupted {
            return ToolOutcome::Cancelled;
        }
        session.cancel_tx = Some(cancel_tx.clone());
    }
    debug_log_session(
        session_id,
        format!("starting tool execution for `{}`", call.tool_name),
    );

    let tool = {
        let Some(session) = sessions.get(&session_id) else {
            return ToolOutcome::Error {
                message: "session not found".into(),
            };
        };
        let Some(tool) = session.tool_registry.get(&call.tool_name) else {
            return ToolOutcome::Error {
                message: format!("unknown tool: {}", call.tool_name),
            };
        };
        Arc::clone(tool)
    };

    let permission_outcome = {
        let Some(session) = sessions.get_mut(&session_id) else {
            return ToolOutcome::Error {
                message: "session not found".into(),
            };
        };
        let (request, local) = build_permission_request(session, call, tool.as_ref());
        let outcome = evaluate_permission(&session.permission_context, request, local);
        session.last_permission_outcome = Some(outcome.clone());
        outcome
    };

    if !permission_outcome.is_allowed() {
        if permission_outcome.kind
            == crate::permission::outcome::PermissionOutcomeKind::RequiresApproval
        {
            let (pending, request) = build_permission_approval_request(&permission_outcome);
            if let Some(session) = sessions.get_mut(&session_id) {
                session.state = SessionState::Paused;
                session.pending_permission_approval = Some(pending);
            }
            let _ = io
                .output
                .send(CoreOutput::SessionStateChanged {
                    session_id,
                    state: SessionState::Paused,
                })
                .await;
            let _ = io
                .output
                .send(CoreOutput::InteractionNeeded {
                    session_id,
                    request,
                })
                .await;

            loop {
                match io.input.recv().await {
                    Some(CoreInput::InteractionResponse {
                        session_id: resp_sid,
                        response,
                    }) if resp_sid == session_id => {
                        let choice = parse_permission_approval_response(&response);
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.state = SessionState::Streaming;
                            session.pending_permission_approval = None;
                        }
                        let _ = io
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;
                        match choice {
                            Some(PermissionApprovalChoice::ApproveOnce) => break,
                            Some(PermissionApprovalChoice::DenyOnce) | None => {
                                if let Some(session) = sessions.get_mut(&session_id) {
                                    session.cancel_tx = None;
                                }
                                return ToolOutcome::Error {
                                    message: ToolError::PermissionDenied {
                                        reason: permission_outcome.reason,
                                    }
                                    .to_string(),
                                };
                            }
                        }
                    }
                    Some(CoreInput::Cancel {
                        session_id: cancel_sid,
                    }) if cancel_sid == session_id => {
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.state = SessionState::Streaming;
                            session.pending_permission_approval = None;
                        }
                        let _ = io
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;
                        interrupt_session(sessions, io.deferred_inputs, session_id);
                        return ToolOutcome::Cancelled;
                    }
                    Some(CoreInput::Signal {
                        session_id: signal_sid,
                        signal,
                    }) if signal_sid == session_id
                        && matches!(
                            signal,
                            SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill
                        ) =>
                    {
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.state = SessionState::Streaming;
                            session.pending_permission_approval = None;
                        }
                        let _ = io
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;
                        interrupt_session(sessions, io.deferred_inputs, session_id);
                        return ToolOutcome::Cancelled;
                    }
                    Some(CoreInput::RequestCheckpoint { reply }) => {
                        let checkpoint = snapshot_sessions(sessions, engine.session_tree).await;
                        let _ = reply.send(checkpoint);
                    }
                    Some(CoreInput::RequestSessionCheckpoint {
                        session_id: requested_session_id,
                        reply,
                    }) => {
                        let persisted_session = match sessions.get(&requested_session_id) {
                            Some(session) => session.snapshot(requested_session_id).await,
                            None => None,
                        };
                        let checkpoint = CoreCheckpoint::new(
                            persisted_session.into_iter().collect(),
                            engine.session_tree.snapshot(),
                        );
                        let _ = reply.send(checkpoint);
                    }
                    Some(other) => io.deferred_inputs.push_back(other),
                    None => {
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.state = SessionState::Streaming;
                            session.pending_permission_approval = None;
                            session.cancel_tx = None;
                        }
                        let _ = io
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;
                        return ToolOutcome::Error {
                            message: ToolError::PermissionDenied {
                                reason: format!(
                                    "{}; approval response channel closed",
                                    permission_outcome.reason
                                ),
                            }
                            .to_string(),
                        };
                    }
                }
            }
        } else {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cancel_tx = None;
            }
            return ToolOutcome::Error {
                message: ToolError::PermissionDenied {
                    reason: permission_outcome.reason,
                }
                .to_string(),
            };
        }
    }

    if call.tool_name == "spawn" {
        let task = match call.arguments.get("task").and_then(|v| v.as_str()) {
            Some(task) => task.to_string(),
            None => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Error {
                    message: "invalid arguments: missing required parameter: task".into(),
                };
            }
        };
        let system_prompt = call
            .arguments
            .get("system_prompt")
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned);
        let (archive_root, max_context_window, prompt_behavior, permission_rules) =
            match sessions.get(&session_id) {
                Some(session) => (
                    session.archive_root.clone(),
                    session.max_context_window,
                    session.permission_context.prompt_behavior(),
                    session.permission_context.rules().persisted_only(),
                ),
                None => {
                    return ToolOutcome::Error {
                        message: "session not found".into(),
                    };
                }
            };
        let child_id = SessionId::new();
        match Box::pin(start_child_session(
            sessions,
            io,
            engine,
            session_id,
            child_id,
            task,
            system_prompt,
            prompt_behavior,
            permission_rules,
            archive_root,
            max_context_window,
        ))
        .await
        {
            Ok(()) => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Success {
                    output: format!("{child_id:?}"),
                };
            }
            Err(error) => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Error {
                    message: format!("failed to spawn: {error}"),
                };
            }
        }
    }

    if call.tool_name == "wait_child" {
        let child_id_str = match call.arguments.get("child_id").and_then(|v| v.as_str()) {
            Some(child_id) => child_id,
            None => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Error {
                    message: "invalid arguments: missing required parameter: child_id".into(),
                };
            }
        };
        let non_blocking = call
            .arguments
            .get("non_blocking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout = call
            .arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis);
        let child_id = match crate::tool::wait_child::parse_session_id(child_id_str) {
            Some(child_id) => child_id,
            None => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Error {
                    message: format!("invalid arguments: invalid child_id: {child_id_str}"),
                };
            }
        };

        if engine.session_tree.parent_of(child_id) != Some(session_id) {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cancel_tx = None;
            }
            return ToolOutcome::Success {
                output: "null".into(),
            };
        }

        if let Some(status) = engine.session_tree.exit_status(child_id).cloned() {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cancel_tx = None;
            }
            return ToolOutcome::Success {
                output: serde_json::to_string(&status).unwrap_or_else(|_| "unknown".into()),
            };
        }

        if non_blocking {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cancel_tx = None;
            }
            return ToolOutcome::Success {
                output: "null".into(),
            };
        }

        if waiting_would_cycle_including_session_tree(
            sessions,
            engine.session_tree,
            session_id,
            child_id,
        ) {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cancel_tx = None;
            }
            return ToolOutcome::Error {
                message: format!(
                    "deadlock detected: waiting for child {child_id:?} would create a wait cycle"
                ),
            };
        }

        if let Some(session) = sessions.get_mut(&session_id) {
            session.cancel_tx = None;
            session.state = SessionState::Waiting;
            session.suspended_wait = Some(SuspendedWait::ChildExit {
                tool_use_id: call.tool_use_id.clone(),
                child_id,
                timeout_at: timeout.map(|value| Instant::now() + value),
            });
            let _ = engine
                .session_tree
                .register_active_wait(session_id, child_id);
        }
        emit_session_waiting(sessions, session_id, io.output, engine.session_tree).await;
        return ToolOutcome::Cancelled;
    }

    if call.tool_name == "recv_message" {
        let source_str = match call.arguments.get("source").and_then(|v| v.as_str()) {
            Some(source) => source,
            None => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return ToolOutcome::Error {
                    message: "invalid arguments: missing required parameter: source".into(),
                };
            }
        };
        let non_blocking = call
            .arguments
            .get("non_blocking")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let timeout = call
            .arguments
            .get("timeout_ms")
            .and_then(|v| v.as_u64())
            .map(Duration::from_millis);
        let source = if source_str == "any" {
            MessageSource::Any
        } else {
            match crate::tool::wait_child::parse_session_id(source_str) {
                Some(source_id) => MessageSource::Session(source_id),
                None => {
                    if let Some(session) = sessions.get_mut(&session_id) {
                        session.cancel_tx = None;
                    }
                    return ToolOutcome::Error {
                        message: format!(
                            "invalid arguments: invalid source session_id: {source_str}"
                        ),
                    };
                }
            }
        };

        let message = if let Some(session) = sessions.get_mut(&session_id) {
            pop_mailbox_message(&mut session.mailbox, &source)
        } else {
            None
        };

        if let Some(session) = sessions.get_mut(&session_id) {
            session.cancel_tx = None;
        }

        if let Some(message) = message {
            return ToolOutcome::Success {
                output: serde_json::json!({
                    "from": message.from,
                    "content": message.content,
                })
                .to_string(),
            };
        }

        if non_blocking {
            return ToolOutcome::Success {
                output: "null".into(),
            };
        }

        if let MessageSource::Session(source_session) = source {
            if waiting_would_cycle(sessions, session_id, source_session) {
                return ToolOutcome::Error {
                    message: format!(
                        "deadlock detected: waiting for session {source_session:?} would create a wait cycle"
                    ),
                };
            }
        }

        if let Some(session) = sessions.get_mut(&session_id) {
            session.state = SessionState::Waiting;
            session.suspended_wait = Some(SuspendedWait::Mailbox {
                tool_use_id: call.tool_use_id.clone(),
                source,
                timeout_at: timeout.map(|value| Instant::now() + value),
            });
        }
        emit_session_waiting(sessions, session_id, io.output, engine.session_tree).await;
        return ToolOutcome::Cancelled;
    }

    let (
        tool,
        filesystem,
        working_directory,
        plan_store,
        cancellation,
        python_group,
        python_runtime,
    ) = {
        let Some(session) = sessions.get(&session_id) else {
            return ToolOutcome::Error {
                message: "session not found".into(),
            };
        };
        let Some(tool) = session.tool_registry.get(&call.tool_name) else {
            return ToolOutcome::Error {
                message: format!("unknown tool: {}", call.tool_name),
            };
        };
        (
            Arc::clone(tool),
            Arc::clone(&session.filesystem),
            session.working_directory.clone(),
            session.plan_store.clone(),
            cancellation.clone(),
            session.python_group.clone(),
            Arc::clone(&session.python_runtime),
        )
    };

    if tool.is_interactive() {
        // Create an interaction channel for this tool.
        // The tool is spawned in a separate task so we can simultaneously
        // poll for interaction requests and tool completion.
        let (req_tx, mut req_rx) =
            mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

        let channel = InteractionChannel { request_tx: req_tx };

        let ctx = ExecutionContext {
            session_id,
            filesystem,
            working_directory,
            interaction_channel: Some(channel),
            plan_store,
            session_group: python_group.clone(),
            python_runtime: Arc::clone(&python_runtime),
            core_input: Some(io.input_tx.clone()),
            cancellation: cancellation.clone(),
        };

        let args = call.arguments.clone();
        let mut tool_handle = tokio::spawn(async move { tool.execute(args, &ctx).await });

        let output_clone = io.output.clone();
        let sid = session_id;

        let outcome = 'tool_loop: loop {
            tokio::select! {
                result = &mut tool_handle => {
                    break 'tool_loop match result {
                        Ok(Ok(tool_output)) => {
                            debug_log_session(
                                session_id,
                                format!(
                                    "interactive tool `{}` completed (error={})",
                                    call.tool_name, tool_output.is_error
                                ),
                            );
                            if tool_output.is_error {
                                ToolOutcome::Error {
                                    message: tool_output.content,
                                }
                            } else {
                                ToolOutcome::Success {
                                    output: tool_output.content,
                                }
                            }
                        }
                        Ok(Err(ToolError::Cancelled)) => {
                            debug_log_session(
                                session_id,
                                format!("interactive tool `{}` cancelled", call.tool_name),
                            );
                            ToolOutcome::Cancelled
                        }
                        Ok(Err(tool_err)) => {
                            debug_log_session(
                                session_id,
                                format!("interactive tool `{}` errored: {}", call.tool_name, tool_err),
                            );
                            ToolOutcome::Error {
                                message: tool_err.to_string(),
                            }
                        }
                        Err(join_err) => {
                            debug_log_session(
                                session_id,
                                format!("interactive tool `{}` panicked: {join_err}", call.tool_name),
                            );
                            ToolOutcome::Error {
                                message: format!("tool task panicked: {join_err}"),
                            }
                        }
                    };
                }
                interaction = req_rx.recv() => {
                    if let Some((request, reply_tx)) = interaction {
                        debug_log_session(
                            session_id,
                            format!(
                                "interactive tool `{}` requested interaction: kind={:?} prompt={}",
                                call.tool_name, request.kind, request.prompt
                            ),
                        );
                        let _ = output_clone.send(CoreOutput::InteractionNeeded {
                            session_id: sid,
                            request,
                        }).await;

                        loop {
                            match io.input.recv().await {
                                Some(CoreInput::InteractionResponse { session_id: resp_sid, response }) if resp_sid == sid => {
                                    debug_log_session(
                                        session_id,
                                        format!("interactive tool `{}` received interaction response", call.tool_name),
                                    );
                                    let _ = reply_tx.send(response);
                                    break;
                                }
                                Some(CoreInput::Cancel { session_id: cancel_sid }) if cancel_sid == sid => {
                                    debug_log_session(
                                        session_id,
                                        format!("interactive tool `{}` cancelled while awaiting interaction", call.tool_name),
                                    );
                                    interrupt_session(sessions, io.deferred_inputs, session_id);
                                    drop(reply_tx);
                                    break 'tool_loop ToolOutcome::Cancelled;
                                }
                                Some(CoreInput::Signal { session_id: signal_sid, signal })
                                    if signal_sid == sid
                                        && matches!(signal, SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill) =>
                                {
                                    debug_log_session(
                                        session_id,
                                        format!("interactive tool `{}` interrupted while awaiting interaction", call.tool_name),
                                    );
                                    interrupt_session(sessions, io.deferred_inputs, session_id);
                                    drop(reply_tx);
                                    break 'tool_loop ToolOutcome::Cancelled;
                                }
                                Some(other) => {
                                    io.deferred_inputs.push_back(other);
                                    continue;
                                }
                                None => {
                                    debug_log_session(
                                        session_id,
                                        format!("interactive tool `{}` failed: input channel closed", call.tool_name),
                                    );
                                    break 'tool_loop ToolOutcome::Error {
                                        message: "input channel closed".into(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
        };

        if let Some(session) = sessions.get_mut(&session_id) {
            if let Some(cancel_tx) = session.cancel_tx.as_ref() {
                let _ = cancel_tx.send(true);
            }
            session.cancel_tx = None;
        }
        outcome
    } else {
        // Non-interactive tool: execute directly
        let ctx = ExecutionContext {
            session_id,
            filesystem,
            working_directory,
            interaction_channel: None,
            plan_store,
            session_group: python_group,
            python_runtime,
            core_input: Some(io.input_tx.clone()),
            cancellation: cancellation.clone(),
        };

        let tool_future = tool.execute(call.arguments.clone(), &ctx);
        tokio::pin!(tool_future);
        let result = loop {
            tokio::select! {
                result = &mut tool_future => {
                    break match result {
                        Ok(tool_output) => {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` completed (error={})", call.tool_name, tool_output.is_error),
                            );
                            if tool_output.is_error {
                                ToolOutcome::Error {
                                    message: tool_output.content,
                                }
                            } else {
                                ToolOutcome::Success {
                                    output: tool_output.content,
                                }
                            }
                        }
                        Err(ToolError::Cancelled) => {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` cancelled", call.tool_name),
                            );
                            ToolOutcome::Cancelled
                        }
                        Err(tool_err) => {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` errored: {}", call.tool_name, tool_err),
                            );
                            ToolOutcome::Error {
                                message: tool_err.to_string(),
                            }
                        }
                    };
                }
                maybe_input = io.input.recv() => {
                    match maybe_input {
                        Some(CoreInput::Cancel { session_id: cancel_sid }) if cancel_sid == session_id => {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` cancelled by core input", call.tool_name),
                            );
                            interrupt_session(sessions, io.deferred_inputs, session_id);
                            break ToolOutcome::Cancelled;
                        }
                        Some(CoreInput::Signal { session_id: signal_sid, signal })
                            if signal_sid == session_id
                                && matches!(signal, SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill) =>
                        {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` interrupted by session signal", call.tool_name),
                            );
                            interrupt_session(sessions, io.deferred_inputs, session_id);
                            break ToolOutcome::Cancelled;
                        }
                        Some(other) => {
                            io.deferred_inputs.push_back(other);
                            continue;
                        }
                        None => {
                            debug_log_session(
                                session_id,
                                format!("tool `{}` failed: input channel closed during execution", call.tool_name),
                            );
                            if let Some(session) = sessions.get_mut(&session_id) {
                                if let Some(cancel_tx) = session.cancel_tx.as_ref() {
                                    let _ = cancel_tx.send(true);
                                }
                            }
                            break ToolOutcome::Error { message: "input channel closed".into() };
                        }
                    }
                }
            }
        };

        if let Some(session) = sessions.get_mut(&session_id) {
            if let Some(cancel_tx) = session.cancel_tx.as_ref() {
                let _ = cancel_tx.send(true);
            }
            session.cancel_tx = None;
        }
        result
    }
}

/// Handle plan progress after an `update_plan` tool call.
///
/// Emits `PlanProgress` events and injects a prompt for newly ready actions.
#[cfg_attr(not(test), allow(dead_code))]
async fn handle_plan_progress(
    session: &mut SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    plan_id_str: &str,
    action_id_str: &str,
) {
    let store = session.plan_store.lock().await;

    let plan_id: Result<crate::planner::PlanId, _> = plan_id_str.parse();
    let plan_id = match plan_id {
        Ok(id) => id,
        Err(_) => return,
    };

    let plan = match store.get(&plan_id) {
        Some(p) => p,
        None => return,
    };

    // Get the status of the updated action for the progress event
    let action_id = crate::planner::ActionId::new(action_id_str);
    let status_label = plan
        .get_action(&action_id)
        .map(|a| a.status.label().to_string())
        .unwrap_or_default();

    let remaining = plan.remaining_count();
    let total = plan.actions.len();

    // Emit PlanProgress
    let _ = output
        .send(CoreOutput::PlanProgress {
            session_id,
            plan_id: plan_id_str.to_string(),
            action_id: action_id_str.to_string(),
            status: status_label,
            remaining,
            total,
        })
        .await;

    if plan.is_complete() {
        // Emit a summary via a user-role message
        let summary = render_plan(plan);
        drop(store);
        session.history.push(Message::user(format!(
            "All actions in the plan are complete. Here is the final summary:\n\n{summary}"
        )));
    } else {
        // Check for newly ready actions
        let ready = get_ready_actions(plan);
        if !ready.is_empty() {
            let mut prompt_parts = Vec::new();
            for action in &ready {
                prompt_parts.push(format!(
                    "[{}] {} — {}",
                    action.action_id, action.title, action.description
                ));
            }
            drop(store);
            let prompt = format!(
                "Execute the next action from your plan: {}. \
                 When done, use the `plan` tool with operation `update_plan` \
                 to mark it as completed. Reuse this exact plan_id: {}",
                prompt_parts.join("; "),
                plan_id_str,
            );
            session.history.push(Message::user(prompt));
        }
    }
}

/// Handle the result of an LLM turn: either complete with text or process tool calls.
///
/// This function handles the tool execution loop: when the LLM requests tools,
/// it executes them and calls the LLM again until the LLM produces text.
#[cfg_attr(not(test), allow(dead_code))]
async fn handle_llm_turn(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) -> TurnOutcome {
    let turn_start = std::time::Instant::now();
    let mut accumulated_usage: Option<quine_llm::TokenUsage> = None;
    let mut accumulated_cache_usage: Option<PromptCacheUsage> = None;
    debug_log_session(
        session_id,
        format!(
            "starting LLM turn with {} history messages",
            sessions
                .get(&session_id)
                .map(|session| session.history.len())
                .unwrap_or(0)
        ),
    );

    loop {
        let Some(session) = sessions.get_mut(&session_id) else {
            return TurnOutcome::Failed("session not found".into());
        };
        if let Err(error) = archive_old_tool_results_in_history(session, session_id).await {
            session.state = SessionState::Idle;
            let duration_us = turn_start.elapsed().as_micros() as u64;
            let _ = io
                .output
                .send(CoreOutput::SessionStateChanged {
                    session_id,
                    state: SessionState::Idle,
                })
                .await;
            emit_failed_turn(
                io.output,
                session_id,
                error,
                duration_us,
                accumulated_usage.clone(),
                accumulated_cache_usage.clone(),
            )
            .await;
            return TurnOutcome::Failed("session error".into());
        }

        let Some(should_auto_compact) = sessions.get(&session_id).map(|session| {
            compaction::should_auto_compact(
                session.max_context_window,
                session.last_input_tokens,
                session.auto_compact_threshold_percent,
            )
        }) else {
            return TurnOutcome::Failed("session not found".into());
        };
        if should_auto_compact {
            let Some(session) = sessions.get_mut(&session_id) else {
                return TurnOutcome::Failed("session not found".into());
            };
            let provider = Arc::clone(&session.provider);
            if let Err(error) =
                compact_session_history(&*provider, session, session_id, CompactionTrigger::Auto)
                    .await
            {
                session.state = SessionState::Idle;
                let duration_us = turn_start.elapsed().as_micros() as u64;
                let _ = io
                    .output
                    .send(CoreOutput::SessionStateChanged {
                        session_id,
                        state: SessionState::Idle,
                    })
                    .await;
                emit_failed_turn(
                    io.output,
                    session_id,
                    error,
                    duration_us,
                    accumulated_usage.clone(),
                    accumulated_cache_usage.clone(),
                )
                .await;
                return TurnOutcome::Failed("session error".into());
            }
        }

        let Some(session) = sessions.get(&session_id) else {
            return TurnOutcome::Failed("session not found".into());
        };
        if session.interrupted {
            debug_log_session(
                session_id,
                "aborting LLM turn because session is interrupted",
            );
            let duration_us = turn_start.elapsed().as_micros() as u64;
            let _ = io
                .output
                .send(CoreOutput::TurnComplete {
                    session_id,
                    duration_us,
                    status: TurnStatus::Cancelled,
                    usage: accumulated_usage.clone(),
                    cache_usage: accumulated_cache_usage.clone(),
                })
                .await;
            return TurnOutcome::Cancelled;
        }
        let history = match sessions.get_mut(&session_id) {
            Some(session) => match build_provider_messages(session).await {
                Ok(history) => history,
                Err(error) => {
                    session.state = SessionState::Idle;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    let _ = io
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Idle,
                        })
                        .await;
                    emit_failed_turn(
                        io.output,
                        session_id,
                        error,
                        duration_us,
                        accumulated_usage.clone(),
                        accumulated_cache_usage.clone(),
                    )
                    .await;
                    return TurnOutcome::Failed("session error".into());
                }
            },
            None => return TurnOutcome::Failed("session not found".into()),
        };
        let tools = match sessions.get(&session_id) {
            Some(session) => session.tools.clone(),
            None => return TurnOutcome::Failed("session not found".into()),
        };
        let prompt_cache_tokens = canonicalize_prompt_cache_tokens(&history, &tools);
        let cache_usage = sessions
            .get(&session_id)
            .map(|session| {
                estimate_prompt_cache_usage(
                    session.last_prompt_cache_tokens.as_deref(),
                    &prompt_cache_tokens,
                )
            })
            .unwrap_or_default();
        let provider = sessions
            .get(&session_id)
            .map(|session| Arc::clone(&session.provider))
            .ok_or_else(|| TurnOutcome::Failed("session not found".into()));
        let provider = match provider {
            Ok(provider) => provider,
            Err(outcome) => return outcome,
        };
        match call_llm_interruptible(
            &*provider,
            history,
            tools,
            session_id,
            io,
            sessions,
            engine.session_tree,
        )
        .await
        {
            Ok(None) => {
                debug_log_session(session_id, "LLM turn interrupted");
                set_session_status_report(sessions, session_id, io.output, None).await;
                let duration_us = turn_start.elapsed().as_micros() as u64;
                let _ = io
                    .output
                    .send(CoreOutput::SessionStateChanged {
                        session_id,
                        state: SessionState::Idle,
                    })
                    .await;
                let _ = io
                    .output
                    .send(CoreOutput::TurnComplete {
                        session_id,
                        duration_us,
                        status: TurnStatus::Cancelled,
                        usage: accumulated_usage.clone(),
                        cache_usage: accumulated_cache_usage.clone(),
                    })
                    .await;
                return TurnOutcome::Cancelled;
            }
            Ok(Some(LlmCallResult {
                turn: LlmTurnResult::Text(full_text),
                usage,
                ..
            })) => {
                let acc = accumulated_cache_usage.get_or_insert_with(PromptCacheUsage::default);
                acc.estimated_hit_tokens += cache_usage.estimated_hit_tokens;
                acc.estimated_miss_tokens += cache_usage.estimated_miss_tokens;

                // Accumulate usage from this LLM call.
                if let Some(u) = usage {
                    let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                    acc.input_tokens += u.input_tokens;
                    acc.output_tokens += u.output_tokens;
                    if let Some(session) = sessions.get_mut(&session_id) {
                        session.last_input_tokens = Some(u.input_tokens);
                    }
                } else if let Some(session) = sessions.get_mut(&session_id) {
                    session.last_input_tokens = None;
                }
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.last_prompt_cache_tokens = Some(prompt_cache_tokens);
                }

                if let Some(session) = sessions.get_mut(&session_id) {
                    session.history.push(Message::assistant(&full_text));
                }
                debug_log_session(
                    session_id,
                    format!(
                        "LLM turn completed with text output ({} chars)",
                        full_text.len()
                    ),
                );
                let status_report_update = if let Some(session) = sessions.get(&session_id) {
                    let threshold = session
                        .persisted_config
                        .status_report_min_tool_rounds
                        .max(1);
                    if session.current_turn_tool_rounds >= threshold {
                        Some(
                            generate_status_report_with_model(
                                session,
                                session_id,
                                0,
                                StatusReportStage::Completed,
                            )
                            .await,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(report) = status_report_update {
                    set_session_status_report(sessions, session_id, io.output, Some(report)).await;
                }

                let _ = io
                    .output
                    .send(CoreOutput::TextComplete {
                        session_id,
                        full_text: full_text.clone(),
                    })
                    .await;

                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
                }
                let duration_us = turn_start.elapsed().as_micros() as u64;
                let _ = io
                    .output
                    .send(CoreOutput::TurnComplete {
                        session_id,
                        duration_us,
                        status: TurnStatus::Success,
                        usage: accumulated_usage,
                        cache_usage: accumulated_cache_usage,
                    })
                    .await;
                let _ = io
                    .output
                    .send(CoreOutput::SessionStateChanged {
                        session_id,
                        state: SessionState::Idle,
                    })
                    .await;
                if let Some(session) = sessions.get_mut(&session_id) {
                    schedule_session_memory_refresh(
                        session,
                        session_id,
                        Arc::clone(&session.provider),
                        io.input_tx.clone(),
                    );
                }
                emit_checkpoint_request(sessions, engine.session_tree, io.output).await;
                return TurnOutcome::Completed(Some(full_text));
            }
            Ok(Some(LlmCallResult {
                turn:
                    LlmTurnResult::ToolCalls {
                        text_before,
                        mut calls,
                    },
                usage,
                ..
            })) => {
                let acc = accumulated_cache_usage.get_or_insert_with(PromptCacheUsage::default);
                acc.estimated_hit_tokens += cache_usage.estimated_hit_tokens;
                acc.estimated_miss_tokens += cache_usage.estimated_miss_tokens;

                // Accumulate usage from this LLM call.
                if let Some(u) = usage {
                    let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                    acc.input_tokens += u.input_tokens;
                    acc.output_tokens += u.output_tokens;
                    if let Some(session) = sessions.get_mut(&session_id) {
                        session.last_input_tokens = Some(u.input_tokens);
                    }
                } else if let Some(session) = sessions.get_mut(&session_id) {
                    session.last_input_tokens = None;
                }
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.last_prompt_cache_tokens = Some(prompt_cache_tokens);
                }

                // Flush any text that preceded the tool calls to the TUI.
                if let Some(ref text) = text_before {
                    debug_log_session(
                        session_id,
                        format!("LLM emitted pre-tool text ({} chars)", text.len()),
                    );
                    let _ = io
                        .output
                        .send(CoreOutput::TextComplete {
                            session_id,
                            full_text: text.clone(),
                        })
                        .await;
                }

                // Record the assistant's tool_use message in history.
                let tool_use_requests: Vec<quine_llm::ToolUseRequest> = calls
                    .iter()
                    .map(|c| quine_llm::ToolUseRequest {
                        tool_use_id: c.tool_use_id.clone(),
                        tool_name: c.tool_name.clone(),
                        arguments: c.arguments.clone(),
                    })
                    .collect();
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.history.push(Message::assistant_tool_use(
                        text_before.clone(),
                        tool_use_requests,
                    ));
                    session.current_turn_tool_rounds =
                        session.current_turn_tool_rounds.saturating_add(1);
                }

                if let Some(session) = sessions.get(&session_id) {
                    calls = calls
                        .iter()
                        .map(|call| normalize_plan_tool_arguments(session, call))
                        .collect();
                }

                let debug = debug_enabled();
                debug_log_session(
                    session_id,
                    format!("LLM requested {} tool call(s)", calls.len()),
                );

                let concurrent_batch = sessions
                    .get(&session_id)
                    .filter(|session| tool_batch_is_concurrency_eligible(session, &calls))
                    .and_then(|session| prepare_concurrent_tool_calls(session, &calls));

                let concurrent_mode = concurrent_batch.is_some();
                let completed_calls = if concurrent_mode {
                    let mut completed_calls = Vec::with_capacity(calls.len());
                    let call_partitions = {
                        let session = sessions
                            .get(&session_id)
                            .expect("session must exist while partitioning tool calls");
                        partition_tool_calls_by_concurrency(session, &calls)
                    };

                    for (is_concurrent, partition) in call_partitions {
                        if is_concurrent && partition.len() > 1 {
                            for call in partition {
                                if debug {
                                    debug_log_session(
                                        session_id,
                                        format!(
                                            "calling tool `{}` concurrently (id={}) args={}",
                                            call.tool_name,
                                            call.tool_use_id,
                                            serde_json::to_string(&call.arguments)
                                                .unwrap_or_default()
                                        ),
                                    );
                                }
                                let _ = io
                                    .output
                                    .send(CoreOutput::ToolRequest {
                                        session_id,
                                        tool_use_id: call.tool_use_id.clone(),
                                        tool_name: call.tool_name.clone(),
                                        arguments: call.arguments.clone(),
                                    })
                                    .await;
                            }

                            let prepared_calls = {
                                let session = sessions
                                    .get(&session_id)
                                    .expect("session must exist while preparing tool calls");
                                prepare_concurrent_tool_calls(session, partition)
                                    .unwrap_or_default()
                            };
                            completed_calls.extend(
                                execute_concurrent_tool_batch(
                                    prepared_calls,
                                    sessions,
                                    session_id,
                                    io,
                                )
                                .await,
                            );
                            continue;
                        }

                        for call in partition {
                            if sessions
                                .get(&session_id)
                                .is_some_and(|session| session.interrupted)
                            {
                                debug_log_session(
                                    session_id,
                                    "skipping pending tool calls because session is interrupted",
                                );
                                let duration_us = turn_start.elapsed().as_micros() as u64;
                                let _ = io
                                    .output
                                    .send(CoreOutput::SessionStateChanged {
                                        session_id,
                                        state: SessionState::Idle,
                                    })
                                    .await;
                                let _ = io
                                    .output
                                    .send(CoreOutput::TurnComplete {
                                        session_id,
                                        duration_us,
                                        status: TurnStatus::Cancelled,
                                        usage: accumulated_usage.clone(),
                                        cache_usage: accumulated_cache_usage.clone(),
                                    })
                                    .await;
                                return TurnOutcome::Cancelled;
                            }
                            if debug {
                                debug_log_session(
                                    session_id,
                                    format!(
                                        "calling tool `{}` (id={}) args={}",
                                        call.tool_name,
                                        call.tool_use_id,
                                        serde_json::to_string(&call.arguments).unwrap_or_default()
                                    ),
                                );
                            }
                            let _ = io
                                .output
                                .send(CoreOutput::ToolRequest {
                                    session_id,
                                    tool_use_id: call.tool_use_id.clone(),
                                    tool_name: call.tool_name.clone(),
                                    arguments: call.arguments.clone(),
                                })
                                .await;

                            let tool_start = std::time::Instant::now();
                            let result =
                                execute_tool_call(call, sessions, session_id, io, engine).await;
                            completed_calls.push(CompletedConcurrentToolCall {
                                index: completed_calls.len(),
                                tool_use_id: call.tool_use_id.clone(),
                                tool_name: call.tool_name.clone(),
                                arguments: call.arguments.clone(),
                                result,
                                duration_us: tool_start.elapsed().as_micros() as u64,
                            });
                        }
                    }

                    completed_calls
                } else {
                    let mut completed_calls = Vec::with_capacity(calls.len());
                    for call in &calls {
                        if sessions
                            .get(&session_id)
                            .is_some_and(|session| session.interrupted)
                        {
                            debug_log_session(
                                session_id,
                                "skipping pending tool calls because session is interrupted",
                            );
                            let duration_us = turn_start.elapsed().as_micros() as u64;
                            let _ = io
                                .output
                                .send(CoreOutput::SessionStateChanged {
                                    session_id,
                                    state: SessionState::Idle,
                                })
                                .await;
                            let _ = io
                                .output
                                .send(CoreOutput::TurnComplete {
                                    session_id,
                                    duration_us,
                                    status: TurnStatus::Cancelled,
                                    usage: accumulated_usage.clone(),
                                    cache_usage: accumulated_cache_usage.clone(),
                                })
                                .await;
                            return TurnOutcome::Cancelled;
                        }
                        if debug {
                            debug_log_session(
                                session_id,
                                format!(
                                    "calling tool `{}` (id={}) args={}",
                                    call.tool_name,
                                    call.tool_use_id,
                                    serde_json::to_string(&call.arguments).unwrap_or_default()
                                ),
                            );
                        }
                        let _ = io
                            .output
                            .send(CoreOutput::ToolRequest {
                                session_id,
                                tool_use_id: call.tool_use_id.clone(),
                                tool_name: call.tool_name.clone(),
                                arguments: call.arguments.clone(),
                            })
                            .await;

                        let tool_start = std::time::Instant::now();
                        let result =
                            execute_tool_call(call, sessions, session_id, io, engine).await;
                        completed_calls.push(CompletedConcurrentToolCall {
                            index: completed_calls.len(),
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            arguments: call.arguments.clone(),
                            result,
                            duration_us: tool_start.elapsed().as_micros() as u64,
                        });
                    }
                    completed_calls
                };

                let mut saw_cancelled_tool = false;
                for completed_call in &completed_calls {
                    let (tool_output, is_error) = match &completed_call.result {
                        ToolOutcome::Success { output } => (output.clone(), false),
                        ToolOutcome::Error { message } => (message.clone(), true),
                        ToolOutcome::Cancelled => {
                            ("Tool execution was cancelled".to_string(), true)
                        }
                    };

                    let _ = io
                        .output
                        .send(CoreOutput::ToolResult {
                            session_id,
                            tool_use_id: completed_call.tool_use_id.clone(),
                            tool_name: completed_call.tool_name.clone(),
                            content: tool_output.clone(),
                            is_error,
                            duration_us: completed_call.duration_us,
                        })
                        .await;

                    if debug {
                        let status = if is_error { "ERROR" } else { "OK" };
                        let preview = if tool_output.len() > 200 {
                            format!("{}...[{} bytes]", &tool_output[..200], tool_output.len())
                        } else {
                            tool_output.clone()
                        };
                        debug_log_session(
                            session_id,
                            format!(
                                "tool `{}` result ({status}): {}",
                                completed_call.tool_name, preview
                            ),
                        );
                    }

                    if let Some(session) = sessions.get_mut(&session_id) {
                        let history_output = match prepare_tool_result_for_history(
                            session,
                            session_id,
                            &completed_call.tool_use_id,
                            &completed_call.tool_name,
                            &tool_output,
                            is_error,
                        )
                        .await
                        {
                            Ok(output) => output,
                            Err(error) => {
                                session.cancel_tx = None;
                                session.state = SessionState::Idle;
                                let duration_us = turn_start.elapsed().as_micros() as u64;
                                let _ = io
                                    .output
                                    .send(CoreOutput::SessionStateChanged {
                                        session_id,
                                        state: SessionState::Idle,
                                    })
                                    .await;
                                emit_failed_turn(
                                    io.output,
                                    session_id,
                                    error.clone(),
                                    duration_us,
                                    accumulated_usage.clone(),
                                    accumulated_cache_usage.clone(),
                                )
                                .await;
                                return TurnOutcome::Failed(error.to_string());
                            }
                        };
                        session.history.push(Message::tool_result(
                            &completed_call.tool_use_id,
                            &history_output,
                            is_error,
                        ));
                    }

                    let should_resume = matches!(completed_call.result, ToolOutcome::Cancelled)
                        && sessions
                            .get(&session_id)
                            .and_then(|session| session.suspended_wait.as_ref())
                            .is_some();

                    if matches!(completed_call.result, ToolOutcome::Cancelled) {
                        saw_cancelled_tool = true;
                    }

                    if should_resume {
                        return TurnOutcome::Suspended;
                    }

                    if !concurrent_mode && matches!(completed_call.result, ToolOutcome::Cancelled) {
                        debug_log_session(
                            session_id,
                            "LLM turn aborted because tool execution was cancelled",
                        );
                        set_session_status_report(sessions, session_id, io.output, None).await;
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.cancel_tx = None;
                            session.state = SessionState::Idle;
                        }
                        let duration_us = turn_start.elapsed().as_micros() as u64;
                        let _ = io
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Idle,
                            })
                            .await;
                        let _ = io
                            .output
                            .send(CoreOutput::TurnComplete {
                                session_id,
                                duration_us,
                                status: TurnStatus::Cancelled,
                                usage: accumulated_usage.clone(),
                                cache_usage: accumulated_cache_usage.clone(),
                            })
                            .await;
                        emit_checkpoint_request(sessions, engine.session_tree, io.output).await;
                        return TurnOutcome::Cancelled;
                    }

                    if completed_call.tool_name == "plan" {
                        let is_update = completed_call
                            .arguments
                            .get("operation")
                            .and_then(|v| v.as_str())
                            == Some("update_plan");

                        if is_update {
                            if let Some(plan_id_str) = completed_call
                                .arguments
                                .get("plan_id")
                                .and_then(|v| v.as_str())
                            {
                                if let Some(action_id_str) = completed_call
                                    .arguments
                                    .get("action_id")
                                    .and_then(|v| v.as_str())
                                {
                                    handle_plan_progress(
                                        sessions
                                            .get_mut(&session_id)
                                            .expect("session should exist"),
                                        session_id,
                                        io.output,
                                        plan_id_str,
                                        action_id_str,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }

                if concurrent_mode && saw_cancelled_tool {
                    debug_log_session(
                        session_id,
                        "LLM turn aborted because concurrent tool execution was cancelled",
                    );
                    set_session_status_report(sessions, session_id, io.output, None).await;
                    if let Some(session) = sessions.get_mut(&session_id) {
                        session.cancel_tx = None;
                        session.state = SessionState::Idle;
                    }
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    let _ = io
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Idle,
                        })
                        .await;
                    let _ = io
                        .output
                        .send(CoreOutput::TurnComplete {
                            session_id,
                            duration_us,
                            status: TurnStatus::Cancelled,
                            usage: accumulated_usage.clone(),
                            cache_usage: accumulated_cache_usage.clone(),
                        })
                        .await;
                    if let Some(session) = sessions.get_mut(&session_id) {
                        schedule_session_memory_refresh(
                            session,
                            session_id,
                            Arc::clone(&session.provider),
                            io.input_tx.clone(),
                        );
                    }
                    emit_checkpoint_request(sessions, engine.session_tree, io.output).await;
                    return TurnOutcome::Cancelled;
                }

                // Call LLM again with tool results
                let status_report_update = if let Some(session) = sessions.get(&session_id) {
                    let threshold = session
                        .persisted_config
                        .status_report_min_tool_rounds
                        .max(1);
                    if session.current_turn_tool_rounds >= threshold {
                        Some(
                            generate_status_report_with_model(
                                session,
                                session_id,
                                completed_calls.len(),
                                StatusReportStage::ReviewingResults,
                            )
                            .await,
                        )
                    } else {
                        None
                    }
                } else {
                    None
                };
                if let Some(report) = status_report_update {
                    set_session_status_report(sessions, session_id, io.output, Some(report)).await;
                }
                debug_log_session(session_id, "continuing LLM turn after tool results");
                continue;
            }
            Err(error) => {
                debug_log_session(session_id, format!("LLM turn failed: {error}"));
                set_session_status_report(sessions, session_id, io.output, None).await;
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
                }
                let _ = io
                    .output
                    .send(CoreOutput::SessionStateChanged {
                        session_id,
                        state: SessionState::Idle,
                    })
                    .await;
                let duration_us = turn_start.elapsed().as_micros() as u64;
                emit_failed_turn(
                    io.output,
                    session_id,
                    error,
                    duration_us,
                    accumulated_usage.clone(),
                    accumulated_cache_usage.clone(),
                )
                .await;
                return TurnOutcome::Failed("session error".into());
            }
        }
    }
}

struct SessionActor {
    session_id: SessionId,
    parent_id: Option<SessionId>,
    session: SessionContext,
    command_rx: mpsc::Receiver<SessionCommand>,
    /// Commands accepted while a turn is in flight and replayed between turns.
    ///
    /// User messages live here when a client sends another prompt to a busy
    /// session. Active LLM/tool loops must not drain this queue, otherwise a
    /// queued user message can be re-deferred forever and starve the stream.
    deferred_commands: VecDeque<SessionCommand>,
    output: mpsc::Sender<CoreOutput>,
    core_input_tx: mpsc::Sender<CoreInput>,
    runtime_event_tx: mpsc::Sender<RuntimeEvent>,
    session_tree: SharedSessionTree,
    web_provider: Arc<dyn WebProvider>,
}

impl SessionActor {
    async fn run(mut self) {
        while let Some(command) = self.next_command().await {
            if !self.handle_command(command).await {
                break;
            }
        }
    }

    async fn next_command(&mut self) -> Option<SessionCommand> {
        if let Some(command) = self.deferred_commands.pop_front() {
            return Some(command);
        }
        self.command_rx.recv().await
    }

    async fn handle_command(&mut self, command: SessionCommand) -> bool {
        match command {
            SessionCommand::UserMessage { content, turn_id } => {
                self.handle_user_message(content, turn_id).await
            }
            SessionCommand::ExitPlanMode { reply } => {
                let result = if !matches!(
                    self.session.state,
                    SessionState::Idle | SessionState::Paused
                ) {
                    Err(format!(
                        "cannot exit plan mode while session is {:?}",
                        self.session.state
                    ))
                } else {
                    self.session
                        .exit_plan_mode_with_web_provider(&self.web_provider)
                        .await
                        .map_err(|error| format!("failed to exit plan mode: {error}"))
                };
                let is_ok = result.is_ok();
                let _ = reply.send(result);
                if is_ok {
                    self.emit_checkpoint_hint().await;
                }
                true
            }
            SessionCommand::UpdateSessionLlm { session_llm, reply } => {
                let result = if !matches!(
                    self.session.state,
                    SessionState::Idle | SessionState::Paused
                ) {
                    Err(format!(
                        "cannot switch model while session is {:?}",
                        self.session.state
                    ))
                } else {
                    self.session
                        .update_llm_provider_with_web_provider(
                            Arc::clone(&session_llm.provider),
                            session_llm.model_profile.clone(),
                            session_llm.max_context_window,
                            &self.web_provider,
                        )
                        .await
                        .map_err(|error| format!("failed to update session model: {error}"))
                };
                let is_ok = result.is_ok();
                let _ = reply.send(result);
                if is_ok {
                    self.emit_checkpoint_hint().await;
                }
                true
            }
            SessionCommand::CompactSession { reply } => {
                let provider = Arc::clone(&self.session.provider);
                let result = compact_session_history(
                    provider.as_ref(),
                    &mut self.session,
                    self.session_id,
                    CompactionTrigger::Manual,
                )
                .await
                .map(|_| ())
                .map_err(|error| error.to_string());
                let _ = reply.send(result);
                true
            }
            SessionCommand::ToolResult {
                tool_use_id,
                result,
            } => {
                self.handle_external_tool_result(tool_use_id, result).await;
                true
            }
            SessionCommand::InteractionResponse(_) => {
                debug_log_session(
                    self.session_id,
                    "received unexpected InteractionResponse while session was idle",
                );
                true
            }
            SessionCommand::Cancel => {
                self.interrupt().await;
                true
            }
            SessionCommand::Signal(signal) => {
                self.handle_signal(signal).await;
                true
            }
            SessionCommand::MailboxMessage(message) => {
                self.handle_mailbox_message(message).await;
                true
            }
            SessionCommand::Snapshot { reply } => {
                let _ = reply.send(self.session.snapshot(self.session_id).await);
                true
            }
            SessionCommand::SessionMemoryRefreshFinished {
                last_summarized_message_index,
                refreshed_at,
                listing_summary,
            } => {
                self.session.session_memory.refresh_in_flight = false;
                if let Some(index) = last_summarized_message_index {
                    self.session.session_memory.last_summarized_message_index = Some(index);
                }
                if let Some(timestamp) = refreshed_at {
                    self.session.session_memory.last_refresh_at = Some(timestamp);
                }
                if let Some(summary) = listing_summary {
                    self.session.session_memory.listing_summary = Some(summary);
                }
                let diagnostics = ensure_turn_diagnostics(&mut self.session);
                diagnostics.session_memory.refresh.attempted = true;
                if let Some(index) = last_summarized_message_index {
                    diagnostics.session_memory.refresh.status = MemoryStatus::Succeeded;
                    diagnostics.session_memory.refresh.reason = None;
                    diagnostics
                        .session_memory
                        .refresh
                        .last_summarized_message_index = Some(index);
                } else {
                    diagnostics.session_memory.refresh.status = MemoryStatus::FailedBestEffort;
                    diagnostics.session_memory.refresh.reason =
                        Some(MemoryDecisionReason::MissingSummary);
                }
                if let Some(timestamp) = refreshed_at {
                    diagnostics.session_memory.refresh.refreshed_at = Some(timestamp.to_rfc3339());
                }
                self.emit_checkpoint_hint().await;
                true
            }
            SessionCommand::QueryMailbox { source, reply } => {
                let message = pop_mailbox_message(&mut self.session.mailbox, &source);
                let _ = reply.send(message);
                true
            }
            SessionCommand::ChildExited { child_id, status } => {
                self.handle_child_exited(child_id, status).await;
                true
            }
            SessionCommand::Shutdown => false,
        }
    }

    async fn handle_user_message(&mut self, content: String, turn_id: String) -> bool {
        debug_log_session(
            self.session_id,
            format!("received UserMessage ({} chars)", content.len()),
        );
        self.session.interrupted = false;
        self.clear_active_wait().await;
        self.session.suspended_wait = None;
        self.session.state = SessionState::Streaming;
        self.session.current_turn_tool_rounds = 0;
        self.session.history.push(Message::user(&content));
        self.session.last_memory_diagnostics =
            Some(default_turn_diagnostics_for_session(&self.session));
        self.set_status_report(None).await;
        self.emit_state(SessionState::Streaming).await;
        let _ = self
            .output
            .send(CoreOutput::TurnStarted {
                session_id: self.session_id,
                turn_id,
            })
            .await;

        let outcome = self.handle_turn().await;
        self.finish_turn(outcome).await
    }

    async fn handle_signal(&mut self, signal: SessionSignal) {
        debug_log_session(self.session_id, format!("received Signal::{signal:?}"));
        match signal {
            SessionSignal::Continue => {
                self.session.interrupted = false;
                if self.session.state == SessionState::Paused {
                    self.session.state = SessionState::Idle;
                }
                self.emit_state(self.session.state).await;
            }
            SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill => {
                self.interrupt().await;
            }
        }
    }

    async fn interrupt(&mut self) {
        self.clear_active_wait().await;
        self.session.state = SessionState::Idle;
        self.session.interrupted = true;
        if let Some(cancel_tx) = &self.session.cancel_tx {
            let _ = cancel_tx.send(true);
        }
        self.session.cancel_tx = None;
        self.session.pending_interaction.take();
        self.session.pending_permission_approval = None;
        self.emit_state(SessionState::Idle).await;
        self.emit_checkpoint_hint().await;
    }

    async fn clear_active_wait(&self) {
        self.session_tree
            .write()
            .await
            .clear_active_wait(self.session_id);
    }

    async fn emit_state(&self, state: SessionState) {
        let _ = self
            .output
            .send(CoreOutput::SessionStateChanged {
                session_id: self.session_id,
                state,
            })
            .await;
    }

    async fn set_status_report(&mut self, report: Option<SessionStatusReport>) {
        if self.session.status_report == report {
            return;
        }
        self.session.status_report = report.clone();
        let _ = self
            .output
            .send(CoreOutput::SessionStatusReport {
                session_id: self.session_id,
                report,
            })
            .await;
    }

    async fn emit_checkpoint_hint(&self) {
        let _ = self
            .runtime_event_tx
            .send(RuntimeEvent::CheckpointHint)
            .await;
    }

    async fn handle_mailbox_message(&mut self, message: MailboxMessage) {
        let should_resume = matches!(
            self.session.suspended_wait.as_ref(),
            Some(SuspendedWait::Mailbox { source, .. }) if mailbox_matches_source(&message, source)
        );
        self.session.mailbox.push_back(message);
        if should_resume {
            let _ = self.resume_from_wait(None).await;
        }
    }

    async fn handle_child_exited(&mut self, child_id: SessionId, _status: ExitStatus) {
        let should_resume = matches!(
            self.session.suspended_wait.as_ref(),
            Some(SuspendedWait::ChildExit { child_id: waiting_child, .. }) if *waiting_child == child_id
        );
        if should_resume {
            let _ = self.resume_from_wait(None).await;
        }
    }

    async fn finish_turn(&mut self, outcome: TurnOutcome) -> bool {
        if let Some(parent_id) = self.parent_id {
            let status = match outcome {
                TurnOutcome::Completed(text) => ExitStatus::Success {
                    output: text
                        .or_else(|| session_output(&self.session))
                        .unwrap_or_default(),
                },
                TurnOutcome::Failed(error) => ExitStatus::Failed { error },
                TurnOutcome::Cancelled => ExitStatus::Cancelled,
                TurnOutcome::Suspended => return true,
            };
            self.session.state = SessionState::Destroyed;
            let _ = self
                .runtime_event_tx
                .send(RuntimeEvent::ChildSessionFinished {
                    session_id: self.session_id,
                    parent_id,
                    status,
                })
                .await;
            false
        } else {
            true
        }
    }

    async fn handle_external_tool_result(&mut self, tool_use_id: String, result: ToolOutcome) {
        debug_log_session(
            self.session_id,
            format!("received external ToolResult for tool_use_id={tool_use_id}"),
        );
        if self.session.suspended_wait.is_some() {
            let _ = self.resume_from_wait(Some(result)).await;
            return;
        }

        if self.session.state != SessionState::AwaitingToolResult {
            debug_log_session(
                self.session_id,
                format!(
                    "ignoring stale external ToolResult for tool_use_id={tool_use_id} while session is {:?}",
                    self.session.state
                ),
            );
            return;
        }

        let (output_text, is_error) = match &result {
            ToolOutcome::Success { output } => (output.clone(), false),
            ToolOutcome::Error { message } => (message.clone(), true),
            ToolOutcome::Cancelled => ("Tool execution was cancelled".to_string(), true),
        };
        let history_output = match prepare_tool_result_for_history(
            &self.session,
            self.session_id,
            &tool_use_id,
            "external",
            &output_text,
            is_error,
        )
        .await
        {
            Ok(output) => output,
            Err(error) => {
                let _ = self
                    .output
                    .send(CoreOutput::SessionError {
                        session_id: self.session_id,
                        error,
                    })
                    .await;
                return;
            }
        };
        self.session.history.push(Message::tool_result(
            &tool_use_id,
            &history_output,
            is_error,
        ));
        self.session.state = SessionState::Streaming;
        self.emit_state(SessionState::Streaming).await;
        let outcome = self.handle_turn().await;
        let _ = self.finish_turn(outcome).await;
    }

    async fn resume_from_wait(&mut self, explicit_result: Option<ToolOutcome>) -> Result<(), ()> {
        let Some(wait) = self.session.suspended_wait.clone() else {
            return Ok(());
        };
        let tool_use_id = wait.tool_use_id().to_string();
        let result = if let Some(result) = explicit_result {
            result
        } else {
            match wait {
                SuspendedWait::Mailbox { source, .. } => {
                    match pop_mailbox_message(&mut self.session.mailbox, &source) {
                        Some(MailboxMessage { from, content }) => ToolOutcome::Success {
                            output: serde_json::json!({
                                "from": from,
                                "content": content,
                            })
                            .to_string(),
                        },
                        None => ToolOutcome::Error {
                            message: "recv_message resumed without a matching message".into(),
                        },
                    }
                }
                SuspendedWait::ChildExit { child_id, .. } => {
                    match self
                        .session_tree
                        .read()
                        .await
                        .exit_status(child_id)
                        .cloned()
                    {
                        Some(status) => ToolOutcome::Success {
                            output: serde_json::to_string(&status)
                                .unwrap_or_else(|_| "unknown".into()),
                        },
                        None => ToolOutcome::Error {
                            message: "wait_child resumed before child exit was recorded".into(),
                        },
                    }
                }
            }
        };

        let (output_text, is_error) = match &result {
            ToolOutcome::Success { output } => (output.clone(), false),
            ToolOutcome::Error { message } => (message.clone(), true),
            ToolOutcome::Cancelled => ("Tool execution was cancelled".to_string(), true),
        };
        let history_output = match prepare_tool_result_for_history(
            &self.session,
            self.session_id,
            &tool_use_id,
            "suspended_wait",
            &output_text,
            is_error,
        )
        .await
        {
            Ok(output) => output,
            Err(error) => {
                let _ = self
                    .output
                    .send(CoreOutput::SessionError {
                        session_id: self.session_id,
                        error,
                    })
                    .await;
                return Err(());
            }
        };

        self.session.suspended_wait = None;
        self.clear_active_wait().await;
        self.session.history.push(Message::tool_result(
            &tool_use_id,
            &history_output,
            is_error,
        ));
        self.session.state = SessionState::Streaming;
        let _ = self
            .output
            .send(CoreOutput::ToolResult {
                session_id: self.session_id,
                tool_use_id,
                tool_name: "suspended_wait".into(),
                content: output_text,
                is_error,
                duration_us: 0,
            })
            .await;
        self.emit_state(SessionState::Streaming).await;
        let outcome = self.handle_turn().await;
        let _ = self.finish_turn(outcome).await;
        Ok(())
    }

    async fn handle_turn(&mut self) -> TurnOutcome {
        let turn_start = std::time::Instant::now();
        let mut accumulated_usage: Option<quine_llm::TokenUsage> = None;
        let mut accumulated_cache_usage: Option<PromptCacheUsage> = None;
        debug_log_session(
            self.session_id,
            format!(
                "starting LLM turn with {} history messages",
                self.session.history.len()
            ),
        );

        loop {
            if let Err(error) =
                archive_old_tool_results_in_history(&mut self.session, self.session_id).await
            {
                self.session.state = SessionState::Idle;
                let duration_us = turn_start.elapsed().as_micros() as u64;
                self.emit_state(SessionState::Idle).await;
                emit_failed_turn(
                    &self.output,
                    self.session_id,
                    error,
                    duration_us,
                    accumulated_usage.clone(),
                    accumulated_cache_usage.clone(),
                )
                .await;
                return TurnOutcome::Failed("session error".into());
            }

            let should_auto_compact = compaction::should_auto_compact(
                self.session.max_context_window,
                self.session.last_input_tokens,
                self.session.auto_compact_threshold_percent,
            );
            if should_auto_compact {
                let provider = Arc::clone(&self.session.provider);
                if let Err(error) = compact_session_history(
                    provider.as_ref(),
                    &mut self.session,
                    self.session_id,
                    CompactionTrigger::Auto,
                )
                .await
                {
                    self.session.state = SessionState::Idle;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    self.emit_state(SessionState::Idle).await;
                    emit_failed_turn(
                        &self.output,
                        self.session_id,
                        error,
                        duration_us,
                        accumulated_usage.clone(),
                        accumulated_cache_usage.clone(),
                    )
                    .await;
                    return TurnOutcome::Failed("session error".into());
                }
            }

            if self.session.interrupted {
                debug_log_session(
                    self.session_id,
                    "aborting LLM turn because session is interrupted",
                );
                let duration_us = turn_start.elapsed().as_micros() as u64;
                let _ = self
                    .output
                    .send(CoreOutput::TurnComplete {
                        session_id: self.session_id,
                        duration_us,
                        status: TurnStatus::Cancelled,
                        usage: accumulated_usage.clone(),
                        cache_usage: accumulated_cache_usage.clone(),
                    })
                    .await;
                return TurnOutcome::Cancelled;
            }

            let history = match build_provider_messages(&mut self.session).await {
                Ok(history) => history,
                Err(error) => {
                    self.session.state = SessionState::Idle;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    self.emit_state(SessionState::Idle).await;
                    emit_failed_turn(
                        &self.output,
                        self.session_id,
                        error,
                        duration_us,
                        accumulated_usage.clone(),
                        accumulated_cache_usage.clone(),
                    )
                    .await;
                    return TurnOutcome::Failed("session error".into());
                }
            };
            let tools = self.session.tools.clone();
            let prompt_cache_tokens = canonicalize_prompt_cache_tokens(&history, &tools);
            let cache_usage = estimate_prompt_cache_usage(
                self.session.last_prompt_cache_tokens.as_deref(),
                &prompt_cache_tokens,
            );
            let provider = Arc::clone(&self.session.provider);

            match self
                .call_llm_interruptible(provider.as_ref(), history, tools)
                .await
            {
                Ok(None) => {
                    debug_log_session(self.session_id, "LLM turn interrupted");
                    self.set_status_report(None).await;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    self.emit_state(SessionState::Idle).await;
                    let _ = self
                        .output
                        .send(CoreOutput::TurnComplete {
                            session_id: self.session_id,
                            duration_us,
                            status: TurnStatus::Cancelled,
                            usage: accumulated_usage.clone(),
                            cache_usage: accumulated_cache_usage.clone(),
                        })
                        .await;
                    return TurnOutcome::Cancelled;
                }
                Ok(Some(LlmCallResult {
                    turn: LlmTurnResult::Text(full_text),
                    usage,
                    ..
                })) => {
                    let acc = accumulated_cache_usage.get_or_insert_with(PromptCacheUsage::default);
                    acc.estimated_hit_tokens += cache_usage.estimated_hit_tokens;
                    acc.estimated_miss_tokens += cache_usage.estimated_miss_tokens;

                    if let Some(u) = usage {
                        let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                        acc.input_tokens += u.input_tokens;
                        acc.output_tokens += u.output_tokens;
                        self.session.last_input_tokens = Some(u.input_tokens);
                    } else {
                        self.session.last_input_tokens = None;
                    }
                    self.session.last_prompt_cache_tokens = Some(prompt_cache_tokens);
                    self.session.history.push(Message::assistant(&full_text));

                    let status_report_update = {
                        let threshold = self
                            .session
                            .persisted_config
                            .status_report_min_tool_rounds
                            .max(1);
                        if should_emit_completed_status_report(
                            self.session.current_turn_tool_rounds,
                            threshold,
                            self.session.status_report.is_some(),
                        ) {
                            Some(
                                generate_status_report_with_model(
                                    &self.session,
                                    self.session_id,
                                    0,
                                    StatusReportStage::Completed,
                                )
                                .await,
                            )
                        } else {
                            None
                        }
                    };
                    if let Some(report) = status_report_update {
                        self.set_status_report(Some(report)).await;
                    }

                    let _ = self
                        .output
                        .send(CoreOutput::TextComplete {
                            session_id: self.session_id,
                            full_text: full_text.clone(),
                        })
                        .await;
                    self.session.state = SessionState::Idle;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    let _ = self
                        .output
                        .send(CoreOutput::TurnComplete {
                            session_id: self.session_id,
                            duration_us,
                            status: TurnStatus::Success,
                            usage: accumulated_usage,
                            cache_usage: accumulated_cache_usage,
                        })
                        .await;
                    self.emit_state(SessionState::Idle).await;
                    let provider = Arc::clone(&self.session.provider);
                    schedule_session_memory_refresh(
                        &mut self.session,
                        self.session_id,
                        provider,
                        self.core_input_tx.clone(),
                    );
                    self.emit_checkpoint_hint().await;
                    return TurnOutcome::Completed(Some(full_text));
                }
                Ok(Some(LlmCallResult {
                    turn:
                        LlmTurnResult::ToolCalls {
                            text_before,
                            mut calls,
                        },
                    usage,
                    ..
                })) => {
                    let acc = accumulated_cache_usage.get_or_insert_with(PromptCacheUsage::default);
                    acc.estimated_hit_tokens += cache_usage.estimated_hit_tokens;
                    acc.estimated_miss_tokens += cache_usage.estimated_miss_tokens;

                    if let Some(u) = usage {
                        let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                        acc.input_tokens += u.input_tokens;
                        acc.output_tokens += u.output_tokens;
                        self.session.last_input_tokens = Some(u.input_tokens);
                    } else {
                        self.session.last_input_tokens = None;
                    }
                    self.session.last_prompt_cache_tokens = Some(prompt_cache_tokens);

                    if let Some(ref text) = text_before {
                        let _ = self
                            .output
                            .send(CoreOutput::TextComplete {
                                session_id: self.session_id,
                                full_text: text.clone(),
                            })
                            .await;
                    }

                    let tool_use_requests: Vec<quine_llm::ToolUseRequest> = calls
                        .iter()
                        .map(|c| quine_llm::ToolUseRequest {
                            tool_use_id: c.tool_use_id.clone(),
                            tool_name: c.tool_name.clone(),
                            arguments: c.arguments.clone(),
                        })
                        .collect();
                    self.session.history.push(Message::assistant_tool_use(
                        text_before.clone(),
                        tool_use_requests,
                    ));
                    self.session.current_turn_tool_rounds =
                        self.session.current_turn_tool_rounds.saturating_add(1);
                    calls = calls
                        .iter()
                        .map(|call| normalize_plan_tool_arguments(&self.session, call))
                        .collect();

                    let mut completed_calls = Vec::with_capacity(calls.len());
                    for call in &calls {
                        if self.session.interrupted {
                            let duration_us = turn_start.elapsed().as_micros() as u64;
                            self.emit_state(SessionState::Idle).await;
                            let _ = self
                                .output
                                .send(CoreOutput::TurnComplete {
                                    session_id: self.session_id,
                                    duration_us,
                                    status: TurnStatus::Cancelled,
                                    usage: accumulated_usage.clone(),
                                    cache_usage: accumulated_cache_usage.clone(),
                                })
                                .await;
                            return TurnOutcome::Cancelled;
                        }
                        let _ = self
                            .output
                            .send(CoreOutput::ToolRequest {
                                session_id: self.session_id,
                                tool_use_id: call.tool_use_id.clone(),
                                tool_name: call.tool_name.clone(),
                                arguments: call.arguments.clone(),
                            })
                            .await;
                        let tool_start = std::time::Instant::now();
                        let result = self.execute_tool_call(call).await;
                        completed_calls.push(CompletedConcurrentToolCall {
                            index: completed_calls.len(),
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            arguments: call.arguments.clone(),
                            result,
                            duration_us: tool_start.elapsed().as_micros() as u64,
                        });
                    }

                    let mut saw_cancelled_tool = false;
                    for completed_call in &completed_calls {
                        let (tool_output, is_error) = match &completed_call.result {
                            ToolOutcome::Success { output } => (output.clone(), false),
                            ToolOutcome::Error { message } => (message.clone(), true),
                            ToolOutcome::Cancelled => {
                                ("Tool execution was cancelled".to_string(), true)
                            }
                        };
                        let _ = self
                            .output
                            .send(CoreOutput::ToolResult {
                                session_id: self.session_id,
                                tool_use_id: completed_call.tool_use_id.clone(),
                                tool_name: completed_call.tool_name.clone(),
                                content: tool_output.clone(),
                                is_error,
                                duration_us: completed_call.duration_us,
                            })
                            .await;

                        let history_output = match prepare_tool_result_for_history(
                            &self.session,
                            self.session_id,
                            &completed_call.tool_use_id,
                            &completed_call.tool_name,
                            &tool_output,
                            is_error,
                        )
                        .await
                        {
                            Ok(output) => output,
                            Err(error) => {
                                self.session.cancel_tx = None;
                                self.session.state = SessionState::Idle;
                                let duration_us = turn_start.elapsed().as_micros() as u64;
                                self.emit_state(SessionState::Idle).await;
                                emit_failed_turn(
                                    &self.output,
                                    self.session_id,
                                    error.clone(),
                                    duration_us,
                                    accumulated_usage.clone(),
                                    accumulated_cache_usage.clone(),
                                )
                                .await;
                                return TurnOutcome::Failed(error.to_string());
                            }
                        };
                        self.session.history.push(Message::tool_result(
                            &completed_call.tool_use_id,
                            &history_output,
                            is_error,
                        ));

                        let should_suspend =
                            matches!(completed_call.result, ToolOutcome::Cancelled)
                                && self.session.suspended_wait.is_some();
                        if should_suspend {
                            return TurnOutcome::Suspended;
                        }

                        if matches!(completed_call.result, ToolOutcome::Cancelled) {
                            saw_cancelled_tool = true;
                        }

                        if completed_call.tool_name == "plan" {
                            let is_update = completed_call
                                .arguments
                                .get("operation")
                                .and_then(|v| v.as_str())
                                == Some("update_plan");
                            if is_update {
                                if let Some(plan_id_str) = completed_call
                                    .arguments
                                    .get("plan_id")
                                    .and_then(|v| v.as_str())
                                {
                                    if let Some(action_id_str) = completed_call
                                        .arguments
                                        .get("action_id")
                                        .and_then(|v| v.as_str())
                                    {
                                        self.handle_plan_progress(plan_id_str, action_id_str).await;
                                    }
                                }
                            }
                        }
                    }

                    if saw_cancelled_tool {
                        self.set_status_report(None).await;
                        self.session.cancel_tx = None;
                        self.session.state = SessionState::Idle;
                        let duration_us = turn_start.elapsed().as_micros() as u64;
                        self.emit_state(SessionState::Idle).await;
                        let _ = self
                            .output
                            .send(CoreOutput::TurnComplete {
                                session_id: self.session_id,
                                duration_us,
                                status: TurnStatus::Cancelled,
                                usage: accumulated_usage.clone(),
                                cache_usage: accumulated_cache_usage.clone(),
                            })
                            .await;
                        let provider = Arc::clone(&self.session.provider);
                        schedule_session_memory_refresh(
                            &mut self.session,
                            self.session_id,
                            provider,
                            self.core_input_tx.clone(),
                        );
                        self.emit_checkpoint_hint().await;
                        return TurnOutcome::Cancelled;
                    }

                    let status_report_update = {
                        let threshold = self
                            .session
                            .persisted_config
                            .status_report_min_tool_rounds
                            .max(1);
                        if should_emit_periodic_status_report(
                            self.session.current_turn_tool_rounds,
                            threshold,
                        ) {
                            Some(
                                generate_status_report_with_model(
                                    &self.session,
                                    self.session_id,
                                    completed_calls.len(),
                                    StatusReportStage::ReviewingResults,
                                )
                                .await,
                            )
                        } else {
                            None
                        }
                    };
                    if let Some(report) = status_report_update {
                        self.set_status_report(Some(report)).await;
                    }
                    continue;
                }
                Err(error) => {
                    debug_log_session(self.session_id, format!("LLM turn failed: {error}"));
                    self.set_status_report(None).await;
                    self.session.state = SessionState::Idle;
                    self.emit_state(SessionState::Idle).await;
                    let duration_us = turn_start.elapsed().as_micros() as u64;
                    emit_failed_turn(
                        &self.output,
                        self.session_id,
                        error,
                        duration_us,
                        accumulated_usage.clone(),
                        accumulated_cache_usage.clone(),
                    )
                    .await;
                    return TurnOutcome::Failed("session error".into());
                }
            }
        }
    }

    async fn call_llm_interruptible(
        &mut self,
        provider: &dyn LlmProvider,
        history: Vec<Message>,
        tools: Vec<ToolDefinition>,
    ) -> Result<Option<LlmCallResult>, CoreError> {
        let send_future = provider.send(&history, &tools);
        tokio::pin!(send_future);

        let mut stream = loop {
            tokio::select! {
                stream_result = &mut send_future => {
                    break stream_result.map_err(|e| CoreError::LlmError {
                        message: format!("{e:#}"),
                    })?;
                }
                maybe_command = self.command_rx.recv() => {
                    match maybe_command {
                        Some(command) => {
                            if self.handle_control_command(command).await {
                                debug_log_session(self.session_id, "LLM request interrupted before stream opened");
                                return Ok(None);
                            }
                        }
                        None => {
                            return Err(CoreError::Internal {
                                message: "input channel closed while awaiting LLM response".into(),
                            });
                        }
                    }
                }
            }
        };

        let mut full_text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = None;

        loop {
            tokio::select! {
                event_result = stream.next() => {
                    let Some(event_result) = event_result else {
                        break;
                    };
                    match event_result {
                        Ok(LlmEvent::ReasoningDelta { text }) => {
                            let _ = self
                                .output
                                .send(CoreOutput::ReasoningDelta {
                                    session_id: self.session_id,
                                    delta: text,
                                })
                                .await;
                        }
                        Ok(LlmEvent::TextDelta { text }) => {
                            full_text.push_str(&text);
                            let _ = self
                                .output
                                .send(CoreOutput::StreamDelta {
                                    session_id: self.session_id,
                                    delta: text,
                                })
                                .await;
                        }
                        Ok(LlmEvent::ToolCall {
                            tool_use_id,
                            tool_name,
                            arguments,
                        }) => {
                            tool_calls.push(PendingToolCall {
                                tool_use_id,
                                tool_name,
                                arguments,
                            });
                        }
                        Ok(LlmEvent::Done { usage: u }) => {
                            usage = u;
                            break;
                        }
                        Err(e) => {
                            return Err(CoreError::LlmError {
                                message: format!("{e:#}"),
                            });
                        }
                    }
                }
                maybe_command = self.command_rx.recv() => {
                    match maybe_command {
                        Some(command) => {
                            if self.handle_control_command(command).await {
                                debug_log_session(self.session_id, "LLM stream interrupted");
                                return Ok(None);
                            }
                        }
                        None => {
                            return Err(CoreError::Internal {
                                message: "input channel closed while streaming LLM response".into(),
                            });
                        }
                    }
                }
            }
        }

        let tool_calls = deduplicate_pending_tool_calls(self.session_id, tool_calls);
        let turn = if tool_calls.is_empty() {
            LlmTurnResult::Text(full_text)
        } else {
            LlmTurnResult::ToolCalls {
                text_before: if full_text.is_empty() {
                    None
                } else {
                    Some(full_text)
                },
                calls: tool_calls,
            }
        };
        Ok(Some(LlmCallResult { turn, usage }))
    }

    async fn handle_control_command(&mut self, command: SessionCommand) -> bool {
        match command {
            SessionCommand::Cancel => {
                self.clear_active_wait().await;
                self.session.state = SessionState::Idle;
                self.session.interrupted = true;
                if let Some(cancel_tx) = &self.session.cancel_tx {
                    let _ = cancel_tx.send(true);
                }
                self.session.cancel_tx = None;
                self.session.pending_interaction.take();
                self.session.pending_permission_approval = None;
                true
            }
            SessionCommand::Signal(
                SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill,
            ) => {
                self.clear_active_wait().await;
                self.session.state = SessionState::Idle;
                self.session.interrupted = true;
                if let Some(cancel_tx) = &self.session.cancel_tx {
                    let _ = cancel_tx.send(true);
                }
                self.session.cancel_tx = None;
                self.session.pending_interaction.take();
                self.session.pending_permission_approval = None;
                true
            }
            SessionCommand::MailboxMessage(message) => {
                self.session.mailbox.push_back(message);
                false
            }
            SessionCommand::ChildExited { .. } => false,
            SessionCommand::Snapshot { reply } => {
                let _ = reply.send(self.session.snapshot(self.session_id).await);
                false
            }
            SessionCommand::SessionMemoryRefreshFinished {
                last_summarized_message_index,
                refreshed_at,
                listing_summary,
            } => {
                self.deferred_commands
                    .push_back(SessionCommand::SessionMemoryRefreshFinished {
                        last_summarized_message_index,
                        refreshed_at,
                        listing_summary,
                    });
                false
            }
            SessionCommand::QueryMailbox { source, reply } => {
                let message = pop_mailbox_message(&mut self.session.mailbox, &source);
                let _ = reply.send(message);
                false
            }
            SessionCommand::InteractionResponse(_)
            | SessionCommand::ExitPlanMode { .. }
            | SessionCommand::UpdateSessionLlm { .. }
            | SessionCommand::CompactSession { .. }
            | SessionCommand::ToolResult { .. }
            | SessionCommand::UserMessage { .. }
            | SessionCommand::Shutdown => {
                self.deferred_commands.push_back(command);
                false
            }
            SessionCommand::Signal(_) => false,
        }
    }

    async fn execute_tool_call(&mut self, call: &PendingToolCall) -> ToolOutcome {
        let (cancel_tx, cancellation) = CancellationChannel::new_pair();
        if self.session.interrupted {
            return ToolOutcome::Cancelled;
        }
        self.session.cancel_tx = Some(cancel_tx.clone());

        let tool = match self.session.tool_registry.get(&call.tool_name) {
            Some(tool) => Arc::clone(tool),
            None => {
                return ToolOutcome::Error {
                    message: format!("unknown tool: {}", call.tool_name),
                };
            }
        };

        let (request, local) = build_permission_request(&self.session, call, tool.as_ref());
        let permission_outcome =
            evaluate_permission(&self.session.permission_context, request, local);
        self.session.last_permission_outcome = Some(permission_outcome.clone());

        if !permission_outcome.is_allowed() {
            if permission_outcome.kind
                == crate::permission::outcome::PermissionOutcomeKind::RequiresApproval
            {
                let (pending, request) = build_permission_approval_request(&permission_outcome);
                self.session.state = SessionState::Paused;
                self.session.pending_permission_approval = Some(pending);
                let _ = self
                    .output
                    .send(CoreOutput::InteractionNeeded {
                        session_id: self.session_id,
                        request,
                    })
                    .await;
                self.emit_state(SessionState::Paused).await;
                loop {
                    match self.command_rx.recv().await {
                        Some(SessionCommand::InteractionResponse(response)) => {
                            let Some(parsed) = parse_permission_approval_response(&response) else {
                                continue;
                            };
                            match parsed {
                                PermissionApprovalChoice::ApproveOnce => {
                                    self.session.pending_permission_approval = None;
                                    self.session.state = SessionState::Streaming;
                                    self.emit_state(SessionState::Streaming).await;
                                    break;
                                }
                                PermissionApprovalChoice::DenyOnce => {
                                    self.session.pending_permission_approval = None;
                                    self.session.state = SessionState::Streaming;
                                    self.emit_state(SessionState::Streaming).await;
                                    self.session.cancel_tx = None;
                                    return ToolOutcome::Error {
                                        message: format!(
                                            "permission denied: {}",
                                            permission_outcome.reason
                                        ),
                                    };
                                }
                            }
                        }
                        Some(command) => {
                            if self.handle_control_command(command).await {
                                self.session.cancel_tx = None;
                                return ToolOutcome::Cancelled;
                            }
                        }
                        None => {
                            self.session.cancel_tx = None;
                            return ToolOutcome::Error {
                                message: "input channel closed".into(),
                            };
                        }
                    }
                }
            } else {
                self.session.cancel_tx = None;
                return ToolOutcome::Error {
                    message: format!("permission denied: {}", permission_outcome.reason),
                };
            }
        }

        if call.tool_name == "wait_child" {
            let child_id_str = match call.arguments.get("child_id").and_then(|v| v.as_str()) {
                Some(child_id) => child_id,
                None => {
                    self.session.cancel_tx = None;
                    return ToolOutcome::Error {
                        message: "invalid arguments: missing required parameter: child_id".into(),
                    };
                }
            };
            let child_id = match crate::tool::wait_child::parse_session_id(child_id_str) {
                Some(child_id) => child_id,
                None => {
                    self.session.cancel_tx = None;
                    return ToolOutcome::Error {
                        message: format!("invalid arguments: invalid child_id: {child_id_str}"),
                    };
                }
            };
            let non_blocking = call
                .arguments
                .get("non_blocking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let timeout = call
                .arguments
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .map(Duration::from_millis);

            let tree = self.session_tree.read().await;
            let result = if tree.parent_of(child_id) == Some(self.session_id) {
                tree.exit_status(child_id).cloned()
            } else {
                None
            };
            drop(tree);

            self.session.cancel_tx = None;
            if let Some(status) = result {
                return ToolOutcome::Success {
                    output: serde_json::to_string(&status).unwrap_or_else(|_| "unknown".into()),
                };
            }
            if non_blocking {
                return ToolOutcome::Success {
                    output: "null".into(),
                };
            }
            if let Err(error) = self
                .session_tree
                .write()
                .await
                .register_active_wait(self.session_id, child_id)
            {
                return ToolOutcome::Error { message: error };
            }
            self.session.state = SessionState::Waiting;
            self.session.suspended_wait = Some(SuspendedWait::ChildExit {
                tool_use_id: call.tool_use_id.clone(),
                child_id,
                timeout_at: timeout.map(|value| Instant::now() + value),
            });
            self.emit_state(SessionState::Waiting).await;
            self.emit_checkpoint_hint().await;
            return ToolOutcome::Cancelled;
        }

        if call.tool_name == "recv_message" {
            let source_str = match call.arguments.get("source").and_then(|v| v.as_str()) {
                Some(source) => source,
                None => {
                    self.session.cancel_tx = None;
                    return ToolOutcome::Error {
                        message: "invalid arguments: missing required parameter: source".into(),
                    };
                }
            };
            let non_blocking = call
                .arguments
                .get("non_blocking")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let timeout = call
                .arguments
                .get("timeout_ms")
                .and_then(|v| v.as_u64())
                .map(Duration::from_millis);
            let source = if source_str == "any" {
                MessageSource::Any
            } else {
                match crate::tool::wait_child::parse_session_id(source_str) {
                    Some(source_id) => MessageSource::Session(source_id),
                    None => {
                        self.session.cancel_tx = None;
                        return ToolOutcome::Error {
                            message: format!(
                                "invalid arguments: invalid source session_id: {source_str}"
                            ),
                        };
                    }
                }
            };

            if let Some(message) = pop_mailbox_message(&mut self.session.mailbox, &source) {
                self.session.cancel_tx = None;
                return ToolOutcome::Success {
                    output: serde_json::json!({
                        "from": message.from,
                        "content": message.content,
                    })
                    .to_string(),
                };
            }

            self.session.cancel_tx = None;
            if non_blocking {
                return ToolOutcome::Success {
                    output: "null".into(),
                };
            }

            if let MessageSource::Session(source_session) = source {
                if let Err(error) = self
                    .session_tree
                    .write()
                    .await
                    .register_active_wait(self.session_id, source_session)
                {
                    return ToolOutcome::Error { message: error };
                }
            }

            self.session.state = SessionState::Waiting;
            self.session.suspended_wait = Some(SuspendedWait::Mailbox {
                tool_use_id: call.tool_use_id.clone(),
                source,
                timeout_at: timeout.map(|value| Instant::now() + value),
            });
            self.emit_state(SessionState::Waiting).await;
            self.emit_checkpoint_hint().await;
            return ToolOutcome::Cancelled;
        }

        let filesystem = Arc::clone(&self.session.filesystem);
        let working_directory = self.session.working_directory.clone();
        let plan_store = self.session.plan_store.clone();
        let session_group = self.session.python_group.clone();
        let python_runtime = Arc::clone(&self.session.python_runtime);

        if tool.is_interactive() {
            let (req_tx, mut req_rx) =
                mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);
            let channel = InteractionChannel { request_tx: req_tx };
            let ctx = ExecutionContext {
                session_id: self.session_id,
                filesystem,
                working_directory,
                interaction_channel: Some(channel),
                plan_store,
                session_group: session_group.clone(),
                python_runtime: Arc::clone(&python_runtime),
                core_input: Some(self.core_input_tx.clone()),
                cancellation: cancellation.clone(),
            };
            let args = call.arguments.clone();
            let mut tool_handle = tokio::spawn(async move { tool.execute(args, &ctx).await });

            let outcome = 'tool_loop: loop {
                tokio::select! {
                    result = &mut tool_handle => {
                        break 'tool_loop match result {
                            Ok(Ok(tool_output)) if tool_output.is_error => ToolOutcome::Error { message: tool_output.content },
                            Ok(Ok(tool_output)) => ToolOutcome::Success { output: tool_output.content },
                            Ok(Err(ToolError::Cancelled)) => ToolOutcome::Cancelled,
                            Ok(Err(tool_err)) => ToolOutcome::Error { message: tool_err.to_string() },
                            Err(join_err) => ToolOutcome::Error { message: format!("tool task panicked: {join_err}") },
                        };
                    }
                    maybe_command = self.command_rx.recv() => {
                        match maybe_command {
                            Some(command) => {
                                if self.handle_control_command(command).await {
                                    break 'tool_loop ToolOutcome::Cancelled;
                                }
                            }
                            None => {
                                break 'tool_loop ToolOutcome::Error {
                                    message: "input channel closed during interactive execution".into(),
                                };
                            }
                        }
                    }
                    interaction = req_rx.recv() => {
                        if let Some((request, reply_tx)) = interaction {
                            let _ = self.output.send(CoreOutput::InteractionNeeded {
                                session_id: self.session_id,
                                request,
                            }).await;
                            loop {
                                match self.command_rx.recv().await {
                                    Some(SessionCommand::InteractionResponse(response)) => {
                                        let _ = reply_tx.send(response);
                                        break;
                                    }
                                    Some(command) => {
                                        if self.handle_control_command(command).await {
                                            drop(reply_tx);
                                            break 'tool_loop ToolOutcome::Cancelled;
                                        }
                                    }
                                    None => {
                                        break 'tool_loop ToolOutcome::Error {
                                            message: "input channel closed".into(),
                                        };
                                    }
                                }
                            }
                        }
                    }
                }
            };
            if let Some(cancel_tx) = self.session.cancel_tx.as_ref() {
                let _ = cancel_tx.send(true);
            }
            self.session.cancel_tx = None;
            outcome
        } else {
            let ctx = ExecutionContext {
                session_id: self.session_id,
                filesystem,
                working_directory,
                interaction_channel: None,
                plan_store,
                session_group,
                python_runtime,
                core_input: Some(self.core_input_tx.clone()),
                cancellation: cancellation.clone(),
            };
            let tool_future = tool.execute(call.arguments.clone(), &ctx);
            tokio::pin!(tool_future);
            let result = loop {
                tokio::select! {
                    result = &mut tool_future => {
                        break match result {
                            Ok(tool_output) if tool_output.is_error => ToolOutcome::Error { message: tool_output.content },
                            Ok(tool_output) => ToolOutcome::Success { output: tool_output.content },
                            Err(ToolError::Cancelled) => ToolOutcome::Cancelled,
                            Err(tool_err) => ToolOutcome::Error { message: tool_err.to_string() },
                        };
                    }
                    maybe_command = self.command_rx.recv() => {
                        match maybe_command {
                            Some(command) => {
                                if self.handle_control_command(command).await {
                                    break ToolOutcome::Cancelled;
                                }
                            }
                            None => {
                                break ToolOutcome::Error { message: "input channel closed during execution".into() };
                            }
                        }
                    }
                }
            };
            if let Some(cancel_tx) = self.session.cancel_tx.as_ref() {
                let _ = cancel_tx.send(true);
            }
            self.session.cancel_tx = None;
            result
        }
    }

    async fn handle_plan_progress(&mut self, plan_id_str: &str, action_id_str: &str) {
        let plan_id = match crate::planner::PlanId::from_str(plan_id_str) {
            Ok(plan_id) => plan_id,
            Err(_) => return,
        };
        let store = self.session.plan_store.lock().await;
        let Some(plan) = store.get(&plan_id) else {
            return;
        };
        let Some(action_id) = plan
            .actions
            .iter()
            .find(|candidate| candidate.action_id.to_string() == action_id_str)
            .map(|candidate| candidate.action_id.clone())
        else {
            return;
        };
        let remaining = get_ready_actions(plan)
            .into_iter()
            .filter(|candidate| candidate.action_id != action_id)
            .count();
        let total = plan.actions.len();
        let status = plan
            .actions
            .iter()
            .find(|action| action.action_id == action_id)
            .map(|action| action.status.label().to_string())
            .unwrap_or_else(|| "unknown".into());
        drop(store);
        let _ = self
            .output
            .send(CoreOutput::PlanProgress {
                session_id: self.session_id,
                plan_id: plan_id_str.to_string(),
                action_id: action_id_str.to_string(),
                status,
                remaining,
                total,
            })
            .await;
    }
}

async fn snapshot_registry_sessions(
    registry: &SessionRegistry,
    session_tree: &SharedSessionTree,
) -> CoreCheckpoint {
    let handles: Vec<SessionHandle> = registry.read().await.values().cloned().collect();
    let mut persisted_sessions = Vec::new();
    for handle in handles {
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .command_tx
            .send(SessionCommand::Snapshot { reply: reply_tx })
            .await
            .is_ok()
        {
            if let Ok(Some(snapshot)) = reply_rx.await {
                persisted_sessions.push(snapshot);
            }
        }
    }
    let tree = session_tree.read().await.snapshot();
    CoreCheckpoint::new(persisted_sessions, tree)
}

async fn snapshot_registry_session(
    registry: &SessionRegistry,
    session_tree: &SharedSessionTree,
    session_id: SessionId,
) -> CoreCheckpoint {
    let handle = registry.read().await.get(&session_id).cloned();
    let mut persisted_sessions = Vec::new();
    if let Some(handle) = handle {
        let (reply_tx, reply_rx) = oneshot::channel();
        if handle
            .command_tx
            .send(SessionCommand::Snapshot { reply: reply_tx })
            .await
            .is_ok()
        {
            if let Ok(Some(snapshot)) = reply_rx.await {
                persisted_sessions.push(snapshot);
            }
        }
    }
    let tree = session_tree.read().await.snapshot();
    CoreCheckpoint::new(persisted_sessions, tree)
}

async fn emit_checkpoint_request_for_registry(
    registry: &SessionRegistry,
    session_tree: &SharedSessionTree,
    output: &mpsc::Sender<CoreOutput>,
) {
    let checkpoint = snapshot_registry_sessions(registry, session_tree).await;
    let _ = output
        .send(CoreOutput::CheckpointRequested { checkpoint })
        .await;
}

#[derive(Clone)]
struct SessionActorSpawner {
    registry: SessionRegistry,
    session_tree: SharedSessionTree,
    output: mpsc::Sender<CoreOutput>,
    core_input_tx: mpsc::Sender<CoreInput>,
    runtime_event_tx: mpsc::Sender<RuntimeEvent>,
    web_provider: Arc<dyn WebProvider>,
}

async fn spawn_session_actor(
    spawner: &SessionActorSpawner,
    session_id: SessionId,
    session: SessionContext,
    parent_id: Option<SessionId>,
) -> Result<(), String> {
    let (command_tx, command_rx) = mpsc::channel(256);
    {
        let mut guard = spawner.registry.write().await;
        if guard.contains_key(&session_id) {
            return Err("session already exists".into());
        }
        guard.insert(
            session_id,
            SessionHandle {
                command_tx: command_tx.clone(),
                provider: Arc::clone(&session.provider),
                max_context_window: session.max_context_window,
                model_profile: session.persisted_config.model_profile.clone(),
                session_group: session.persisted_config.session_group.clone(),
            },
        );
    }

    let actor = SessionActor {
        session_id,
        parent_id,
        session,
        command_rx,
        deferred_commands: VecDeque::new(),
        output: spawner.output.clone(),
        core_input_tx: spawner.core_input_tx.clone(),
        runtime_event_tx: spawner.runtime_event_tx.clone(),
        session_tree: Arc::clone(&spawner.session_tree),
        web_provider: Arc::clone(&spawner.web_provider),
    };
    tokio::spawn(actor.run());
    Ok(())
}

/// Run the core event loop, processing inputs and emitting outputs.
///
/// The `provider` is used to send conversation history to the LLM and
/// stream back responses. Tools are executed directly within the core.
pub async fn run_core_loop(
    handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    restored_checkpoint: Option<CoreCheckpoint>,
) {
    run_core_loop_with_compaction_and_web_provider_and_python_runtime(
        handle,
        provider,
        Arc::new(NoopWebProvider),
        restored_checkpoint,
        std::env::temp_dir().join("quine-core-compactions"),
        None,
        PythonRuntime::new(),
    )
    .await;
}

pub async fn run_core_loop_with_compaction(
    handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    restored_checkpoint: Option<CoreCheckpoint>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
) {
    run_core_loop_with_compaction_and_web_provider_and_python_runtime(
        handle,
        provider,
        Arc::new(NoopWebProvider),
        restored_checkpoint,
        archive_root,
        max_context_window,
        PythonRuntime::new(),
    )
    .await;
}

pub async fn run_core_loop_with_compaction_and_web_provider(
    handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    web_provider: Arc<dyn WebProvider>,
    restored_checkpoint: Option<CoreCheckpoint>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
) {
    run_core_loop_with_compaction_and_web_provider_and_python_runtime(
        handle,
        provider,
        web_provider,
        restored_checkpoint,
        archive_root,
        max_context_window,
        PythonRuntime::new(),
    )
    .await;
}

pub async fn run_core_loop_with_compaction_and_web_provider_and_python_runtime(
    mut handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    web_provider: Arc<dyn WebProvider>,
    restored_checkpoint: Option<CoreCheckpoint>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
    python_runtime: Arc<PythonRuntime>,
) {
    run_core_loop_with_compaction_and_wait_notifier(
        &mut handle,
        provider,
        web_provider,
        restored_checkpoint,
        archive_root,
        max_context_window,
        python_runtime,
    )
    .await;
}

async fn run_core_loop_with_compaction_and_wait_notifier(
    handle: &mut CoreHandle,
    provider: Arc<dyn LlmProvider>,
    web_provider: Arc<dyn WebProvider>,
    restored_checkpoint: Option<CoreCheckpoint>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
    python_runtime: Arc<PythonRuntime>,
) {
    let restored_tree = restored_checkpoint
        .as_ref()
        .map(|checkpoint| SessionTree::restore(checkpoint.session_tree.clone()))
        .unwrap_or_default();
    let registry: SessionRegistry = Arc::new(RwLock::new(HashMap::new()));
    let session_tree: SharedSessionTree = Arc::new(RwLock::new(restored_tree));
    let (runtime_event_tx, mut runtime_event_rx) = mpsc::channel(256);
    let spawner = SessionActorSpawner {
        registry: Arc::clone(&registry),
        session_tree: Arc::clone(&session_tree),
        output: handle.output.clone(),
        core_input_tx: handle.input_tx.clone(),
        runtime_event_tx: runtime_event_tx.clone(),
        web_provider: Arc::clone(&web_provider),
    };
    let (scheduler_handle, scheduler_task) = spawn_scheduler(handle.input_tx.clone());
    debug_log("core event loop started");

    if let Some(checkpoint) = restored_checkpoint {
        for persisted_session in checkpoint.sessions {
            match SessionContext::from_persisted_with_web_provider(
                persisted_session,
                &provider,
                &web_provider,
                archive_root.clone(),
                max_context_window,
                Arc::clone(&python_runtime),
            )
            .await
            {
                Ok((session_id, session)) => {
                    let state = session.state;
                    let parent_id = session_tree.read().await.parent_of(session_id);
                    if spawn_session_actor(&spawner, session_id, session, parent_id)
                        .await
                        .is_ok()
                    {
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged { session_id, state })
                            .await;
                    }
                }
                Err(error) => {
                    debug_log(format!(
                        "failed to restore session from checkpoint: {error}"
                    ));
                }
            }
        }
    }

    loop {
        tokio::select! {
            maybe_event = runtime_event_rx.recv() => {
                match maybe_event {
                    Some(RuntimeEvent::ChildSessionFinished { session_id, parent_id, status }) => {
                        registry.write().await.remove(&session_id);
                        session_tree.write().await.record_exit(session_id, status.clone());
                        let _ = handle.output.send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Destroyed,
                        }).await;
                        let _ = handle.output.send(CoreOutput::ChildExited {
                            parent_id,
                            child_id: session_id,
                            status: status.clone(),
                        }).await;
                        let handles: Vec<SessionHandle> = registry.read().await.values().cloned().collect();
                        for session_handle in handles {
                            let _ = session_handle.command_tx.send(SessionCommand::ChildExited {
                                child_id: session_id,
                                status: status.clone(),
                            }).await;
                        }
                        emit_checkpoint_request_for_registry(&registry, &session_tree, &handle.output).await;
                    }
                    Some(RuntimeEvent::CheckpointHint) => {
                        emit_checkpoint_request_for_registry(&registry, &session_tree, &handle.output).await;
                    }
                    None => {}
                }
            }
            maybe_input = handle.input.recv() => {
                let Some(input) = maybe_input else {
                    break;
                };
                match input {
                    CoreInput::CreateSession {
                        session_id,
                        system_prompt,
                        working_directory,
                        skills,
                        plan_mode,
                        prompt_behavior,
                        initial_messages,
                        agent_key,
                        team_key,
                        session_group,
                        memory_policy,
                        session_llm,
                        auto_compact_threshold_percent,
                        status_report_min_tool_rounds,
                        permission_rules,
                        reply,
                    } => {
                        if registry.read().await.contains_key(&session_id) {
                            let _ = reply.send(Err("session already exists".into()));
                            continue;
                        }
                        let work_dir = working_directory
                            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                        match SessionContext::new_with_web_provider(
                            session_id,
                            SessionInit {
                                system_prompt,
                                skills,
                                working_directory: work_dir,
                                plan_mode,
                                prompt_behavior,
                                initial_messages,
                                archive_root: archive_root.clone(),
                                max_context_window: session_llm.max_context_window,
                                prompt_memory_mode: prompt_memory_mode_from_env(),
                                agent_key,
                                team_key,
                                memory_policy,
                                model_profile: session_llm.model_profile.clone(),
                                auto_compact_threshold_percent,
                                status_report_min_tool_rounds,
                                session_group,
                            },
                            &session_llm.provider,
                            &web_provider,
                            Arc::clone(&python_runtime),
                        )
                        .await
                        {
                            Ok(mut ctx) => {
                                ctx.permission_context.set_rules(permission_rules);
                                match spawn_session_actor(
                                    &spawner,
                                    session_id,
                                    ctx,
                                    None,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        let _ = handle.output.send(CoreOutput::SessionStateChanged {
                                            session_id,
                                            state: SessionState::Idle,
                                        }).await;
                                        emit_checkpoint_request_for_registry(&registry, &session_tree, &handle.output).await;
                                        let _ = reply.send(Ok(()));
                                    }
                                    Err(error) => {
                                        let _ = reply.send(Err(error));
                                    }
                                }
                            }
                            Err(error) => {
                                let _ = reply.send(Err(format!("failed to create session: {error}")));
                            }
                        }
                    }
                    CoreInput::ExitPlanMode { session_id, reply } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::ExitPlanMode { reply }).await;
                        } else {
                            let _ = reply.send(Err("unknown session".into()));
                        }
                    }
                    CoreInput::UpdateSessionLlm { session_id, session_llm, reply } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::UpdateSessionLlm {
                                session_llm,
                                reply,
                            }).await;
                        } else {
                            let _ = reply.send(Err("unknown session".into()));
                        }
                    }
                    CoreInput::UserMessage {
                        session_id,
                        content,
                        turn_id,
                    } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::UserMessage {
                                content,
                                turn_id,
                            }).await;
                        } else {
                            let _ = handle.output.send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::SessionNotFound,
                            }).await;
                        }
                    }
                    CoreInput::ScheduleUserMessage {
                        session_id,
                        content,
                        delay,
                        cadence,
                    } => {
                        if scheduler_handle
                            .schedule_user_message(session_id, content, delay, cadence)
                            .await
                            .is_err()
                        {
                            let _ = handle.output.send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::Internal {
                                    message: "scheduler unavailable".into(),
                                },
                            }).await;
                        }
                    }
                    CoreInput::CompactSession { session_id, reply } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::CompactSession { reply }).await;
                        } else {
                            let _ = reply.send(Err("session not found".into()));
                        }
                    }
                    CoreInput::ToolResult { session_id, tool_use_id, result } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::ToolResult {
                                tool_use_id,
                                result,
                            }).await;
                        } else {
                            let _ = handle.output.send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::SessionNotFound,
                            }).await;
                        }
                    }
                    CoreInput::InteractionResponse { session_id, response } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::InteractionResponse(response)).await;
                        }
                    }
                    CoreInput::Cancel { session_id } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::Cancel).await;
                        }
                    }
                    CoreInput::SessionMemoryRefreshFinished {
                        session_id,
                        last_summarized_message_index,
                        refreshed_at,
                        listing_summary,
                    } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::SessionMemoryRefreshFinished {
                                last_summarized_message_index,
                                refreshed_at,
                                listing_summary,
                            }).await;
                        }
                    }
                    CoreInput::Shutdown => {
                        let handles: Vec<SessionHandle> = registry.read().await.values().cloned().collect();
                        for session_handle in handles {
                            let _ = session_handle.command_tx.send(SessionCommand::Shutdown).await;
                        }
                        break;
                    }
                    CoreInput::SpawnSession {
                        parent_id,
                        child_id,
                        task,
                        system_prompt,
                        prompt_behavior,
                        permission_rules,
                        inheritance: _inheritance,
                        reply,
                    } => {
                        if registry.read().await.contains_key(&child_id) {
                            let _ = reply.send(Err("session already exists".into()));
                            continue;
                        }
                        let parent_handle = registry.read().await.get(&parent_id).cloned();
                        let inherited_provider = parent_handle
                            .as_ref()
                            .map(|handle| Arc::clone(&handle.provider))
                            .unwrap_or_else(|| Arc::clone(&provider));
                        let inherited_model_profile = parent_handle
                            .as_ref()
                            .and_then(|handle| handle.model_profile.clone());
                        let inherited_session_group = parent_handle
                            .as_ref()
                            .and_then(|handle| handle.session_group.clone());
                        let inherited_max_context_window = parent_handle
                            .as_ref()
                            .and_then(|handle| handle.max_context_window)
                            .or(max_context_window);
                        match SessionContext::new(
                            child_id,
                            SessionInit {
                                system_prompt,
                                skills: Vec::new(),
                                working_directory: std::env::current_dir().unwrap_or_default(),
                                plan_mode: false,
                                prompt_behavior,
                                initial_messages: Vec::new(),
                                archive_root: archive_root.clone(),
                                max_context_window: inherited_max_context_window,
                                prompt_memory_mode: PromptMemoryMode::Disabled,
                                agent_key: None,
                                team_key: None,
                                memory_policy: MemoryPolicyConfig::default(),
                                model_profile: inherited_model_profile,
                                session_group: inherited_session_group.clone(),
                                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                            },
                            &inherited_provider,
                        )
                        .await
                        {
                            Ok(mut ctx) => {
                                ctx.python_runtime = Arc::clone(&python_runtime);
                                ctx.python_group =
                                    effective_session_group(child_id, inherited_session_group.as_deref());
                                ctx.persisted_config.session_group = inherited_session_group;
                                ctx.permission_context.set_rules(permission_rules);
                                session_tree.write().await.add_child(parent_id, child_id);
                                match spawn_session_actor(
                                    &spawner,
                                    child_id,
                                    ctx,
                                    Some(parent_id),
                                )
                                .await
                                {
                                    Ok(()) => {
                                        let _ = handle.output.send(CoreOutput::ChildSpawned { parent_id, child_id }).await;
                                        let _ = handle.output.send(CoreOutput::SessionStateChanged {
                                            session_id: child_id,
                                            state: SessionState::Idle,
                                        }).await;
                                        if let Some(child_handle) = registry.read().await.get(&child_id).cloned() {
                                            let _ = child_handle.command_tx.send(SessionCommand::UserMessage {
                                                content: task,
                                                turn_id: uuid::Uuid::new_v4().to_string(),
                                            }).await;
                                        }
                                        emit_checkpoint_request_for_registry(&registry, &session_tree, &handle.output).await;
                                        let _ = reply.send(Ok(()));
                                    }
                                    Err(error) => {
                                        let _ = reply.send(Err(error));
                                    }
                                }
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error.to_string()));
                            }
                        }
                    }
                    CoreInput::Signal { session_id, signal } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::Signal(signal)).await;
                        }
                    }
                    CoreInput::SendMessage { from, to, content } => {
                        if let Some(session_handle) = registry.read().await.get(&to).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::MailboxMessage(
                                MailboxMessage { from, content: content.clone() }
                            )).await;
                            let _ = handle.output.send(CoreOutput::MessageReceived {
                                session_id: to,
                                from,
                                content,
                            }).await;
                        }
                    }
                    CoreInput::RecvMessage { session_id, source, non_blocking: _, timeout: _, reply } => {
                        if let Some(session_handle) = registry.read().await.get(&session_id).cloned() {
                            let _ = session_handle.command_tx.send(SessionCommand::QueryMailbox {
                                source,
                                reply,
                            }).await;
                        } else {
                            let _ = reply.send(None);
                        }
                    }
                    CoreInput::SendHarnessIpcMessage { target, content, reply } => {
                        let result = scheduler_handle
                            .send_ipc_message(target, content)
                            .await
                            .map_err(|error| error.to_string());
                        let _ = reply.send(result);
                    }
                    CoreInput::RecvHarnessIpcMessage { source, non_blocking, reply } => {
                        let result = scheduler_handle
                            .recv_ipc_message(source, non_blocking)
                            .await
                            .ok()
                            .flatten();
                        let _ = reply.send(result);
                    }
                    CoreInput::RequestCheckpoint { reply } => {
                        let checkpoint = snapshot_registry_sessions(&registry, &session_tree).await;
                        let _ = reply.send(checkpoint);
                    }
                    CoreInput::RequestSessionCheckpoint { session_id, reply } => {
                        let checkpoint =
                            snapshot_registry_session(&registry, &session_tree, session_id).await;
                        let _ = reply.send(checkpoint);
                    }
                    CoreInput::WaitSession { parent_id, child_id, reply, non_blocking, timeout: _ } => {
                        let tree = session_tree.read().await;
                        let result = if tree.parent_of(child_id) == Some(parent_id) {
                            tree.exit_status(child_id).cloned()
                        } else {
                            None
                        };
                        drop(tree);
                        if let Some(status) = result {
                            let _ = reply.send(Ok(Some(status)));
                        } else if non_blocking {
                            let _ = reply.send(Ok(None));
                        } else {
                            let (wait_tx, wait_rx) = oneshot::channel();
                            let already_exited = session_tree.write().await.register_waiter(child_id, wait_tx);
                            if already_exited {
                                let status = session_tree.read().await.exit_status(child_id).cloned();
                                let _ = reply.send(Ok(status));
                            } else {
                                tokio::spawn(async move {
                                    let response = wait_rx.await
                                        .map(Some)
                                        .map_err(|_| "wait reply channel dropped".to_string());
                                    let _ = reply.send(response);
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    let _ = scheduler_handle.shutdown().await;
    let _ = scheduler_task.await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{create_channels, ChannelConfig};
    use crate::permission::types::PermissionMode;
    use crate::session::{ExitStatus, InheritanceFlags};
    use crate::SessionLlmConfig;
    use std::collections::VecDeque;
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
    use std::sync::Mutex;
    use tempfile::TempDir;
    use tokio::sync::{oneshot, Barrier, Notify};
    use tokio::time::Duration as TokioDuration;

    use crate::memory::load_summary_metadata;
    use sha2::{Digest, Sha256};

    /// A mock LLM provider that returns a fixed text response.
    struct MockProvider {
        response_text: String,
    }

    impl MockProvider {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
            }
        }

        fn empty() -> Self {
            Self::new("")
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for MockProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = self.response_text.clone();
            let events = if text.is_empty() {
                vec![Ok(LlmEvent::Done { usage: None })]
            } else {
                vec![
                    Ok(LlmEvent::TextDelta { text }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    struct FailingStreamProvider;

    #[async_trait::async_trait]
    impl LlmProvider for FailingStreamProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::ReasoningDelta {
                    text: "thinking".into(),
                }),
                Err(anyhow::anyhow!(
                    "openai_compat stream read failed: error decoding response body"
                )),
            ])))
        }
    }

    struct CountingProvider {
        response_text: String,
        calls: AtomicUsize,
    }

    impl CountingProvider {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for CountingProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let text = self.response_text.clone();
            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ])))
        }
    }

    struct RejectZombieToolUseProvider {
        forbidden_tool_use_id: String,
    }

    #[async_trait::async_trait]
    impl LlmProvider for RejectZombieToolUseProvider {
        async fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            for message in messages {
                if let MessageContent::ToolUse { tool_calls, .. } = &message.content {
                    if tool_calls
                        .iter()
                        .any(|call| call.tool_use_id == self.forbidden_tool_use_id)
                    {
                        anyhow::bail!("zombie tool call survived restore sanitation");
                    }
                }
            }

            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::TextDelta {
                    text: "restored reply".into(),
                }),
                Ok(LlmEvent::Done { usage: None }),
            ])))
        }
    }

    struct SequenceProvider {
        responses: Mutex<VecDeque<String>>,
    }

    impl SequenceProvider {
        fn new(responses: impl IntoIterator<Item = impl Into<String>>) -> Self {
            Self {
                responses: Mutex::new(
                    responses
                        .into_iter()
                        .map(Into::into)
                        .collect::<VecDeque<_>>(),
                ),
            }
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for SequenceProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_default();
            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ])))
        }
    }

    struct ConcurrentSessionProvider {
        response_text: String,
        started: AtomicUsize,
        started_notify: Notify,
        released: AtomicBool,
        release_notify: Notify,
    }

    impl ConcurrentSessionProvider {
        fn new(text: impl Into<String>) -> Self {
            Self {
                response_text: text.into(),
                started: AtomicUsize::new(0),
                started_notify: Notify::new(),
                released: AtomicBool::new(false),
                release_notify: Notify::new(),
            }
        }

        async fn wait_until_started(&self, expected: usize) {
            tokio::time::timeout(TokioDuration::from_secs(1), async {
                loop {
                    if self.started.load(Ordering::SeqCst) >= expected {
                        break;
                    }
                    self.started_notify.notified().await;
                }
            })
            .await
            .expect("sessions never entered the provider concurrently");
        }

        fn release(&self) {
            self.released.store(true, Ordering::SeqCst);
            self.release_notify.notify_waiters();
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for ConcurrentSessionProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.started_notify.notify_waiters();

            tokio::time::timeout(TokioDuration::from_secs(1), async {
                loop {
                    if self.started.load(Ordering::SeqCst) >= 2 {
                        break;
                    }
                    self.started_notify.notified().await;
                }
            })
            .await
            .map_err(|_| anyhow::anyhow!("second session never reached the provider"))?;

            loop {
                if self.released.load(Ordering::SeqCst) {
                    break;
                }
                self.release_notify.notified().await;
            }

            let text = self.response_text.clone();
            Ok(Box::pin(futures::stream::iter([
                Ok(LlmEvent::TextDelta { text }),
                Ok(LlmEvent::Done { usage: None }),
            ])))
        }
    }

    struct QueuedUserMessageProvider {
        first_turn_released: Arc<AtomicBool>,
        first_turn_release_notify: Arc<Notify>,
    }

    impl QueuedUserMessageProvider {
        fn new() -> Self {
            Self {
                first_turn_released: Arc::new(AtomicBool::new(false)),
                first_turn_release_notify: Arc::new(Notify::new()),
            }
        }

        fn release_first_turn(&self) {
            self.first_turn_released.store(true, Ordering::SeqCst);
            self.first_turn_release_notify.notify_waiters();
        }
    }

    #[async_trait::async_trait]
    impl LlmProvider for QueuedUserMessageProvider {
        async fn send(
            &self,
            messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            match latest_user_request_text(messages) {
                Some("first") => {
                    let released = Arc::clone(&self.first_turn_released);
                    let release_notify = Arc::clone(&self.first_turn_release_notify);
                    Ok(Box::pin(futures::stream::unfold(0_u8, move |step| {
                        let released = Arc::clone(&released);
                        let release_notify = Arc::clone(&release_notify);
                        async move {
                            match step {
                                0 => Some((
                                    Ok(LlmEvent::TextDelta {
                                        text: "first reply".into(),
                                    }),
                                    1,
                                )),
                                1 => {
                                    loop {
                                        if released.load(Ordering::SeqCst) {
                                            break;
                                        }
                                        release_notify.notified().await;
                                    }
                                    Some((Ok(LlmEvent::Done { usage: None }), 2))
                                }
                                _ => None,
                            }
                        }
                    })))
                }
                Some("second") => Ok(Box::pin(futures::stream::iter([
                    Ok(LlmEvent::TextDelta {
                        text: "second reply".into(),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]))),
                _ => Ok(Box::pin(futures::stream::iter([Ok(LlmEvent::Done {
                    usage: None,
                })]))),
            }
        }
    }

    fn session_llm_config(provider: Arc<dyn LlmProvider>) -> SessionLlmConfig {
        SessionLlmConfig {
            provider,
            max_context_window: None,
            model_profile: None,
        }
    }

    async fn wait_for_test_files(paths: &[&std::path::Path], description: &str) {
        let deadline = tokio::time::Instant::now() + TokioDuration::from_secs(5);
        loop {
            let missing = paths
                .iter()
                .filter(|path| !path.exists())
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>();
            if missing.is_empty() {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {description}; missing: {}",
                missing.join(", ")
            );
            tokio::time::sleep(TokioDuration::from_millis(50)).await;
        }
    }

    async fn wait_for_summary_metadata_index(
        path: &std::path::Path,
        minimum_index: usize,
        description: &str,
    ) {
        let deadline = tokio::time::Instant::now() + TokioDuration::from_secs(5);
        let mut last_observed = None;
        loop {
            if let Ok(metadata) = load_summary_metadata(path) {
                last_observed = Some(metadata.last_summarized_message_index);
                if metadata.last_summarized_message_index >= minimum_index {
                    return;
                }
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for {description}; last observed metadata index: {last_observed:?}"
            );
            tokio::time::sleep(TokioDuration::from_millis(50)).await;
        }
    }

    #[tokio::test]
    async fn session_context_bootstraps_permission_foundation_from_plan_mode() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let temp_dir = TempDir::new().unwrap();
        let working_directory = temp_dir.path().to_path_buf();
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: working_directory.clone(),
                plan_mode: true,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: temp_dir.path().join("archive"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        assert_eq!(session.permission_context.mode(), PermissionMode::Plan);
        assert_eq!(
            session.permission_context.pre_plan_mode(),
            Some(PermissionMode::Default)
        );
        assert_eq!(
            session.permission_context.workspace_root(),
            &working_directory
        );
        assert!(session
            .permission_context
            .additional_allowed_roots()
            .is_empty());
    }

    #[tokio::test]
    async fn exit_plan_mode_restores_default_permission_mode() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let temp_dir = TempDir::new().unwrap();
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: temp_dir.path().to_path_buf(),
                plan_mode: true,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: temp_dir.path().join("archive"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        session.exit_plan_mode(&provider).await.unwrap();

        assert!(!session.persisted_config.plan_mode);
        assert_eq!(session.permission_context.mode(), PermissionMode::Default);
        assert_eq!(session.permission_context.pre_plan_mode(), None);
    }

    #[tokio::test]
    async fn session_context_bootstraps_explicit_headless_prompt_behavior() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let temp_dir = TempDir::new().unwrap();
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: temp_dir.path().to_path_buf(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Headless,
                initial_messages: Vec::new(),
                archive_root: temp_dir.path().join("archive"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        assert_eq!(
            session.permission_context.prompt_behavior(),
            PermissionPromptBehavior::Headless
        );
        assert_eq!(
            session.persisted_config.prompt_behavior,
            PermissionPromptBehavior::Headless
        );
    }

    fn compacted_summary_text(history: &[Message]) -> &str {
        match history.get(1).map(|message| &message.content) {
            Some(MessageContent::Text(text)) => text,
            other => panic!("expected compacted assistant summary, got {other:?}"),
        }
    }

    async fn make_session_for_compaction(
        provider: &Arc<dyn LlmProvider>,
        archive_root: PathBuf,
        history: Vec<Message>,
    ) -> (SessionId, SessionContext) {
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root,
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            provider,
        )
        .await
        .unwrap();
        session.history = history;
        (session_id, session)
    }

    fn write_persistent_memory_fixture(
        archive_root: &std::path::Path,
        working_directory: &std::path::Path,
        memory_md: &str,
        index_json: &str,
        entries: &[(&str, &str)],
    ) {
        let normalized = working_directory.to_string_lossy().replace('\\', "/");
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        let digest = hasher.finalize();
        let project_key = hex::encode(&digest[..16]);
        let project_dir = archive_root
            .join("memory")
            .join("projects")
            .join(project_key);
        std::fs::create_dir_all(project_dir.join("entries")).unwrap();
        std::fs::write(project_dir.join("MEMORY.md"), memory_md).unwrap();
        std::fs::write(project_dir.join("index.json"), index_json).unwrap();
        for (relative_path, content) in entries {
            let path = project_dir.join(relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, content).unwrap();
        }
    }

    fn write_session_memory_fixture(
        session: &SessionContext,
        summary: &str,
        last_summarized_message_index: usize,
    ) {
        std::fs::create_dir_all(&session.session_memory.paths.directory).unwrap();
        std::fs::write(&session.session_memory.paths.summary_path, summary).unwrap();
        std::fs::write(
            &session.session_memory.paths.metadata_path,
            serde_json::to_string(&crate::memory::SessionSummaryMetadata {
                last_summarized_message_index,
                updated_at: Utc::now(),
                template_version: session.session_memory.template_version,
            })
            .unwrap(),
        )
        .unwrap();
    }

    #[tokio::test]
    async fn build_provider_messages_injects_targeted_prompt_memory_without_persisting_it() {
        let tempdir = TempDir::new().unwrap();
        let archive_root = tempdir.path().join("archive");
        let working_directory = tempdir.path().join("workspace");
        std::fs::create_dir_all(working_directory.join(".git")).unwrap();

        let index_json = r#"{
  "entries": [
    {
      "entry_id": "cargo-test",
      "title": "Cargo test command",
      "summary": "Use cargo test for the Rust test suite",
      "slug": "cargo-test-command",
      "path": "entries/cargo-test.md",
      "updated_at": "2025-01-01T00:00:00Z",
      "keywords": ["cargo", "test", "rust"],
      "pinned": true
    }
  ]
}"#;
        let entry = r#"---
entry_id: cargo-test
title: Cargo test command
summary: Use cargo test for the Rust test suite
keywords:
  - cargo
  - test
  - rust
updated_at: 2025-01-01T00:00:00Z
pinned: true
---
Run `cargo test` from the workspace root to execute the Rust test suite.
"#;
        write_persistent_memory_fixture(
            &archive_root,
            &working_directory,
            "# Durable Memory\n\n- Use cargo test",
            index_json,
            &[("entries/cargo-test.md", entry)],
        );

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: Some("base system prompt".into()),
                skills: Vec::new(),
                working_directory: working_directory.clone(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::user("How do I run the Rust test suite?")],
                archive_root,
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.persisted_config.prompt_memory_mode = PromptMemoryMode::TargetedRecall;

        let provider_messages = build_provider_messages(&mut session).await.unwrap();
        assert_eq!(
            session.last_prompt_memory.mode,
            PromptMemoryMode::TargetedRecall
        );
        assert_eq!(
            session.last_prompt_memory.selected_entry_ids,
            vec!["cargo-test"]
        );
        assert_eq!(session.history.len(), 2);
        assert_eq!(provider_messages.len(), 3);
        assert_eq!(provider_messages[1].role, quine_llm::Role::System);
        let injected_text = match &provider_messages[1].content {
            MessageContent::Text(text) => text,
            other => panic!("expected text message, got {other:?}"),
        };
        assert!(injected_text.contains("Relevant durable memory `cargo-test`:"));
        assert!(injected_text.contains("cargo test"));
        assert_eq!(provider_messages[2].role, quine_llm::Role::User);
        match &provider_messages[2].content {
            MessageContent::Text(text) => assert_eq!(text, "How do I run the Rust test suite?"),
            other => panic!("expected text message, got {other:?}"),
        }
        assert!(session
            .history
            .iter()
            .all(|message| match &message.content {
                MessageContent::Text(text) => !text.contains("Relevant durable memory"),
                _ => true,
            }));
    }

    #[tokio::test]
    async fn system_prompt_includes_default_tools_and_claude() {
        let tempdir = TempDir::new().unwrap();
        let working_directory = tempdir.path().join("workspace");
        std::fs::create_dir_all(&working_directory).unwrap();
        std::fs::write(
            tempdir.path().join("CLAUDE.md"),
            "# Project Rules\n\nUse cargo test before merging.",
        )
        .unwrap();

        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory,
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: tempdir.path().join("archive"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let prompt = match &session.history[0].content {
            MessageContent::Text(text) => text,
            other => panic!("expected system prompt text, got {other:?}"),
        };
        assert!(prompt.contains("You are a helpful coding assistant."));
        assert!(prompt.contains("## Available Tools"));
        assert!(prompt.contains("`read_file` (read-only, idempotent)"));
        assert!(prompt.contains("# Project Instructions (from CLAUDE.md)"));
        assert!(prompt.contains("Use cargo test before merging."));
    }

    #[test]
    fn sanitize_restored_history_removes_zombie_tool_calls_and_orphan_results() {
        let sanitized = sanitize_restored_history(
            SessionId::new(),
            vec![
                Message::system("system"),
                Message::user("before"),
                Message::assistant_tool_use(
                    Some("trying tool".into()),
                    vec![quine_llm::ToolUseRequest {
                        tool_use_id: "call_zombie".into(),
                        tool_name: "read_file".into(),
                        arguments: serde_json::json!({"path": "Cargo.toml"}),
                    }],
                ),
                Message::tool_result("call_orphan", "never requested", false),
            ],
        );

        assert_eq!(sanitized.len(), 3);
        match &sanitized[2].content {
            MessageContent::Text(text) => assert_eq!(text, "trying tool"),
            other => panic!("expected assistant text after zombie tool cleanup, got {other:?}"),
        }
    }

    #[test]
    fn estimate_prompt_cache_usage_uses_shared_prefix() {
        let previous = vec![
            "system".into(),
            "alpha".into(),
            "beta".into(),
            "gamma".into(),
        ];
        let current = vec![
            "system".into(),
            "alpha".into(),
            "beta".into(),
            "delta".into(),
            "epsilon".into(),
        ];

        let usage = estimate_prompt_cache_usage(Some(&previous), &current);
        assert_eq!(usage.estimated_hit_tokens, 3);
        assert_eq!(usage.estimated_miss_tokens, 2);
    }

    #[tokio::test]
    async fn valid_session_memory_compaction_uses_summary_md() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(CountingProvider::new("legacy summary"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::user("keep this"),
            Message::assistant("latest state"),
        ];
        let (session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history).await;
        session.session_memory.last_summarized_message_index = Some(2);
        write_session_memory_fixture(
            &session,
            "## Current State\n\n- Memory summary from summary.md\n",
            2,
        );

        let compacted = compact_session_history(
            provider_dyn.as_ref(),
            &mut session,
            session_id,
            CompactionTrigger::Manual,
        )
        .await
        .unwrap();

        assert!(compacted);
        assert_eq!(provider.call_count(), 0);
        let summary = compacted_summary_text(&session.history);
        assert!(summary.contains("Memory summary from summary.md"));
        assert!(summary.contains("Context compacted from archive"));
        assert_eq!(session.history.len(), 4);
        let diagnostics = session
            .last_memory_diagnostics
            .as_ref()
            .expect("compaction diagnostics should be recorded");
        assert_eq!(
            diagnostics.session_memory.compaction.status,
            MemoryStatus::Succeeded
        );
        assert_eq!(
            diagnostics.session_memory.compaction.source,
            Some(CompactionSourceDiagnostics::SessionMemory)
        );
        let archive_dir = temp
            .path()
            .join("compactions")
            .join(session_id_string(session_id));
        assert!(archive_dir.join("0001.json").exists());
    }

    #[tokio::test]
    async fn invalid_session_memory_compaction_falls_back() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(CountingProvider::new("legacy summary"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::user("live tail"),
        ];
        let (session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history).await;
        session.session_memory.last_summarized_message_index = Some(3);
        write_session_memory_fixture(&session, "## Current State\n\n- Stale summary\n", 3);

        compact_session_history(
            provider_dyn.as_ref(),
            &mut session,
            session_id,
            CompactionTrigger::Manual,
        )
        .await
        .unwrap();

        assert_eq!(provider.call_count(), 1);
        let summary = compacted_summary_text(&session.history);
        assert!(summary.contains("legacy summary"));
        let diagnostics = session
            .last_memory_diagnostics
            .as_ref()
            .expect("fallback diagnostics should be recorded");
        assert_eq!(
            diagnostics.session_memory.compaction.status,
            MemoryStatus::Skipped
        );
        assert_eq!(
            diagnostics.session_memory.compaction.source,
            Some(CompactionSourceDiagnostics::LegacySummarizer)
        );
        assert_eq!(
            diagnostics.session_memory.compaction.reason,
            Some(MemoryDecisionReason::Fallback)
        );
    }

    #[tokio::test]
    async fn compaction_waits_for_refresh_when_join_is_available() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(CountingProvider::new("legacy summary"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::user("preserve after boundary"),
        ];
        let (session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history).await;
        session.session_memory.refresh_in_flight = true;

        let paths = session.session_memory.paths.clone();
        let template_version = session.session_memory.template_version;
        let refresh_handle = session.session_memory.refresh_handle.clone();
        let (lock_ready_tx, lock_ready_rx) = tokio::sync::oneshot::channel();
        let writer = tokio::spawn(async move {
            let _guard = refresh_handle.lock.lock().await;
            let _ = lock_ready_tx.send(());
            tokio::time::sleep(TokioDuration::from_millis(20)).await;
            std::fs::create_dir_all(&paths.directory).unwrap();
            std::fs::write(
                &paths.summary_path,
                "## Current State\n\n- Fresh session memory\n",
            )
            .unwrap();
            std::fs::write(
                &paths.metadata_path,
                serde_json::to_string(&crate::memory::SessionSummaryMetadata {
                    last_summarized_message_index: 2,
                    updated_at: Utc::now(),
                    template_version,
                })
                .unwrap(),
            )
            .unwrap();
        });
        lock_ready_rx.await.unwrap();

        compact_session_history(
            provider_dyn.as_ref(),
            &mut session,
            session_id,
            CompactionTrigger::Manual,
        )
        .await
        .unwrap();
        writer.await.unwrap();

        assert_eq!(provider.call_count(), 0);
        assert!(compacted_summary_text(&session.history).contains("Fresh session memory"));
    }

    #[tokio::test]
    async fn compaction_falls_back_when_refresh_snapshot_is_not_safely_available() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(CountingProvider::new("legacy summary"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("old request"),
            Message::assistant("old answer"),
            Message::user("preserve after boundary"),
        ];
        let (session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history).await;
        session.session_memory.refresh_in_flight = true;

        let refresh_handle = session.session_memory.refresh_handle.clone();
        let blocker = tokio::spawn(async move {
            let _guard = refresh_handle.lock.lock().await;
            tokio::time::sleep(TokioDuration::from_millis(400)).await;
        });

        compact_session_history(
            provider_dyn.as_ref(),
            &mut session,
            session_id,
            CompactionTrigger::Manual,
        )
        .await
        .unwrap();
        blocker.await.unwrap();

        assert_eq!(provider.call_count(), 1);
        assert!(compacted_summary_text(&session.history).contains("legacy summary"));
    }

    fn prompt_memory_project_dir(
        archive_root: &std::path::Path,
        working_directory: &std::path::Path,
    ) -> PathBuf {
        use sha2::{Digest, Sha256};

        let mut current = working_directory.to_path_buf();
        while !(current.join(".git").exists()
            || current.join("CLAUDE.md").exists()
            || current.join("Cargo.toml").exists())
        {
            if !current.pop() {
                current = working_directory.to_path_buf();
                break;
            }
        }
        let normalized = current.to_string_lossy().replace('\\', "/");
        let mut hasher = Sha256::new();
        hasher.update(normalized.as_bytes());
        let digest = hasher.finalize();
        archive_root
            .join("memory")
            .join("projects")
            .join(hex::encode(&digest[..16]))
    }

    fn write_prompt_memory_fixture(
        archive_root: &std::path::Path,
        working_directory: &std::path::Path,
        index_json: &serde_json::Value,
        entry_files: &[(&str, &str)],
        memory_md: &str,
    ) {
        let project_dir = prompt_memory_project_dir(archive_root, working_directory);
        std::fs::create_dir_all(project_dir.join("entries")).unwrap();
        std::fs::write(
            project_dir.join("index.json"),
            serde_json::to_vec_pretty(index_json).unwrap(),
        )
        .unwrap();
        std::fs::write(project_dir.join("MEMORY.md"), memory_md).unwrap();
        for (path, content) in entry_files {
            std::fs::write(project_dir.join(path), content).unwrap();
        }
    }

    #[tokio::test]
    async fn prompt_memory_disabled_mode_is_request_equivalent_to_legacy_path() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(CountingProvider::new("ok"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("hello"),
            Message::assistant("world"),
        ];
        let (_session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history.clone())
                .await;
        session.persisted_config.prompt_memory_mode = PromptMemoryMode::Disabled;

        let built = build_provider_messages(&mut session).await.unwrap();
        let expected = history.clone();
        assert_eq!(built.len(), expected.len());
        for (left, right) in built.iter().zip(expected.iter()) {
            assert_eq!(left.role, right.role);
            if let (MessageContent::Text(a), MessageContent::Text(b)) =
                (&left.content, &right.content)
            {
                assert_eq!(a, b);
            }
        }
        assert!(session.last_prompt_memory.selected_entry_ids.is_empty());
    }

    #[tokio::test]
    async fn targeted_recall_prompt_assembly_inserts_ephemeral_reminders_before_latest_user_message(
    ) {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("CLAUDE.md"), "# project\n").unwrap();

        let provider = Arc::new(CountingProvider::new("ok"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("earlier"),
            Message::assistant("reply"),
            Message::user("What should I run to execute all tests in this repo?"),
        ];
        let (_session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history.clone())
                .await;
        session.persisted_config.working_directory = project_dir.clone();
        session.persisted_config.prompt_memory_mode = PromptMemoryMode::TargetedRecall;
        write_prompt_memory_fixture(
            temp.path(),
            &project_dir,
            &serde_json::json!({
                "entries": [{
                    "entry_id": "rust-test-command",
                    "title": "Run tests",
                    "summary": "Use cargo test to run the Rust test suite",
                    "slug": "rust-test-command",
                    "path": "entries/rust-test-command.md",
                    "updated_at": Utc::now(),
                    "keywords": ["cargo", "test", "rust"],
                    "pinned": false
                }]
            }),
            &[(
                "entries/rust-test-command.md",
                "---\nentry_id: rust-test-command\ntitle: Run tests\nsummary: Use cargo test to run the Rust test suite\nkeywords:\n  - cargo\n  - test\nupdated_at: 2026-04-04T00:00:00Z\npinned: false\n---\n\nUse `cargo test` to run the Rust test suite.\n",
            )],
            "# MEMORY\n\n- [Run tests](entries/rust-test-command.md) - Use cargo test\n",
        );

        let built = build_provider_messages(&mut session).await.unwrap();
        assert_eq!(session.history.len(), history.len());
        match &session.history[3].content {
            MessageContent::Text(text) => {
                assert_eq!(text, "What should I run to execute all tests in this repo?")
            }
            other => panic!("expected latest user text, got {other:?}"),
        }
        assert_eq!(
            session.last_prompt_memory.selected_entry_ids,
            vec!["rust-test-command"]
        );
        let latest_user_index = built
            .iter()
            .rposition(|message| message.role == quine_llm::Role::User)
            .unwrap();
        assert!(latest_user_index > 0);
        match &built[latest_user_index - 1].content {
            MessageContent::Text(text) => assert!(text.contains("rust-test-command")),
            other => panic!("expected injected reminder, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn targeted_recall_prefers_agent_scope_when_configured_for_narrower_conflicts() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("CLAUDE.md"), "# project\n").unwrap();

        let provider = Arc::new(CountingProvider::new("ok"));
        let provider_dyn: Arc<dyn LlmProvider> = provider.clone();
        let history = vec![
            Message::system("system"),
            Message::user("What editor should I use for this task?"),
        ];
        let (_session_id, mut session) =
            make_session_for_compaction(&provider_dyn, temp.path().to_path_buf(), history).await;
        session.persisted_config.working_directory = project_dir.clone();
        session.persisted_config.prompt_memory_mode = PromptMemoryMode::TargetedRecall;
        session.persisted_config.agent_key = Some("planner".into());
        session.persisted_config.memory_policy = MemoryPolicyConfig {
            flags: crate::memory::MemoryFeatureFlags {
                advanced_scopes_enabled: true,
                agent_memory_enabled: true,
                ..crate::memory::MemoryFeatureFlags::default()
            },
            read_policy: crate::memory::MemoryReadPolicy {
                allow_cross_scope_recall: false,
                ..crate::memory::MemoryReadPolicy::default()
            },
            lookup_order: crate::memory::ScopedMemoryLookupOrder::ProjectThenAgent,
            conflict_resolution: crate::memory::MemoryConflictResolution::PreferNarrowerScope,
            ..MemoryPolicyConfig::default()
        };

        let project_root = temp
            .path()
            .join("memory")
            .join("projects")
            .join(crate::memory::project_key(&project_dir));
        let agent_root = temp
            .path()
            .join("memory")
            .join("agents")
            .join(crate::memory::project_key(&project_dir))
            .join("planner");
        std::fs::create_dir_all(project_root.join("entries")).unwrap();
        std::fs::create_dir_all(agent_root.join("entries")).unwrap();
        std::fs::write(
            project_root.join("index.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "entries": [{
                    "entry_id": "editor-style",
                    "title": "Preferred editor",
                    "summary": "Preferred editor: vim",
                    "slug": "editor-style",
                    "path": "entries/editor-style.md",
                    "updated_at": "2026-04-04T00:00:00Z",
                    "keywords": ["editor", "vim"],
                    "pinned": false
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            agent_root.join("index.json"),
            serde_json::to_vec_pretty(&serde_json::json!({
                "entries": [{
                    "entry_id": "editor-style",
                    "title": "Preferred editor",
                    "summary": "Preferred editor: helix",
                    "slug": "editor-style",
                    "path": "entries/editor-style.md",
                    "updated_at": "2026-04-04T00:00:00Z",
                    "keywords": ["editor", "helix"],
                    "pinned": false
                }]
            }))
            .unwrap(),
        )
        .unwrap();
        std::fs::write(
            project_root.join("entries/editor-style.md"),
            "---\nentry_id: editor-style\ntitle: Preferred editor\nsummary: \"Preferred editor: vim\"\nkeywords:\n  - editor\n  - vim\nupdated_at: 2026-04-04T00:00:00Z\npinned: false\n---\n\nPreferred editor: vim\n",
        )
        .unwrap();
        std::fs::write(
            agent_root.join("entries/editor-style.md"),
            "---\nentry_id: editor-style\ntitle: Preferred editor\nsummary: \"Preferred editor: helix\"\nkeywords:\n  - editor\n  - helix\nupdated_at: 2026-04-04T00:00:00Z\npinned: false\n---\n\nPreferred editor: helix\n",
        )
        .unwrap();

        let built = build_provider_messages(&mut session).await.unwrap();
        assert_eq!(
            session.last_prompt_memory.selected_entry_ids,
            vec!["editor-style"]
        );
        let latest_user_index = built
            .iter()
            .rposition(|message| message.role == quine_llm::Role::User)
            .unwrap();
        match &built[latest_user_index - 1].content {
            MessageContent::Text(text) => assert!(text.contains("helix")),
            other => panic!("expected injected reminder, got {other:?}"),
        }
        assert_eq!(
            session
                .last_memory_diagnostics
                .as_ref()
                .and_then(|diagnostics| diagnostics
                    .persistent_memory
                    .conflict_winner_scope
                    .clone()),
            Some(crate::memory::PersistentMemoryScope::agent(
                crate::memory::project_key(&project_dir),
                "planner",
            ))
        );
    }

    #[test]
    fn wait_graph_cycle_detection_catches_indirect_cycles() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut sessions = HashMap::new();
        let session_a = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-wait-cycles-a"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        let session_b = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-wait-cycles-b"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        let session_c = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-wait-cycles-c"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();

        let id_a = SessionId::new();
        let id_b = SessionId::new();
        let id_c = SessionId::new();
        let mut session_a = session_a;
        let mut session_b = session_b;
        let session_c = session_c;
        session_a.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_a".into(),
            source: MessageSource::Session(id_b),
            timeout_at: None,
        });
        session_b.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_b".into(),
            source: MessageSource::Session(id_c),
            timeout_at: None,
        });
        sessions.insert(id_a, session_a);
        sessions.insert(id_b, session_b);
        sessions.insert(id_c, session_c);

        assert!(waiting_would_cycle(&sessions, id_c, id_a));
        assert!(waiting_would_cycle(&sessions, id_b, id_b));
        assert!(!waiting_would_cycle(&sessions, id_c, SessionId::new()));
    }

    #[test]
    fn waiting_would_cycle_including_session_tree_detects_mixed_wait_cycles() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut sessions = HashMap::new();
        let mut session_tree = SessionTree::new();

        let waiter_id = SessionId::new();
        let dependency_id = SessionId::new();
        let transitive_id = SessionId::new();

        let mut waiter_session = runtime
            .block_on(SessionContext::new(
                waiter_id,
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-mixed-wait-cycle-a"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        let dependency_session = runtime
            .block_on(SessionContext::new(
                dependency_id,
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-mixed-wait-cycle-b"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();

        waiter_session.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_waiter".into(),
            source: MessageSource::Session(dependency_id),
            timeout_at: None,
        });
        sessions.insert(waiter_id, waiter_session);
        sessions.insert(dependency_id, dependency_session);
        session_tree
            .register_active_wait(dependency_id, transitive_id)
            .unwrap();
        session_tree
            .register_active_wait(transitive_id, waiter_id)
            .unwrap();

        assert!(waiting_would_cycle_including_session_tree(
            &sessions,
            &session_tree,
            waiter_id,
            dependency_id,
        ));
    }

    #[test]
    fn next_wait_deadline_picks_earliest_timeout() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let mut sessions = HashMap::new();
        let id_a = SessionId::new();
        let id_b = SessionId::new();
        let mut session_a = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-timeout-a"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        let mut session_b = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-timeout-b"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        let early = Instant::now() + TokioDuration::from_millis(10);
        let late = Instant::now() + TokioDuration::from_millis(20);
        session_a.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_a".into(),
            source: MessageSource::Any,
            timeout_at: Some(late),
        });
        session_b.suspended_wait = Some(SuspendedWait::ChildExit {
            tool_use_id: "toolu_b".into(),
            child_id: SessionId::new(),
            timeout_at: Some(early),
        });
        sessions.insert(id_a, session_a);
        sessions.insert(id_b, session_b);

        assert_eq!(next_wait_deadline(&sessions), Some(early));
    }

    #[tokio::test]
    async fn timeout_expiry_resumes_waiting_session_with_error() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::assistant_tool_use(None, vec![])],
                archive_root: std::env::temp_dir().join("quine-core-timeout-resume"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.state = SessionState::Waiting;
        session.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_timeout".into(),
            source: MessageSource::Any,
            timeout_at: Some(Instant::now() - TokioDuration::from_millis(1)),
        });

        let mut sessions = HashMap::from([(session_id, session)]);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(16);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        drain_wait_timeouts(&mut sessions, &mut io, &mut engine).await;

        let session = sessions.get(&session_id).unwrap();
        assert!(session.suspended_wait.is_none());
        assert_eq!(session.state, SessionState::Idle);
        let tool_entries: Vec<_> = session
            .history
            .iter()
            .filter(|message| message.role == quine_llm::Role::Tool)
            .collect();
        assert!(!tool_entries.is_empty());
        let serialized = serde_json::to_string(tool_entries.last().unwrap()).unwrap();
        assert!(serialized.contains("recv_message timed out"));

        let mut saw_tool_error = false;
        while let Ok(event) = output_rx.try_recv() {
            if let CoreOutput::ToolResult {
                is_error, content, ..
            } = event
            {
                saw_tool_error = is_error && content.contains("recv_message timed out");
            }
        }
        assert!(saw_tool_error);
    }

    #[tokio::test]
    async fn send_message_queues_resume_for_waiting_mailbox_session() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let waiting_id = SessionId::new();
        let sender_id = SessionId::new();
        let mut waiting_session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-mailbox-resume"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        waiting_session.state = SessionState::Waiting;
        waiting_session.suspended_wait = Some(SuspendedWait::Mailbox {
            tool_use_id: "toolu_wait".into(),
            source: MessageSource::Session(sender_id),
            timeout_at: None,
        });

        let mut sessions = HashMap::from([(waiting_id, waiting_session)]);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(8);
        let mut deferred_inputs = VecDeque::new();
        let wake_waits = Notify::new();

        handle_send_message_input(
            &mut sessions,
            &output_tx,
            sender_id,
            waiting_id,
            "hello".into(),
            &mut deferred_inputs,
            &wake_waits,
        )
        .await;

        let queued = deferred_inputs.pop_front().expect("expected resume input");
        match queued {
            CoreInput::ToolResult { session_id, .. } => assert_eq!(session_id, waiting_id),
            other => panic!("expected queued ToolResult, got {other:?}"),
        }

        let session = sessions.get_mut(&waiting_id).unwrap();
        let message = pop_mailbox_message(&mut session.mailbox, &MessageSource::Session(sender_id))
            .expect("expected queued mailbox message");
        assert_eq!(message.content, "hello");

        let event = output_rx.recv().await.expect("expected MessageReceived");
        match event {
            CoreOutput::MessageReceived {
                session_id,
                from,
                content,
            } => {
                assert_eq!(session_id, waiting_id);
                assert_eq!(from, sender_id);
                assert_eq!(content, "hello");
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn concurrent_tool_batch_requires_explicit_safe_metadata_and_allowlist() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let safe_calls = vec![
            PendingToolCall {
                tool_use_id: "toolu_1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_2".into(),
                tool_name: "find".into(),
                arguments: serde_json::json!({}),
            },
        ];
        assert!(tool_batch_is_concurrency_eligible(&session, &safe_calls));

        let unknown_calls = vec![
            PendingToolCall {
                tool_use_id: "toolu_1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_2".into(),
                tool_name: "unknown".into(),
                arguments: serde_json::json!({}),
            },
        ];
        assert!(!tool_batch_is_concurrency_eligible(
            &session,
            &unknown_calls
        ));

        let special_tool_calls = vec![
            PendingToolCall {
                tool_use_id: "toolu_1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_2".into(),
                tool_name: "wait_child".into(),
                arguments: serde_json::json!({"child_id": "test"}),
            },
        ];
        assert!(!tool_batch_is_concurrency_eligible(
            &session,
            &special_tool_calls
        ));
    }

    #[tokio::test]
    async fn partition_tool_calls_by_concurrency_preserves_order_and_batches() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let temp_dir = tempfile::tempdir().unwrap();
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: temp_dir.path().to_path_buf(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: temp_dir.path().join("archive"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        session
            .tool_registry
            .register(Arc::new(ProbeTool::test_tool("read_file")));
        session
            .tool_registry
            .register(Arc::new(ProbeTool::test_tool("find")));
        session
            .tool_registry
            .register(Arc::new(NonConcurrentProbeTool::new("bash")));
        session.tools = session.tool_registry.tool_definitions();

        let calls = vec![
            PendingToolCall {
                tool_use_id: "toolu_1".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_2".into(),
                tool_name: "find".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_3".into(),
                tool_name: "bash".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_4".into(),
                tool_name: "read_file".into(),
                arguments: serde_json::json!({}),
            },
            PendingToolCall {
                tool_use_id: "toolu_5".into(),
                tool_name: "find".into(),
                arguments: serde_json::json!({}),
            },
        ];

        let partitions = partition_tool_calls_by_concurrency(&session, &calls);
        assert_eq!(partitions.len(), 3);
        assert!(partitions[0].0);
        assert_eq!(partitions[0].1.len(), 2);
        assert!(!partitions[1].0);
        assert_eq!(partitions[1].1.len(), 1);
        assert!(partitions[2].0);
        assert_eq!(partitions[2].1.len(), 2);
        assert_eq!(partitions[0].1[0].tool_use_id, "toolu_1");
        assert_eq!(partitions[0].1[1].tool_use_id, "toolu_2");
        assert_eq!(partitions[1].1[0].tool_use_id, "toolu_3");
        assert_eq!(partitions[2].1[0].tool_use_id, "toolu_4");
        assert_eq!(partitions[2].1[1].tool_use_id, "toolu_5");
    }

    #[tokio::test]
    async fn call_llm_with_messages_deduplicates_tool_call_ids() {
        let provider: Arc<dyn LlmProvider> = Arc::new(ToolCallThenTextProvider {
            call_count: AtomicU32::new(0),
            tool_calls: vec![
                (
                    "toolu_duplicate".into(),
                    "read_file".into(),
                    serde_json::json!({"path": "first"}),
                ),
                (
                    "toolu_duplicate".into(),
                    "read_file".into(),
                    serde_json::json!({"path": "second"}),
                ),
                (
                    "toolu_unique".into(),
                    "find".into(),
                    serde_json::json!({"name": "*.rs"}),
                ),
            ],
            final_text: "done".into(),
        });

        let result = call_llm_with_messages(
            provider.as_ref(),
            &[Message::user("inspect")],
            &[],
            SessionId::new(),
            None,
        )
        .await
        .unwrap();

        match result.turn {
            LlmTurnResult::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].tool_use_id, "toolu_duplicate");
                assert_eq!(calls[0].arguments["path"], "first");
                assert_eq!(calls[1].tool_use_id, "toolu_unique");
            }
            LlmTurnResult::Text(text) => panic!("expected tool calls, got text: {text}"),
        }
    }

    #[tokio::test]
    async fn session_omits_web_tools_when_web_provider_is_disabled() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let temp_dir = tempfile::tempdir().unwrap();
        let (_, session) =
            make_session_for_compaction(&provider, temp_dir.path().to_path_buf(), Vec::new()).await;

        assert!(!session.tools.iter().any(|tool| tool.name == "web_search"));
        assert!(!session.tools.iter().any(|tool| tool.name == "web_open"));
        assert!(session.tool_registry.get("web_search").is_some());
        assert!(session.tool_registry.get("web_open").is_some());
    }

    #[tokio::test]
    async fn concurrent_tool_batch_preserves_request_order_in_outputs() {
        let provider: Arc<dyn LlmProvider> = Arc::new(ToolCallThenTextProvider {
            call_count: AtomicU32::new(0),
            tool_calls: vec![
                (
                    "toolu_read".into(),
                    "read_file".into(),
                    serde_json::json!({}),
                ),
                ("toolu_find".into(), "find".into(), serde_json::json!({})),
            ],
            final_text: "done".into(),
        });
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::user("inspect")],
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(2));
        let completion_order = Arc::new(Mutex::new(Vec::new()));
        session.tool_registry.register(Arc::new(ProbeTool {
            name: "read_file",
            delay: std::time::Duration::from_millis(60),
            active: Arc::clone(&active),
            max_active: Arc::clone(&max_active),
            barrier: Arc::clone(&barrier),
            completion_order: Arc::clone(&completion_order),
        }));
        session.tool_registry.register(Arc::new(ProbeTool {
            name: "find",
            delay: std::time::Duration::from_millis(5),
            active,
            max_active: Arc::clone(&max_active),
            barrier,
            completion_order: Arc::clone(&completion_order),
        }));
        session.tools = session.tool_registry.tool_definitions();

        let session_id = SessionId::new();
        let mut sessions = HashMap::from([(session_id, session)]);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(32);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let outcome = handle_llm_turn(&mut sessions, session_id, &mut io, &mut engine).await;
        assert!(matches!(outcome, TurnOutcome::Completed(Some(ref text)) if text == "done"));
        assert_eq!(max_active.load(Ordering::SeqCst), 2);
        assert_eq!(
            completion_order.lock().unwrap().as_slice(),
            &["find", "read_file"]
        );

        let mut tool_request_ids = Vec::new();
        let mut tool_result_ids = Vec::new();
        let mut saw_text_complete = false;
        let mut saw_turn_complete = false;
        while let Ok(event) = output_rx.try_recv() {
            match event {
                CoreOutput::ToolRequest { tool_use_id, .. } => tool_request_ids.push(tool_use_id),
                CoreOutput::ToolResult { tool_use_id, .. } => tool_result_ids.push(tool_use_id),
                CoreOutput::TextComplete { full_text, .. } => {
                    assert_eq!(full_text, "done");
                    saw_text_complete = true;
                }
                CoreOutput::TurnComplete { .. } => saw_turn_complete = true,
                _ => {}
            }
        }

        assert_eq!(tool_request_ids, vec!["toolu_read", "toolu_find"]);
        assert_eq!(tool_result_ids, vec!["toolu_read", "toolu_find"]);
        assert!(saw_text_complete);
        assert!(saw_turn_complete);
    }

    #[tokio::test]
    async fn cancelling_concurrent_tool_batch_reaches_all_running_calls() {
        let provider: Arc<dyn LlmProvider> = Arc::new(ToolCallThenTextProvider {
            call_count: AtomicU32::new(0),
            tool_calls: vec![
                (
                    "toolu_read".into(),
                    "read_file".into(),
                    serde_json::json!({}),
                ),
                ("toolu_find".into(), "find".into(), serde_json::json!({})),
            ],
            final_text: "should not happen".into(),
        });
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::user("inspect")],
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let started = Arc::new(Barrier::new(3));
        let read_cancelled = Arc::new(AtomicBool::new(false));
        let find_cancelled = Arc::new(AtomicBool::new(false));
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "read_file",
                started: Arc::clone(&started),
                cancelled: Arc::clone(&read_cancelled),
            }));
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "find",
                started: Arc::clone(&started),
                cancelled: Arc::clone(&find_cancelled),
            }));
        session.tools = session.tool_registry.tool_definitions();
        session.state = SessionState::Streaming;

        let session_id = SessionId::new();
        let mut sessions = HashMap::from([(session_id, session)]);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(32);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(8);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let send_cancel = async {
            started.wait().await;
            input_tx
                .send(CoreInput::Cancel { session_id })
                .await
                .unwrap();
        };
        let (outcome, ()) = tokio::join!(
            handle_llm_turn(&mut sessions, session_id, &mut io, &mut engine),
            send_cancel
        );
        assert!(matches!(outcome, TurnOutcome::Cancelled));
        assert!(read_cancelled.load(Ordering::SeqCst));
        assert!(find_cancelled.load(Ordering::SeqCst));

        let mut tool_result_ids = Vec::new();
        let mut saw_turn_complete = false;
        while let Ok(event) = output_rx.try_recv() {
            match event {
                CoreOutput::ToolResult { tool_use_id, .. } => tool_result_ids.push(tool_use_id),
                CoreOutput::TurnComplete { .. } => saw_turn_complete = true,
                CoreOutput::TextComplete { full_text, .. } => {
                    panic!("unexpected text completion after cancel: {full_text}");
                }
                _ => {}
            }
        }
        assert_eq!(tool_result_ids, vec!["toolu_read", "toolu_find"]);
        assert!(saw_turn_complete);
    }

    #[tokio::test]
    async fn handle_plan_progress_prompt_includes_exact_plan_id() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::user("plan this")],
                archive_root: std::env::temp_dir().join("quine-core-plan-progress-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let plan_id = crate::planner::PlanId::new();
        {
            let mut store = session.plan_store.lock().await;
            store.insert(
                plan_id,
                crate::planner::ActionPlan {
                    plan_id,
                    title: "Test plan".into(),
                    actions: vec![
                        crate::planner::Action {
                            action_id: crate::planner::ActionId::new("a1"),
                            title: "First".into(),
                            description: "do first".into(),
                            depends_on: vec![],
                            status: crate::planner::ActionStatus::Completed,
                            result: Some("done".into()),
                        },
                        crate::planner::Action {
                            action_id: crate::planner::ActionId::new("a2"),
                            title: "Second".into(),
                            description: "do second".into(),
                            depends_on: vec![crate::planner::ActionId::new("a1")],
                            status: crate::planner::ActionStatus::Pending,
                            result: None,
                        },
                    ],
                },
            );
        }

        let (output_tx, _output_rx) = tokio::sync::mpsc::channel(8);
        let session_id = SessionId::new();
        handle_plan_progress(
            &mut session,
            session_id,
            &output_tx,
            &plan_id.to_string(),
            "a1",
        )
        .await;

        let prompt = session
            .history
            .last()
            .and_then(|message| match &message.content {
                MessageContent::Text(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("ready-action prompt should be appended");
        assert!(prompt.contains("update_plan"));
        assert!(prompt.contains(&format!("Reuse this exact plan_id: {plan_id}")));
    }

    #[test]
    fn extract_plan_id_from_tool_output_supports_both_formats() {
        assert_eq!(
            extract_plan_id_from_tool_output(
                "Plan created (ID: 11111111-1111-1111-1111-111111111111)\n\nPlan: Demo"
            ),
            Some("11111111-1111-1111-1111-111111111111".into())
        );
        assert_eq!(
            extract_plan_id_from_tool_output(
                "Plan created (ID: 22222222-2222-2222-2222-222222222222)\nplan_id: 22222222-2222-2222-2222-222222222222\n\nPlan: Demo"
            ),
            Some("22222222-2222-2222-2222-222222222222".into())
        );
    }

    #[test]
    fn normalize_plan_tool_arguments_recovers_plan_id_from_prior_tool_result() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = runtime
            .block_on(SessionContext::new(
                SessionId::new(),
                SessionInit {
                    system_prompt: None,
                    skills: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    archive_root: std::env::temp_dir().join("quine-core-plan-id-resolution-tests"),
                    max_context_window: None,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                &provider,
            ))
            .unwrap();
        session.history.push(Message::tool_result(
            "call_create_plan",
            "Plan created (ID: 33333333-3333-3333-3333-333333333333)\nplan_id: 33333333-3333-3333-3333-333333333333\n\nPlan: Demo",
            false,
        ));

        let normalized = normalize_plan_tool_arguments(
            &session,
            &PendingToolCall {
                tool_use_id: "call_update_plan".into(),
                tool_name: "plan".into(),
                arguments: serde_json::json!({
                    "operation": "update_plan",
                    "plan_id": "call_create_plan",
                    "action_id": "a1",
                    "status": "completed"
                }),
            },
        );

        assert_eq!(
            normalized
                .arguments
                .get("plan_id")
                .and_then(|value| value.as_str()),
            Some("33333333-3333-3333-3333-333333333333")
        );
    }

    struct ToolCallThenTextProvider {
        call_count: AtomicU32,
        tool_calls: Vec<(String, String, serde_json::Value)>,
        final_text: String,
    }

    #[async_trait::async_trait]
    impl LlmProvider for ToolCallThenTextProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let count = self.call_count.fetch_add(1, Ordering::SeqCst);
            let events = if count == 0 {
                let mut events = Vec::new();
                for (tool_use_id, tool_name, arguments) in &self.tool_calls {
                    events.push(Ok(LlmEvent::ToolCall {
                        tool_use_id: tool_use_id.clone(),
                        tool_name: tool_name.clone(),
                        arguments: arguments.clone(),
                    }));
                }
                events.push(Ok(LlmEvent::Done { usage: None }));
                events
            } else {
                vec![
                    Ok(LlmEvent::TextDelta {
                        text: self.final_text.clone(),
                    }),
                    Ok(LlmEvent::Done { usage: None }),
                ]
            };
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    struct ProbeTool {
        name: &'static str,
        delay: std::time::Duration,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
        barrier: Arc<Barrier>,
        completion_order: Arc<Mutex<Vec<&'static str>>>,
    }

    impl ProbeTool {
        fn test_tool(name: &'static str) -> Self {
            Self {
                name,
                delay: std::time::Duration::from_millis(0),
                active: Arc::new(AtomicUsize::new(0)),
                max_active: Arc::new(AtomicUsize::new(0)),
                barrier: Arc::new(Barrier::new(1)),
                completion_order: Arc::new(Mutex::new(Vec::new())),
            }
        }
    }

    struct NonConcurrentProbeTool {
        name: &'static str,
    }

    impl NonConcurrentProbeTool {
        fn new(name: &'static str) -> Self {
            Self { name }
        }
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for ProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Test probe tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn is_read_only(&self) -> bool {
            true
        }

        fn is_idempotent(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ExecutionContext,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            let active_now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            let _ = self
                .max_active
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                    Some(current.max(active_now))
                });
            self.barrier.wait().await;
            tokio::time::sleep(self.delay).await;
            self.completion_order.lock().unwrap().push(self.name);
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(crate::tool::ToolOutput::success(self.name))
        }
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for NonConcurrentProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Non-concurrent test probe tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ExecutionContext,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            Ok(crate::tool::ToolOutput::success(self.name))
        }
    }

    struct CancellableProbeTool {
        name: &'static str,
        started: Arc<Barrier>,
        cancelled: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for CancellableProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Test cancellable probe tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn is_read_only(&self) -> bool {
            true
        }

        fn is_idempotent(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            context: &ExecutionContext,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            self.started.wait().await;
            context.cancellation.cancelled().await;
            self.cancelled.store(true, Ordering::SeqCst);
            Err(crate::tool::ToolError::Cancelled)
        }
    }

    struct MutatingProbeTool {
        name: &'static str,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for MutatingProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Test mutating probe tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ExecutionContext,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            Ok(crate::tool::ToolOutput::success("mutated"))
        }
    }

    struct MarkerProbeTool {
        name: &'static str,
        ran: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::tool::Tool for MarkerProbeTool {
        fn name(&self) -> &str {
            self.name
        }

        fn description(&self) -> &str {
            "Test marker probe tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        fn is_read_only(&self) -> bool {
            true
        }

        fn is_idempotent(&self) -> bool {
            true
        }

        async fn execute(
            &self,
            _arguments: serde_json::Value,
            _context: &ExecutionContext,
        ) -> Result<crate::tool::ToolOutput, crate::tool::ToolError> {
            self.ran.store(true, Ordering::SeqCst);
            Ok(crate::tool::ToolOutput::success("marker"))
        }
    }

    #[tokio::test]
    async fn build_permission_request_uses_apply_patch_path_resource() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        let tool = crate::tool::write::WriteTool;
        let call = PendingToolCall {
            tool_use_id: "toolu_apply_patch".into(),
            tool_name: "apply_patch".into(),
            arguments: serde_json::json!({"file_path": "src/lib.rs", "edits": []}),
        };

        let (request, local) = build_permission_request(&session, &call, &tool);

        assert_eq!(request.tool_name, "apply_patch");
        assert_eq!(
            request.scope,
            crate::permission::request::PermissionScope::Write
        );
        assert!(matches!(
            request.resource,
            PermissionResource::Path { ref path } if path.ends_with("src/lib.rs")
        ));
        assert_eq!(
            local.expect("tool-local decision should exist").decision,
            crate::permission::types::PermissionDecision::Defer
        );
    }

    #[tokio::test]
    async fn build_permission_request_attaches_bash_command_risk_metadata() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        let tool = crate::tool::bash::BashTool;
        let call = PendingToolCall {
            tool_use_id: "toolu_bash".into(),
            tool_name: "bash".into(),
            arguments: serde_json::json!({"command": "pwd"}),
        };

        let (request, local) = build_permission_request(&session, &call, &tool);

        assert_eq!(request.tool_name, "bash");
        assert_eq!(
            request.scope,
            crate::permission::request::PermissionScope::Execute
        );
        assert!(matches!(
            request.resource,
            PermissionResource::Command { ref descriptor }
                if descriptor.program.as_deref() == Some("pwd")
                    && descriptor.risk == crate::permission::CommandRisk::ReadOnly
        ));
        assert_eq!(
            local.expect("tool-local decision should exist").decision,
            crate::permission::types::PermissionDecision::Defer
        );
    }

    #[tokio::test]
    async fn build_permission_request_treats_ask_user_as_internal_interaction() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        let tool = AskUserTool;
        let call = PendingToolCall {
            tool_use_id: "toolu_ask".into(),
            tool_name: "ask_user".into(),
            arguments: serde_json::json!({"question": "Proceed?"}),
        };

        let (request, _) = build_permission_request(&session, &call, &tool);

        assert_eq!(request.tool_name, "ask_user");
        assert_eq!(
            request.scope,
            crate::permission::request::PermissionScope::Read
        );
        assert_eq!(request.resource, PermissionResource::None);
    }

    #[tokio::test]
    async fn execute_tool_call_consumes_cancel_for_non_interactive_tool() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "read_probe",
                started: Arc::new(Barrier::new(1)),
                cancelled: Arc::new(AtomicBool::new(false)),
            }));

        let call = PendingToolCall {
            tool_use_id: "toolu_cancel".into(),
            tool_name: "read_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, _output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let session_id = SessionId::new();
        input_tx
            .send(CoreInput::Cancel { session_id })
            .await
            .unwrap();

        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };
        let result =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(matches!(result, ToolOutcome::Cancelled));
        assert!(sessions
            .get(&session_id)
            .is_some_and(|session| session.cancel_tx.is_none()));
        assert!(deferred_inputs.is_empty());
    }

    #[tokio::test]
    async fn execute_tool_call_buffers_unrelated_input_while_waiting_for_cancel() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "read_probe",
                started: Arc::new(Barrier::new(1)),
                cancelled: Arc::new(AtomicBool::new(false)),
            }));

        let call = PendingToolCall {
            tool_use_id: "toolu_cancel".into(),
            tool_name: "read_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, _output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let session_id = SessionId::new();
        let other_session_id = SessionId::new();

        input_tx
            .send(CoreInput::UserMessage {
                session_id: other_session_id,
                content: "hello".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();
        input_tx
            .send(CoreInput::Cancel { session_id })
            .await
            .unwrap();

        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };
        let result =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(matches!(result, ToolOutcome::Cancelled));
        assert!(matches!(
            deferred_inputs.pop_front(),
            Some(CoreInput::UserMessage {
                session_id,
                content,
                ..
            }) if session_id == other_session_id && content == "hello"
        ));
    }

    #[tokio::test]
    async fn execute_tool_call_records_permission_outcome_before_execution() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.tool_registry.register(Arc::new(MutatingProbeTool {
            name: "write_probe",
        }));

        let call = PendingToolCall {
            tool_use_id: "toolu_permission".into(),
            tool_name: "write_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let execute = execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine);
        let deny = async {
            loop {
                match output_rx.recv().await {
                    Some(CoreOutput::InteractionNeeded { .. }) => {
                        input_tx
                            .send(CoreInput::InteractionResponse {
                                session_id,
                                response: InteractionResponse {
                                    response: "deny once".into(),
                                    selected_indices: vec![1],
                                },
                            })
                            .await
                            .unwrap();
                        break;
                    }
                    Some(CoreOutput::SessionStateChanged { .. }) => {}
                    other => panic!("expected permission interaction, got {other:?}"),
                }
            }
        };
        let (outcome, ()) = tokio::join!(execute, deny);

        assert!(
            matches!(outcome, ToolOutcome::Error { ref message } if message.contains("permission denied")),
            "expected permission-denied error, got {outcome:?}"
        );
        let permission_outcome = sessions
            .get(&session_id)
            .and_then(|session| session.last_permission_outcome.clone())
            .expect("permission outcome should be recorded");
        assert_eq!(
            permission_outcome.kind,
            crate::permission::outcome::PermissionOutcomeKind::RequiresApproval
        );
        assert_eq!(
            permission_outcome.final_decision,
            crate::permission::types::PermissionDecision::Ask
        );
        assert_eq!(
            permission_outcome.source.kind,
            crate::permission::request::PermissionMatchKind::ModeDefault
        );
        assert_eq!(permission_outcome.request.tool_name, "write_probe");
    }

    #[tokio::test]
    async fn execute_tool_call_waits_for_permission_approval_and_resumes_on_approve() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.tool_registry.register(Arc::new(MutatingProbeTool {
            name: "write_probe",
        }));

        let call = PendingToolCall {
            tool_use_id: "toolu_permission_approve".into(),
            tool_name: "write_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let execute = execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine);
        let approve = async {
            loop {
                match output_rx.recv().await {
                    Some(CoreOutput::InteractionNeeded { request, .. }) => {
                        assert!(request
                            .source_label
                            .as_deref()
                            .is_some_and(|label| label.starts_with("permission:")));
                        input_tx
                            .send(CoreInput::InteractionResponse {
                                session_id,
                                response: InteractionResponse {
                                    response: "approve once".into(),
                                    selected_indices: vec![0],
                                },
                            })
                            .await
                            .unwrap();
                        break;
                    }
                    Some(CoreOutput::SessionStateChanged { .. }) => {}
                    other => panic!("expected permission interaction, got {other:?}"),
                }
            }
        };

        let (outcome, ()) = tokio::join!(execute, approve);

        assert!(matches!(outcome, ToolOutcome::Success { ref output } if output == "mutated"));
        assert!(sessions
            .get(&session_id)
            .is_some_and(|session| session.pending_permission_approval.is_none()));
    }

    #[tokio::test]
    async fn execute_tool_call_resolves_permission_approval_to_denial() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.tool_registry.register(Arc::new(MutatingProbeTool {
            name: "write_probe",
        }));

        let call = PendingToolCall {
            tool_use_id: "toolu_permission_deny".into(),
            tool_name: "write_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let execute = execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine);
        let deny = async {
            loop {
                match output_rx.recv().await {
                    Some(CoreOutput::InteractionNeeded { .. }) => {
                        input_tx
                            .send(CoreInput::InteractionResponse {
                                session_id,
                                response: InteractionResponse {
                                    response: "deny once".into(),
                                    selected_indices: vec![1],
                                },
                            })
                            .await
                            .unwrap();
                        break;
                    }
                    Some(CoreOutput::SessionStateChanged { .. }) => {}
                    other => panic!("expected permission interaction, got {other:?}"),
                }
            }
        };

        let (outcome, ()) = tokio::join!(execute, deny);

        assert!(
            matches!(outcome, ToolOutcome::Error { ref message } if message.contains("permission denied"))
        );
        assert!(sessions
            .get(&session_id)
            .is_some_and(|session| session.pending_permission_approval.is_none()));
    }

    #[tokio::test]
    async fn execute_tool_call_denies_headless_permission_request_without_interaction() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Headless,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session.tool_registry.register(Arc::new(MutatingProbeTool {
            name: "write_probe",
        }));

        let call = PendingToolCall {
            tool_use_id: "toolu_permission_headless".into(),
            tool_name: "write_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let outcome =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(
            matches!(outcome, ToolOutcome::Error { ref message } if message.contains("permission denied"))
        );
        assert!(output_rx.try_recv().is_err());
        let permission_outcome = sessions
            .get(&session_id)
            .and_then(|session| session.last_permission_outcome.clone())
            .expect("permission outcome should be recorded");
        assert_eq!(
            permission_outcome.source.kind,
            crate::permission::request::PermissionMatchKind::HeadlessFallback
        );
        assert_eq!(
            permission_outcome.final_decision,
            crate::permission::types::PermissionDecision::Deny
        );
        assert!(sessions
            .get(&session_id)
            .is_some_and(|session| session.pending_permission_approval.is_none()));
    }

    #[tokio::test]
    async fn execute_tool_call_denies_outside_workspace_write_boundary() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let workspace = tempfile::TempDir::new().unwrap();
        let session_id = SessionId::new();
        let session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: workspace.path().to_path_buf(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-permission-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();

        let call = PendingToolCall {
            tool_use_id: "toolu_outside_write".into(),
            tool_name: "apply_patch".into(),
            arguments: serde_json::json!({
                "file_path": "../forbidden.txt",
                "new_file_content": "forbidden"
            }),
        };

        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let outcome =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(
            matches!(outcome, ToolOutcome::Error { ref message } if message.contains("permission denied"))
        );
        assert!(output_rx.try_recv().is_err());
        let outside_path = workspace.path().parent().unwrap().join("forbidden.txt");
        assert!(!outside_path.exists());
    }

    #[tokio::test]
    async fn execute_tool_call_consumes_stop_signal_for_non_interactive_tool() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let mut session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "read_probe",
                started: Arc::new(Barrier::new(1)),
                cancelled: Arc::new(AtomicBool::new(false)),
            }));

        let call = PendingToolCall {
            tool_use_id: "toolu_signal_cancel".into(),
            tool_name: "read_probe".into(),
            arguments: serde_json::json!({}),
        };

        let (output_tx, _output_rx) = tokio::sync::mpsc::channel(4);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(4);
        let mut deferred_inputs = VecDeque::new();
        let session_id = SessionId::new();
        input_tx
            .send(CoreInput::Signal {
                session_id,
                signal: SessionSignal::Stop,
            })
            .await
            .unwrap();

        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut sessions = HashMap::from([(session_id, session)]);
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };
        let result =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(matches!(result, ToolOutcome::Cancelled));
        assert!(sessions
            .get(&session_id)
            .is_some_and(|session| session.cancel_tx.is_none() && session.interrupted));
    }

    #[tokio::test]
    async fn prepare_tool_result_for_history_keeps_small_output_inline() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        let session_id = SessionId::new();

        let output = prepare_tool_result_for_history(
            &session,
            session_id,
            "toolu_small",
            "bash",
            "small output",
            false,
        )
        .await
        .unwrap();

        assert_eq!(output, "small output");
    }

    #[tokio::test]
    async fn prepare_tool_result_for_history_archives_oversized_output() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let archive_root = std::env::temp_dir().join(format!(
            "quine-core-compaction-tests-{:?}",
            SessionId::new()
        ));
        let session = SessionContext::new(
            SessionId::new(),
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                archive_root: archive_root.clone(),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        let session_id = SessionId::new();
        let oversized = (1..=13)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n")
            + "\n"
            + &"x".repeat(compaction::MAX_TOOL_RESULT_CHARS_IN_HISTORY + 128);

        let output = prepare_tool_result_for_history(
            &session,
            session_id,
            "toolu_big",
            "bash",
            &oversized,
            false,
        )
        .await
        .unwrap();

        assert!(output.contains("[tool result archived: bash, ok"));
        assert!(output.contains("archive="));
        assert!(output.contains("line 1"));
        assert!(output.contains("line 12"));
        assert!(!output.contains("line 13"));
        assert!(output.contains("[... omitted 2 more line(s); full tool result archived at "));

        let archived_dir = archive_root
            .join("tool-results")
            .join(session_id_string(session_id));
        let entries = std::fs::read_dir(&archived_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 1);
        let archived = std::fs::read_to_string(entries[0].path()).unwrap();
        assert_eq!(archived.len(), oversized.len());

        let _ = std::fs::remove_dir_all(archive_root);
    }

    #[tokio::test]
    async fn spawn_and_wait_child_complete_without_core_ack_deadlock() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::new("73"));
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let parent_id = SessionId::new();
        let (create_reply_tx, create_reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id: parent_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: create_reply_tx,
            })
            .await
            .unwrap();
        assert!(create_reply_rx.await.unwrap().is_ok());
        let _ = output.recv().await.unwrap();

        let child_id = SessionId::new();
        let (spawn_reply_tx, spawn_reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::SpawnSession {
                parent_id,
                child_id,
                task: "Reply with exactly one integer. No explanation.".into(),
                system_prompt: None,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                inheritance: InheritanceFlags::default(),
                reply: spawn_reply_tx,
            })
            .await
            .unwrap();
        assert!(spawn_reply_rx.await.unwrap().is_ok());

        let (wait_reply_tx, wait_reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::WaitSession {
                parent_id,
                child_id,
                reply: wait_reply_tx,
                non_blocking: false,
                timeout: None,
            })
            .await
            .unwrap();

        let status = tokio::time::timeout(std::time::Duration::from_secs(10), wait_reply_rx)
            .await
            .expect("wait_child should not deadlock")
            .unwrap()
            .expect("child should exit successfully");

        match status {
            Some(ExitStatus::Success { output }) => {
                assert!(
                    output.contains("73"),
                    "expected child output in wait result: {output}"
                );
            }
            other => panic!("expected completed exit status, got {other:?}"),
        }

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn create_session_and_shutdown_baseline() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());

        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn session_memory_foundation_creates_and_updates_summary() {
        let temp = TempDir::new().unwrap();
        let (mut harness, core) = create_channels(ChannelConfig::default());
        let provider = Arc::new(MockProvider::new("assistant reply"));
        let loop_handle = tokio::spawn(run_core_loop_with_compaction(
            core,
            provider.clone(),
            None,
            temp.path().to_path_buf(),
            None,
        ));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = harness.output.recv().await.unwrap();

        for content in [
            "first message about crates/quine-core/src/engine.rs",
            "second message",
        ] {
            harness
                .input
                .send(CoreInput::UserMessage {
                    session_id,
                    content: content.into(),
                    turn_id: uuid::Uuid::new_v4().to_string(),
                })
                .await
                .unwrap();
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), harness.output.recv())
                    .await
                {
                    Ok(Some(CoreOutput::TurnComplete { .. })) => break,
                    Ok(Some(_)) => {}
                    other => panic!("unexpected event while waiting for TurnComplete: {other:?}"),
                }
            }
        }

        let summary_path = temp
            .path()
            .join("sessions")
            .join(session_id.to_string())
            .join("session-memory")
            .join("summary.md");
        let metadata_path = temp
            .path()
            .join("sessions")
            .join(session_id.to_string())
            .join("session-memory")
            .join("summary.meta.json");
        wait_for_test_files(
            &[summary_path.as_path(), metadata_path.as_path()],
            "session memory foundation files",
        )
        .await;
        assert!(summary_path.exists(), "summary file should exist");
        assert!(metadata_path.exists(), "summary metadata should exist");
        let summary = std::fs::read_to_string(&summary_path).unwrap();
        assert!(summary.contains("## Current State"));
        assert!(summary.contains("crates/quine-core/src/engine.rs"));
        let metadata = load_summary_metadata(&metadata_path).unwrap();
        assert!(metadata.last_summarized_message_index >= 1);

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn session_memory_refresh_persists_model_generated_listing_summary() {
        let temp = TempDir::new().unwrap();
        let (mut harness, core) = create_channels(ChannelConfig::default());
        let provider = Arc::new(SequenceProvider::new([
            "assistant reply",
            "Implements session-memory listing summaries for active sessions.",
        ]));
        let loop_handle = tokio::spawn(run_core_loop_with_compaction(
            core,
            provider.clone(),
            None,
            temp.path().to_path_buf(),
            None,
        ));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = harness.output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "add a listing summary to session memory".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), harness.output.recv())
                .await
            {
                Ok(Some(CoreOutput::TurnComplete { .. })) => break,
                Ok(Some(_)) => {}
                other => panic!("unexpected event while waiting for TurnComplete: {other:?}"),
            }
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let expected = "Implements session-memory listing summaries for active sessions.";
        let persisted = loop {
            let (reply_tx, reply_rx) = oneshot::channel();
            harness
                .input
                .send(CoreInput::RequestCheckpoint { reply: reply_tx })
                .await
                .unwrap();
            let checkpoint = reply_rx.await.unwrap();

            let persisted = checkpoint
                .sessions
                .iter()
                .find(|session| session.session_id == session_id)
                .and_then(|session| session.memory_state.as_ref())
                .and_then(|state| state.session_memory.as_ref())
                .and_then(|state| state.listing_summary.clone());
            if persisted.as_deref() == Some(expected) {
                break persisted;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out waiting for listing summary, last observed value: {persisted:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        };
        assert_eq!(persisted.as_deref(), Some(expected));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn session_memory_refresh_clears_inflight_and_advances_multiple_turns() {
        let temp = TempDir::new().unwrap();
        let (mut harness, core) = create_channels(ChannelConfig::default());
        let provider = Arc::new(MockProvider::new("assistant reply"));
        let loop_handle = tokio::spawn(run_core_loop_with_compaction(
            core,
            provider.clone(),
            None,
            temp.path().to_path_buf(),
            None,
        ));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = harness.output.recv().await.unwrap();

        let metadata_path = temp
            .path()
            .join("sessions")
            .join(session_id.to_string())
            .join("session-memory")
            .join("summary.meta.json");

        for (index, content) in ["LIVE ROUND 1", "LIVE ROUND 2"].into_iter().enumerate() {
            harness
                .input
                .send(CoreInput::UserMessage {
                    session_id,
                    content: content.into(),
                    turn_id: uuid::Uuid::new_v4().to_string(),
                })
                .await
                .unwrap();
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), harness.output.recv())
                    .await
                {
                    Ok(Some(CoreOutput::TurnComplete { .. })) => break,
                    Ok(Some(_)) => {}
                    other => panic!("unexpected event while waiting for TurnComplete: {other:?}"),
                }
            }
            let minimum_index = if index == 0 { 1 } else { 4 };
            wait_for_summary_metadata_index(
                &metadata_path,
                minimum_index,
                "session memory refresh metadata to advance",
            )
            .await;
        }

        let metadata = load_summary_metadata(&metadata_path).unwrap();
        assert!(metadata.last_summarized_message_index >= 4);

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn restore_session_memory_foundation_recovers_missing_files() {
        let temp = TempDir::new().unwrap();
        let provider = Arc::new(MockProvider::new("assistant reply"));
        let session_id = SessionId::new();

        {
            let (mut harness, core) = create_channels(ChannelConfig::default());
            let loop_handle = tokio::spawn(run_core_loop_with_compaction(
                core,
                provider.clone(),
                None,
                temp.path().to_path_buf(),
                None,
            ));
            let (reply_tx, reply_rx) = oneshot::channel();
            harness
                .input
                .send(CoreInput::CreateSession {
                    session_id,
                    system_prompt: None,
                    working_directory: None,
                    skills: Vec::new(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    agent_key: None,
                    team_key: None,
                    session_group: None,
                    permission_rules: PermissionRuleSet::default(),
                    memory_policy: MemoryPolicyConfig::default(),
                    session_llm: session_llm_config(provider.clone()),
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                    reply: reply_tx,
                })
                .await
                .unwrap();
            reply_rx.await.unwrap().unwrap();
            let _ = harness.output.recv().await.unwrap();
            harness
                .input
                .send(CoreInput::UserMessage {
                    session_id,
                    content: "seed session memory".into(),
                    turn_id: uuid::Uuid::new_v4().to_string(),
                })
                .await
                .unwrap();
            loop {
                match tokio::time::timeout(std::time::Duration::from_secs(5), harness.output.recv())
                    .await
                {
                    Ok(Some(CoreOutput::TurnComplete { .. })) => break,
                    Ok(Some(_)) => {}
                    other => panic!("unexpected event while waiting for TurnComplete: {other:?}"),
                }
            }
            harness.input.send(CoreInput::Shutdown).await.unwrap();
            loop_handle.await.unwrap();
        }

        let summary_dir = temp
            .path()
            .join("sessions")
            .join(session_id.to_string())
            .join("session-memory");
        let summary_path = summary_dir.join("summary.md");
        let metadata_path = summary_dir.join("summary.meta.json");
        wait_for_test_files(
            &[summary_path.as_path(), metadata_path.as_path()],
            "session memory files before restore",
        )
        .await;
        std::fs::remove_file(&summary_path).unwrap();
        std::fs::remove_file(&metadata_path).unwrap();

        let checkpoint = crate::persistence::CoreCheckpoint::new(
            vec![crate::persistence::PersistedSession {
                session_id,
                created_at: Utc::now(),
                state: crate::persistence::PersistedSessionState::Idle,
                config: crate::persistence::PersistedSessionConfig {
                    system_prompt: None,
                    skill_names: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                history: vec![
                    Message::user("seed session memory"),
                    Message::assistant("assistant reply"),
                ],
                plan_store: crate::persistence::PersistedPlanStore::default(),
                memory_state: Some(crate::persistence::PersistedMemoryState {
                    session_memory: Some(crate::persistence::PersistedSessionMemoryState {
                        enabled: true,
                        last_summarized_message_index: Some(1),
                        template_version: 1,
                        listing_summary: Some("Seeded session memory for restore coverage.".into()),
                    }),
                    persistent_memory: Some(crate::persistence::PersistedPersistentMemoryState {
                        enabled: true,
                        last_extracted_message_index: Some(1),
                        scope_state: None,
                    }),
                    prompt_memory: None,
                    memory_diagnostics: None,
                }),
                permission_state: None,
                status_report: None,
                python_state: None,
            }],
            crate::persistence::PersistedSessionTree {
                parents: std::collections::HashMap::new(),
                children: std::collections::HashMap::new(),
                exit_statuses: std::collections::HashMap::new(),
            },
        );

        let (mut harness, core) = create_channels(ChannelConfig::default());
        let loop_handle = tokio::spawn(run_core_loop_with_compaction(
            core,
            provider.clone(),
            Some(checkpoint),
            temp.path().to_path_buf(),
            None,
        ));
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "resume after restore".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();
        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), harness.output.recv())
                .await
            {
                Ok(Some(CoreOutput::TurnComplete { .. })) => break,
                Ok(Some(_)) => {}
                other => panic!("unexpected event while waiting for TurnComplete: {other:?}"),
            }
        }
        wait_for_test_files(
            &[summary_path.as_path(), metadata_path.as_path()],
            "session memory files after restore",
        )
        .await;
        assert!(summary_path.exists(), "summary file should be recreated");
        assert!(metadata_path.exists(), "metadata file should be recreated");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn restore_session_sanitizes_zombie_tool_calls_before_next_provider_request() {
        let temp = TempDir::new().unwrap();
        let session_id = SessionId::new();
        let zombie_tool_use_id = "call_zombie";
        let provider = Arc::new(RejectZombieToolUseProvider {
            forbidden_tool_use_id: zombie_tool_use_id.into(),
        });
        let checkpoint = crate::persistence::CoreCheckpoint::new(
            vec![crate::persistence::PersistedSession {
                session_id,
                created_at: Utc::now(),
                state: crate::persistence::PersistedSessionState::Idle,
                config: crate::persistence::PersistedSessionConfig {
                    system_prompt: None,
                    skill_names: Vec::new(),
                    working_directory: std::env::current_dir().unwrap_or_default(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    prompt_memory_mode: PromptMemoryMode::Disabled,
                    agent_key: None,
                    team_key: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    model_profile: None,
                    session_group: None,
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                },
                history: vec![
                    Message::user("run a tool"),
                    Message::assistant_tool_use(
                        Some("calling read_file".into()),
                        vec![quine_llm::ToolUseRequest {
                            tool_use_id: zombie_tool_use_id.into(),
                            tool_name: "read_file".into(),
                            arguments: serde_json::json!({"path": "Cargo.toml"}),
                        }],
                    ),
                ],
                plan_store: crate::persistence::PersistedPlanStore::default(),
                memory_state: None,
                permission_state: None,
                status_report: None,
                python_state: None,
            }],
            crate::persistence::PersistedSessionTree {
                parents: std::collections::HashMap::new(),
                children: std::collections::HashMap::new(),
                exit_statuses: std::collections::HashMap::new(),
            },
        );

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let loop_handle = tokio::spawn(run_core_loop_with_compaction(
            core,
            provider,
            Some(checkpoint),
            temp.path().to_path_buf(),
            None,
        ));

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "resume".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(5), output.recv()).await {
                Ok(Some(CoreOutput::TurnComplete {
                    session_id: event_session_id,
                    ..
                })) if event_session_id == session_id => break,
                Ok(Some(CoreOutput::SessionError {
                    session_id: event_session_id,
                    error,
                })) if event_session_id == session_id => {
                    panic!("restored session should not surface an LLM error: {error:?}");
                }
                Ok(Some(_)) => {}
                other => panic!("unexpected event while waiting for restored turn: {other:?}"),
            }
        }

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_to_unknown_session_errors() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(MockProvider::empty()), None));

        let session_id = SessionId::new();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::SessionError { error, .. } => match error {
                CoreError::SessionNotFound => {}
                other => panic!("expected SessionNotFound, got {other:?}"),
            },
            other => panic!("expected SessionError, got {other:?}"),
        }

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn stale_external_tool_result_is_ignored_when_session_is_idle() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());

        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());
        let _ = output.recv().await.unwrap(); // SessionStateChanged(Idle)

        harness
            .input
            .send(CoreInput::ToolResult {
                session_id,
                tool_use_id: "toolu_stale".into(),
                result: ToolOutcome::Success {
                    output: "stale".into(),
                },
            })
            .await
            .unwrap();

        if let Ok(Some(event)) =
            tokio::time::timeout(TokioDuration::from_millis(100), output.recv()).await
        {
            assert!(
                !matches!(event, CoreOutput::SessionError { .. }),
                "stale external tool result should not emit a session error, got {event:?}"
            );
        }

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_produces_turn_complete() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());

        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain the SessionStateChanged from creation and checkpoint request.
        let _ = output.recv().await.unwrap();
        let _ = output.recv().await.unwrap();

        let turn_id = uuid::Uuid::new_v4().to_string();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
                turn_id: turn_id.clone(),
            })
            .await
            .unwrap();

        // Expect: SessionStateChanged(Streaming), TurnStarted, TextComplete, TurnComplete
        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::TurnStarted {
                session_id: started_session_id,
                turn_id: started_turn_id,
            } if started_session_id == session_id && started_turn_id == turn_id
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TextComplete { .. }));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn llm_stream_error_emits_session_error_and_failed_turn_complete() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider: Arc<dyn LlmProvider> = Arc::new(FailingStreamProvider);

        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let _ = output.recv().await.unwrap();
        let _ = output.recv().await.unwrap();

        let turn_id = uuid::Uuid::new_v4().to_string();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
                turn_id: turn_id.clone(),
            })
            .await
            .unwrap();

        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));
        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::TurnStarted {
                session_id: started_session_id,
                turn_id: started_turn_id,
            } if started_session_id == session_id && started_turn_id == turn_id
        ));
        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::ReasoningDelta { delta, .. } if delta == "thinking"
        ));
        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::SessionStateChanged {
                state: SessionState::Idle,
                ..
            }
        ));
        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::SessionError {
                error: CoreError::LlmError { message },
                ..
            } if message.contains("openai_compat stream read failed")
                && message.contains("error decoding response body")
        ));
        assert!(matches!(
            output.recv().await.unwrap(),
            CoreOutput::TurnComplete {
                status: TurnStatus::Failed,
                ..
            }
        ));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_session_id_returns_error() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());

        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_ok());

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_err());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn user_message_streams_llm_response() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = Arc::new(MockProvider::new("Hello from the LLM!"));
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                permission_rules: PermissionRuleSet::default(),
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let _ = output.recv().await.unwrap();
        let _ = output.recv().await.unwrap();

        let turn_id = uuid::Uuid::new_v4().to_string();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
                turn_id: turn_id.clone(),
            })
            .await
            .unwrap();

        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::TurnStarted {
                session_id: started_session_id,
                turn_id: started_turn_id,
            } if started_session_id == session_id && started_turn_id == turn_id
        ));

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::StreamDelta { delta, .. } => {
                assert_eq!(delta, "Hello from the LLM!");
            }
            other => panic!("expected StreamDelta, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        match event {
            CoreOutput::TextComplete { full_text, .. } => {
                assert_eq!(full_text, "Hello from the LLM!");
            }
            other => panic!("expected TextComplete, got {other:?}"),
        }

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn different_sessions_can_enter_provider_concurrently() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = Arc::new(ConcurrentSessionProvider::new("parallel reply"));
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_a = SessionId::new();
        let session_b = SessionId::new();
        for session_id in [session_a, session_b] {
            let (reply_tx, reply_rx) = oneshot::channel();
            harness
                .input
                .send(CoreInput::CreateSession {
                    session_id,
                    system_prompt: None,
                    working_directory: None,
                    skills: Vec::new(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    initial_messages: Vec::new(),
                    agent_key: None,
                    team_key: None,
                    session_group: None,
                    permission_rules: PermissionRuleSet::default(),
                    memory_policy: MemoryPolicyConfig::default(),
                    session_llm: session_llm_config(provider.clone()),
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                    reply: reply_tx,
                })
                .await
                .unwrap();
            reply_rx.await.unwrap().unwrap();
        }

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id: session_a,
                content: "first".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id: session_b,
                content: "second".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        provider.wait_until_started(2).await;
        provider.release();

        let mut completed = Vec::new();
        while completed.len() < 2 {
            match tokio::time::timeout(TokioDuration::from_secs(5), output.recv()).await {
                Ok(Some(CoreOutput::TurnComplete { session_id, .. })) => {
                    if !completed.contains(&session_id) {
                        completed.push(session_id);
                    }
                }
                Ok(Some(_)) => {}
                other => panic!("unexpected output while waiting for turns to complete: {other:?}"),
            }
        }

        assert!(completed.contains(&session_a));
        assert!(completed.contains(&session_b));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn busy_session_buffers_user_messages_until_current_turn_finishes() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = Arc::new(QueuedUserMessageProvider::new());
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                permission_rules: PermissionRuleSet::default(),
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "first".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        loop {
            match tokio::time::timeout(TokioDuration::from_secs(5), output.recv()).await {
                Ok(Some(CoreOutput::StreamDelta { delta, .. })) if delta == "first reply" => {
                    break;
                }
                Ok(Some(CoreOutput::SessionError { error, .. })) => {
                    panic!("session error before first stream blocked: {error:?}");
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("output channel closed before first stream blocked"),
                Err(_) => panic!("timed out waiting for first stream delta"),
            }
        }

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "second".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        tokio::time::sleep(TokioDuration::from_millis(100)).await;
        provider.release_first_turn();

        let mut completed_turns = 0;
        let mut first_turn_complete = false;
        let mut completed_text = Vec::new();

        while completed_turns < 2 {
            match tokio::time::timeout(TokioDuration::from_secs(5), output.recv()).await {
                Ok(Some(CoreOutput::TextComplete { full_text, .. })) => {
                    if full_text == "second reply" {
                        assert!(
                            first_turn_complete,
                            "queued user message completed before the active turn completed"
                        );
                    }
                    completed_text.push(full_text);
                }
                Ok(Some(CoreOutput::TurnComplete {
                    session_id: observed,
                    ..
                })) if observed == session_id => {
                    completed_turns += 1;
                    if completed_turns == 1 {
                        first_turn_complete = true;
                    }
                }
                Ok(Some(CoreOutput::SessionError { error, .. })) => {
                    panic!("session error while draining queued user messages: {error:?}");
                }
                Ok(Some(_)) => {}
                Ok(None) => panic!("output channel closed while draining queued user messages"),
                Err(_) => panic!("timed out waiting for queued user message to complete"),
            }
        }

        assert_eq!(completed_text, vec!["first reply", "second reply"]);

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn tool_call_executed_in_core_after_permission_approval() {
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenTextProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    // Ask to read a file that we'll create in the test
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_1".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "echo hello_world"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Done!".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let provider = Arc::new(ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                permission_rules: PermissionRuleSet::default(),
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap(); // SessionStateChanged(Idle)

        // Send user message
        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "run echo".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        // Collect events until TurnComplete
        let mut got_tool_request = false;
        let mut got_interaction_needed = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(event)) => match event {
                    CoreOutput::ToolRequest { tool_name, .. } => {
                        assert_eq!(tool_name, "bash");
                        got_tool_request = true;
                    }
                    CoreOutput::InteractionNeeded { .. } => {
                        got_interaction_needed = true;
                        harness
                            .input
                            .send(CoreInput::InteractionResponse {
                                session_id,
                                response: InteractionResponse {
                                    response: "approve once".into(),
                                    selected_indices: vec![0],
                                },
                            })
                            .await
                            .unwrap();
                    }
                    CoreOutput::TextComplete { full_text, .. } => {
                        assert_eq!(full_text, "Done!");
                        got_text_complete = true;
                    }
                    CoreOutput::TurnComplete { .. } => {
                        got_turn_complete = true;
                        break;
                    }
                    _ => {} // SessionStateChanged, StreamDelta, etc.
                },
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for events"),
            }
        }

        assert!(got_tool_request, "should have received ToolRequest");
        assert!(got_interaction_needed, "should have required approval");
        assert!(got_text_complete, "should have received TextComplete");
        assert!(got_turn_complete, "should have received TurnComplete");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn stop_signal_interrupts_running_tool_and_skips_remaining_tool_calls() {
        struct TwoToolProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for TwoToolProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_blocking".into(),
                            tool_name: "read_probe_one".into(),
                            arguments: serde_json::json!({}),
                        }),
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_marker".into(),
                            tool_name: "read_probe_two".into(),
                            arguments: serde_json::json!({}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "should not happen".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let provider: Arc<dyn LlmProvider> = Arc::new(TwoToolProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let started = Arc::new(Barrier::new(2));
        let marker_ran = Arc::new(AtomicBool::new(false));
        let session_id = SessionId::new();
        let mut session = SessionContext::new(
            session_id,
            SessionInit {
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: vec![Message::user("run tools")],
                archive_root: std::env::temp_dir().join("quine-core-interrupt-tests"),
                max_context_window: None,
                prompt_memory_mode: PromptMemoryMode::Disabled,
                agent_key: None,
                team_key: None,
                memory_policy: MemoryPolicyConfig::default(),
                model_profile: None,
                session_group: None,
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
            },
            &provider,
        )
        .await
        .unwrap();
        session
            .tool_registry
            .register(Arc::new(CancellableProbeTool {
                name: "read_probe_one",
                started: Arc::clone(&started),
                cancelled: Arc::new(AtomicBool::new(false)),
            }));
        session.tool_registry.register(Arc::new(MarkerProbeTool {
            name: "read_probe_two",
            ran: Arc::clone(&marker_ran),
        }));
        session.tools = session.tool_registry.tool_definitions();
        session.state = SessionState::Streaming;

        let mut sessions = HashMap::from([(session_id, session)]);
        let (output_tx, mut output_rx) = tokio::sync::mpsc::channel(32);
        let (input_tx, mut input_rx) = tokio::sync::mpsc::channel(8);
        let mut deferred_inputs = VecDeque::new();
        let mut io = CoreIo {
            output: &output_tx,
            input: &mut input_rx,
            input_tx: &input_tx,
            deferred_inputs: &mut deferred_inputs,
        };
        let mut session_tree = SessionTree::new();
        let mut engine = EngineState {
            provider: &provider,
            session_tree: &mut session_tree,
        };

        let send_stop = async {
            started.wait().await;
            input_tx
                .send(CoreInput::Signal {
                    session_id,
                    signal: SessionSignal::Stop,
                })
                .await
                .unwrap();
        };
        let (outcome, ()) = tokio::join!(
            handle_llm_turn(&mut sessions, session_id, &mut io, &mut engine),
            send_stop
        );

        assert!(matches!(outcome, TurnOutcome::Cancelled));
        assert!(!marker_ran.load(Ordering::SeqCst));

        let mut saw_first_tool = false;
        let mut saw_second_tool = false;
        let mut saw_turn_complete = false;
        while let Ok(event) = output_rx.try_recv() {
            match event {
                CoreOutput::ToolRequest { tool_use_id, .. } if tool_use_id == "tc_blocking" => {
                    saw_first_tool = true;
                }
                CoreOutput::ToolRequest { tool_use_id, .. } if tool_use_id == "tc_marker" => {
                    saw_second_tool = true;
                }
                CoreOutput::TurnComplete { .. } => saw_turn_complete = true,
                _ => {}
            }
        }

        assert!(
            saw_first_tool,
            "expected first tool request before interrupt"
        );
        assert!(
            !saw_second_tool,
            "second tool should not run after interrupt"
        );
        assert!(saw_turn_complete, "interrupted turn should still complete");
    }

    #[tokio::test]
    async fn bash_tool_emits_permission_prompt_before_completion() {
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenTextProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_1".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "touch permission-prompt.txt"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Done!".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = Arc::new(ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                permission_rules: PermissionRuleSet::default(),
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "run echo".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let mut saw_interaction_needed = false;
        let mut saw_text_complete = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::InteractionNeeded { .. })) => {
                    saw_interaction_needed = true;
                    harness
                        .input
                        .send(CoreInput::InteractionResponse {
                            session_id,
                            response: InteractionResponse {
                                response: "approve once".into(),
                                selected_indices: vec![0],
                            },
                        })
                        .await
                        .unwrap();
                }
                Ok(Some(CoreOutput::TextComplete { full_text, .. })) => {
                    assert_eq!(full_text, "Done!");
                    saw_text_complete = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    saw_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for events"),
            }
        }

        assert!(saw_interaction_needed, "bash tool should require approval");
        assert!(saw_text_complete, "turn should complete after approval");
        assert!(saw_turn_complete, "turn should complete successfully");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn bash_read_only_command_completes_without_permission_prompt() {
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenTextProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_pwd".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "pwd"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Done!".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = Arc::new(ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                permission_rules: PermissionRuleSet::default(),
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "run pwd".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let mut saw_interaction_needed = false;
        let mut saw_text_complete = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::InteractionNeeded { .. })) => {
                    saw_interaction_needed = true;
                }
                Ok(Some(CoreOutput::TextComplete { full_text, .. })) => {
                    assert_eq!(full_text, "Done!");
                    saw_text_complete = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    saw_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for events"),
            }
        }

        assert!(
            !saw_interaction_needed,
            "read-only bash should not require approval"
        );
        assert!(saw_text_complete, "turn should complete without approval");
        assert!(saw_turn_complete, "turn should complete successfully");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn plan_mode_read_only_tool_completes_without_permission_prompt() {
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenTextProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_read_plan".into(),
                            tool_name: "read_file".into(),
                            arguments: serde_json::json!({"file_path": "notes.txt"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Plan complete".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let workspace = TempDir::new().unwrap();
        std::fs::write(workspace.path().join("notes.txt"), "plan-only-read").unwrap();
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = Arc::new(ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: Some(workspace.path().to_path_buf()),
                skills: Vec::new(),
                plan_mode: true,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "inspect the note".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let mut saw_interaction_needed = false;
        let mut saw_successful_read = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::InteractionNeeded { .. })) => {
                    saw_interaction_needed = true;
                }
                Ok(Some(CoreOutput::ToolResult {
                    tool_name,
                    is_error,
                    content,
                    ..
                })) if tool_name == "read_file" => {
                    assert!(!is_error, "read_file should succeed in plan mode");
                    assert!(content.contains("plan-only-read"));
                    saw_successful_read = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    saw_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for plan-mode read events"),
            }
        }

        assert!(!saw_interaction_needed, "plan-mode read should not prompt");
        assert!(saw_successful_read, "read_file should run in plan mode");
        assert!(saw_turn_complete, "plan-mode read turn should complete");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn plan_mode_mutating_bash_request_is_denied_without_running() {
        struct ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for ToolThenTextProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_plan_write".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "touch blocked.txt"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "Plan complete".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let workspace = TempDir::new().unwrap();
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = Arc::new(ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: None,
                working_directory: Some(workspace.path().to_path_buf()),
                skills: Vec::new(),
                plan_mode: true,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "try to mutate in plan mode".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let mut saw_interaction_needed = false;
        let mut saw_denied_tool_result = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::InteractionNeeded { .. })) => {
                    saw_interaction_needed = true;
                }
                Ok(Some(CoreOutput::ToolResult {
                    tool_name,
                    is_error,
                    content,
                    ..
                })) if tool_name == "bash" => {
                    assert!(is_error, "bash mutation should not run in plan mode");
                    assert!(content.contains("permission denied"));
                    saw_denied_tool_result = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    saw_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for plan-mode write events"),
            }
        }

        assert!(
            !saw_interaction_needed,
            "plan-mode mutation should deny, not prompt"
        );
        assert!(saw_denied_tool_result, "mutating bash should be denied");
        assert!(!workspace.path().join("blocked.txt").exists());
        assert!(saw_turn_complete, "plan-mode denied turn should complete");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn create_session_seeds_initial_messages_after_system_prompt() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let provider = Arc::new(MockProvider::empty());
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let session_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        let seeded_message = Message::assistant("final plan summary");
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id,
                system_prompt: Some("base prompt".into()),
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: vec![seeded_message.clone()],
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn send_and_receive_mailbox_messages() {
        let (mut harness, core) = create_channels(ChannelConfig::default());
        let provider = Arc::new(MockProvider::empty());
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let sender_id = SessionId::new();
        let receiver_id = SessionId::new();

        for session_id in [sender_id, receiver_id] {
            let (reply_tx, reply_rx) = oneshot::channel();
            harness
                .input
                .send(CoreInput::CreateSession {
                    session_id,
                    system_prompt: None,
                    working_directory: None,
                    skills: Vec::new(),
                    plan_mode: false,
                    prompt_behavior: PermissionPromptBehavior::Interactive,
                    permission_rules: PermissionRuleSet::default(),
                    initial_messages: Vec::new(),
                    agent_key: None,
                    team_key: None,
                    session_group: None,
                    memory_policy: MemoryPolicyConfig::default(),
                    session_llm: session_llm_config(provider.clone()),
                    auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                    status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                    reply: reply_tx,
                })
                .await
                .unwrap();
            assert!(reply_rx.await.unwrap().is_ok());
        }

        harness
            .input
            .send(CoreInput::SendMessage {
                from: sender_id,
                to: receiver_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        let event = loop {
            let event =
                tokio::time::timeout(std::time::Duration::from_secs(1), harness.output.recv())
                    .await
                    .expect("timeout waiting for mailbox event")
                    .expect("missing mailbox event");
            if matches!(event, CoreOutput::MessageReceived { .. }) {
                break event;
            }
        };
        match event {
            CoreOutput::MessageReceived {
                session_id,
                from,
                content,
            } => {
                assert_eq!(session_id, receiver_id);
                assert_eq!(from, sender_id);
                assert_eq!(content, "hello");
            }
            other => panic!("expected MessageReceived, got {other:?}"),
        }

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::RecvMessage {
                session_id: receiver_id,
                source: MessageSource::Session(sender_id),
                non_blocking: false,
                timeout: None,
                reply: reply_tx,
            })
            .await
            .unwrap();

        let message = tokio::time::timeout(std::time::Duration::from_secs(1), reply_rx)
            .await
            .expect("timeout waiting for recv reply")
            .expect("recv reply dropped")
            .expect("missing mailbox message");
        assert_eq!(message.from, sender_id);
        assert_eq!(message.content, "hello");

        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::RecvMessage {
                session_id: receiver_id,
                source: MessageSource::Any,
                non_blocking: true,
                timeout: None,
                reply: reply_tx,
            })
            .await
            .unwrap();
        assert!(reply_rx.await.unwrap().is_none());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn recv_message_tool_receives_mailbox_message_without_deadlock() {
        struct RecvMessageProvider {
            call_count: std::sync::atomic::AtomicU32,
        }

        #[async_trait::async_trait]
        impl LlmProvider for RecvMessageProvider {
            async fn send(
                &self,
                _messages: &[Message],
                _tools: &[ToolDefinition],
            ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
            {
                let count = self
                    .call_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let events = if count == 0 {
                    vec![
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_recv".into(),
                            tool_name: "recv_message".into(),
                            arguments: serde_json::json!({"source": "any"}),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                } else {
                    vec![
                        Ok(LlmEvent::TextDelta {
                            text: "message received".into(),
                        }),
                        Ok(LlmEvent::Done { usage: None }),
                    ]
                };
                Ok(Box::pin(futures::stream::iter(events)))
            }
        }

        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = Arc::new(RecvMessageProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let loop_handle = tokio::spawn(run_core_loop(core, provider.clone(), None));

        let receiver_id = SessionId::new();
        let sender_id = SessionId::new();
        let (reply_tx, reply_rx) = oneshot::channel();
        harness
            .input
            .send(CoreInput::CreateSession {
                session_id: receiver_id,
                system_prompt: None,
                working_directory: None,
                skills: Vec::new(),
                plan_mode: false,
                prompt_behavior: PermissionPromptBehavior::Interactive,
                permission_rules: PermissionRuleSet::default(),
                initial_messages: Vec::new(),
                agent_key: None,
                team_key: None,
                session_group: None,
                memory_policy: MemoryPolicyConfig::default(),
                session_llm: session_llm_config(provider.clone()),
                auto_compact_threshold_percent: DEFAULT_AUTO_COMPACT_THRESHOLD_PERCENT,
                status_report_min_tool_rounds: default_status_report_min_tool_rounds(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id: receiver_id,
                content: "wait for a message".into(),
                turn_id: uuid::Uuid::new_v4().to_string(),
            })
            .await
            .unwrap();

        let mut got_tool_request = false;
        let mut got_message_received = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::ToolRequest { tool_name, .. }))
                    if tool_name == "recv_message" =>
                {
                    got_tool_request = true;
                    harness
                        .input
                        .send(CoreInput::SendMessage {
                            from: sender_id,
                            to: receiver_id,
                            content: "hello".into(),
                        })
                        .await
                        .unwrap();
                }
                Ok(Some(CoreOutput::MessageReceived {
                    session_id,
                    from,
                    content,
                })) => {
                    assert_eq!(session_id, receiver_id);
                    assert_eq!(from, sender_id);
                    assert_eq!(content, "hello");
                    got_message_received = true;
                }
                Ok(Some(CoreOutput::TextComplete { full_text, .. })) => {
                    assert_eq!(full_text, "message received");
                    got_text_complete = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    got_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for recv_message flow"),
            }
        }

        assert!(got_tool_request, "expected recv_message tool request");
        assert!(got_message_received, "expected mailbox event");
        assert!(got_text_complete, "expected post-tool text completion");
        assert!(got_turn_complete, "expected turn completion");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }
}
