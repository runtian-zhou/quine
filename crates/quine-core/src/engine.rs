use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message, MessageContent, ToolDefinition};
use tokio::sync::{mpsc, oneshot};

use crate::channel::{CoreHandle, CoreInput, CoreOutput, ToolOutcome};
use crate::compaction::{self, CompactionTrigger};
use crate::error::CoreError;
use crate::filesystem::OverlayFilesystem;
use crate::memory::{render_memory_section, MemoryService};
use crate::permission::{PermissionChecker, PermissionContext, PermissionDecision};
use crate::planner::scheduler::{get_ready_actions, render_plan};
use crate::session::{ExitStatus, SessionId, SessionSignal, SessionState};
use crate::session_tree::SessionTree;
use crate::skill::Skill;
use crate::tool::{
    ask_user::AskUserTool, bash::BashTool, find::FindTool, plan::PlanTool, read::ReadTool,
    recv_message::RecvMessageTool, send_message::SendMessageTool, signal::SignalTool,
    skill_template::SkillTemplateTool, spawn::SpawnTool, subagent::SubagentTool,
    wait_child::WaitChildTool, write::WriteTool, CancellationChannel, ExecutionContext,
    InteractionChannel, InteractionKind, InteractionRequest, InteractionResponse, ToolError,
    ToolRegistry,
};

/// Default system prompt used when no CLAUDE.md and no explicit prompt is provided.
const DEFAULT_SYSTEM_PROMPT: &str = "\
You are a helpful coding assistant. You help users with software engineering tasks \
using the tools available to you. Each message from the user is a new request — \
respond to it directly. Use tools when needed to read files, run commands, or \
write code. Be concise and accurate.";

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

/// System prompt prepended in plan mode to restrict the agent to read-only exploration.
const PLAN_MODE_SYSTEM_PROMPT: &str = "\
You are a software architect and planning specialist. Your role is to explore the \
codebase and create detailed implementation plans. You are in READ-ONLY mode.

CRITICAL CONSTRAINTS:
- You MUST NOT create, edit, delete, or modify any files
- You MUST NOT run commands that alter system state (no writes, no installs, no git commits)
- You can ONLY use read-only tools: read_file, find, bash (read-only commands like ls, cat, grep, git log)

PROCESS:
1. Understand the user's requirements
2. Explore the codebase thoroughly using read-only tools
3. Analyze existing patterns, architecture, and conventions
4. Design a solution that fits the existing codebase
5. Produce a detailed step-by-step implementation plan

YOUR PLAN MUST INCLUDE:
- Overview of the approach
- Specific files to create or modify (with paths)
- Code sketches or type signatures where helpful
- Dependencies between steps
- Critical files for implementation (3-5 key files with justifications)

Remember: You can ONLY explore and plan. You CANNOT modify any files.";

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
    /// Whether bash permission prompts should be auto-approved.
    auto_approve_permissions: bool,
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
}

struct SessionInit {
    session_id: SessionId,
    system_prompt: Option<String>,
    skills: Vec<Skill>,
    working_directory: PathBuf,
    plan_mode: bool,
    initial_messages: Vec<Message>,
    auto_approve_permissions: bool,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
    memory_service: Option<Arc<dyn MemoryService>>,
}

impl SessionContext {
    async fn new(
        init: SessionInit,
        provider: &Arc<dyn LlmProvider>,
        permission_checker: &Option<Arc<dyn PermissionChecker>>,
    ) -> Result<Self, CoreError> {
        let SessionInit {
            session_id,
            system_prompt,
            skills,
            working_directory,
            plan_mode,
            initial_messages,
            auto_approve_permissions,
            archive_root,
            max_context_window,
            memory_service,
        } = init;
        let filesystem = Arc::new(
            OverlayFilesystem::new(working_directory.clone(), working_directory.clone())
                .await
                .map_err(|e| CoreError::Internal {
                    message: format!("failed to create session filesystem: {e}"),
                })?,
        );

        let plan_store = crate::tool::plan::new_plan_store();

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Arc::new(ReadTool));
        tool_registry.register(Arc::new(BashTool));
        tool_registry.register(Arc::new(FindTool));
        tool_registry.register(Arc::new(AskUserTool));
        tool_registry.register(Arc::new(PlanTool::new(plan_store.clone())));

        if !plan_mode {
            tool_registry.register(Arc::new(WriteTool));
            tool_registry.register(Arc::new(SubagentTool::new(
                Arc::clone(provider),
                permission_checker.clone(),
            )));
            tool_registry.register(Arc::new(SpawnTool));
            tool_registry.register(Arc::new(WaitChildTool));
            tool_registry.register(Arc::new(SignalTool));
            tool_registry.register(Arc::new(SendMessageTool));
            tool_registry.register(Arc::new(RecvMessageTool));
        }

        // Register skill template tools.
        for skill in &skills {
            for tool_def in &skill.tool_definitions {
                tool_registry.register(Arc::new(SkillTemplateTool::new(tool_def.clone())));
            }
        }

        let tools = tool_registry.tool_definitions();

        // Build combined system prompt: CLAUDE.md + base + skills + default fallback.
        let applicable_memory = if let Some(service) = &memory_service {
            service
                .load_applicable(&working_directory, session_id)
                .await
                .map_err(|e| CoreError::Internal {
                    message: format!("failed to load memory: {e}"),
                })?
        } else {
            Vec::new()
        };

        let combined_prompt = {
            let mut prompt_parts = Vec::new();

            // In plan mode, prepend the read-only architect prompt.
            if plan_mode {
                prompt_parts.push(PLAN_MODE_SYSTEM_PROMPT.to_string());
            }

            // Auto-load CLAUDE.md from working directory (with parent traversal).
            if let Some(claude_md_path) = find_claude_md(&working_directory) {
                if let Ok(content) = std::fs::read_to_string(&claude_md_path) {
                    prompt_parts.push(format!(
                        "# Project Instructions (from CLAUDE.md)\n\n{content}"
                    ));
                }
            }

            if let Some(base) = &system_prompt {
                prompt_parts.push(base.clone());
            }
            for skill in &skills {
                if let Some(sp) = &skill.system_prompt {
                    prompt_parts.push(format!("\n## Skill: {}\n{}", skill.meta.name, sp));
                }
            }
            if let Some(memory_prompt) = render_memory_section(&applicable_memory) {
                prompt_parts.push(memory_prompt);
            }

            // Always ensure a system prompt exists (critical for local models).
            if prompt_parts.is_empty() {
                prompt_parts.push(DEFAULT_SYSTEM_PROMPT.to_string());
            }
            Some(prompt_parts.join("\n\n"))
        };

        let mut history = Vec::new();
        if let Some(prompt) = &combined_prompt {
            history.push(Message::system(prompt.clone()));
        }
        history.extend(initial_messages);

        Ok(Self {
            state: SessionState::Idle,
            system_prompt: combined_prompt,
            history,
            tools,
            tool_registry,
            filesystem,
            working_directory,
            auto_approve_permissions,
            pending_interaction: None,
            plan_store,
            cancel_tx: None,
            interrupted: false,
            last_input_tokens: None,
            archive_root,
            max_context_window,
            compaction_generation: 0,
        })
    }
}

/// Send the current conversation to the LLM and stream the response.
#[allow(dead_code)]
async fn call_llm(
    provider: &dyn LlmProvider,
    session: &SessionContext,
    session_id: SessionId,
    output: &tokio::sync::mpsc::Sender<CoreOutput>,
) -> Result<LlmCallResult, CoreError> {
    let messages = compaction::build_micro_compacted_history(&session.history);
    call_llm_with_messages(
        provider,
        &messages,
        &session.tools,
        session_id,
        Some(output),
    )
    .await
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
            message: e.to_string(),
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
                    message: e.to_string(),
                });
            }
        }
    }

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

async fn call_llm_interruptible(
    provider: &dyn LlmProvider,
    history: Vec<Message>,
    tools: Vec<ToolDefinition>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    sessions: &mut HashMap<SessionId, SessionContext>,
) -> Result<Option<LlmCallResult>, CoreError> {
    let send_future = provider.send(&history, &tools);
    tokio::pin!(send_future);

    let mut stream = loop {
        tokio::select! {
            stream_result = &mut send_future => {
                break stream_result.map_err(|e| CoreError::LlmError {
                    message: e.to_string(),
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
                            message: e.to_string(),
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

struct PendingToolCall {
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
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

struct PermissionWait<'a> {
    output: &'a mpsc::Sender<CoreOutput>,
    input: &'a mut mpsc::Receiver<CoreInput>,
    deferred_inputs: &'a mut VecDeque<CoreInput>,
    cancellation: &'a CancellationChannel,
}

struct CoreIo<'a> {
    output: &'a mpsc::Sender<CoreOutput>,
    input: &'a mut mpsc::Receiver<CoreInput>,
    input_tx: &'a mpsc::Sender<CoreInput>,
    deferred_inputs: &'a mut VecDeque<CoreInput>,
}

struct EngineState<'a> {
    provider: &'a Arc<dyn LlmProvider>,
    permission_checker: &'a Option<Arc<dyn PermissionChecker>>,
    session_tree: &'a mut SessionTree,
}

enum TurnOutcome {
    Completed(Option<String>),
    Failed(String),
    Cancelled,
}

enum SessionControlFlow {
    Continue,
    Interrupted,
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

fn handle_session_control_input(
    input: CoreInput,
    session_id: SessionId,
    sessions: &mut HashMap<SessionId, SessionContext>,
    deferred_inputs: &mut VecDeque<CoreInput>,
) -> SessionControlFlow {
    match input {
        CoreInput::Cancel {
            session_id: cancel_sid,
        } if cancel_sid == session_id => {
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
            interrupt_session(sessions, deferred_inputs, session_id);
            SessionControlFlow::Interrupted
        }
        other => {
            deferred_inputs.push_back(other);
            SessionControlFlow::Continue
        }
    }
}

async fn compact_session_history(
    provider: &dyn LlmProvider,
    session: &mut SessionContext,
    session_id: SessionId,
    trigger: CompactionTrigger,
) -> Result<bool, CoreError> {
    let (prefix, tail) = compaction::split_history_for_compaction(&session.history);
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
    let summary = summarize_history(provider, session_id, &archive_ref, trigger, &prefix).await?;
    session.history =
        compaction::compacted_history(&session.history, &summary, &archive_ref, &tail);
    session.last_input_tokens = None;
    session.compaction_generation = archived.generation;
    Ok(true)
}

fn session_id_string(session_id: SessionId) -> String {
    serde_json::to_value(session_id)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| "unknown-session".to_string())
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
    Ok(compaction::render_archived_tool_result(
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

async fn finalize_child_session(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_tree: &mut SessionTree,
    session_id: SessionId,
    turn_outcome: TurnOutcome,
    output: &mpsc::Sender<CoreOutput>,
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
    };

    let parent_id = session_tree.parent_of(session_id);
    session_tree.record_exit(session_id, status.clone());

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
}

#[allow(clippy::too_many_arguments)]
async fn start_child_session(
    sessions: &mut HashMap<SessionId, SessionContext>,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
    parent_id: SessionId,
    child_id: SessionId,
    task: String,
    system_prompt: Option<String>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
) -> Result<(), String> {
    if sessions.contains_key(&child_id) {
        return Err("session already exists".into());
    }

    let work_dir = std::env::current_dir().unwrap_or_default();
    let work_dir_display = work_dir.display().to_string();

    let ctx = SessionContext::new(
        SessionInit {
            session_id: child_id,
            system_prompt,
            skills: Vec::new(),
            working_directory: work_dir,
            plan_mode: false,
            initial_messages: Vec::new(),
            auto_approve_permissions: false,
            archive_root,
            max_context_window,
            memory_service: None,
        },
        engine.provider,
        engine.permission_checker,
    )
    .await
    .map_err(|e| e.to_string())?;

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
    // Schedule the child's first task for the next core-loop iteration so
    // `spawn` can acknowledge immediately instead of blocking on child completion.
    io.deferred_inputs.push_back(CoreInput::UserMessage {
        session_id: child_id,
        content: task,
    });
    Ok(())
}

async fn wait_on_child_session(
    session_tree: &mut SessionTree,
    parent_id: SessionId,
    child_id: SessionId,
    non_blocking: bool,
) -> Option<ExitStatus> {
    if session_tree.parent_of(child_id) != Some(parent_id) {
        return None;
    }

    let (reply_tx, reply_rx) = oneshot::channel();
    let already_exited = session_tree.register_waiter(child_id, reply_tx);
    if already_exited || !non_blocking {
        reply_rx.await.ok()
    } else {
        None
    }
}

/// Check permissions for a tool call, optionally requesting user confirmation.
///
/// Returns `Ok(())` if the tool is allowed to proceed, or `Err(ToolOutcome)` with
/// the denial/cancellation outcome.
async fn check_permission(
    checker: &dyn PermissionChecker,
    call: &PendingToolCall,
    session: &SessionContext,
    session_id: SessionId,
    wait: PermissionWait<'_>,
) -> Result<(), ToolOutcome> {
    let context = PermissionContext {
        session_id,
        working_directory: session.working_directory.clone(),
    };

    debug_log_session(
        session_id,
        format!(
            "permission check requested for tool `{}` with args {}",
            call.tool_name,
            serde_json::to_string(&call.arguments).unwrap_or_default()
        ),
    );

    let decision = match checker
        .check(&call.tool_name, &call.arguments, &context)
        .await
    {
        Ok(d) => d,
        Err(e) => {
            // On checker error, default to requiring confirmation
            PermissionDecision::RequiresConfirmation {
                risk_score: 0.5,
                reason: format!("permission checker error: {e}"),
            }
        }
    };

    match decision {
        PermissionDecision::Allow => {
            debug_log_session(
                session_id,
                format!("permission allowed for tool `{}`", call.tool_name),
            );
            Ok(())
        }
        PermissionDecision::Deny { reason } => {
            debug_log_session(
                session_id,
                format!("permission denied for tool `{}`: {reason}", call.tool_name),
            );
            Err(ToolOutcome::Error {
                message: format!("permission denied: {reason}"),
            })
        }
        PermissionDecision::RequiresConfirmation { risk_score, reason } => {
            debug_log_session(
                session_id,
                format!(
                    "permission confirmation required for tool `{}` (risk {:.1}): {}",
                    call.tool_name, risk_score, reason
                ),
            );
            let prompt = format!(
                "Tool `{}` with args `{}` scored {:.1} risk: {}. Allow? [y/N]",
                call.tool_name,
                serde_json::to_string(&call.arguments).unwrap_or_default(),
                risk_score,
                reason
            );

            let request = InteractionRequest {
                prompt,
                kind: InteractionKind::Confirmation,
                options: Vec::new(),
                allow_freeform: false,
                source_label: None,
            };

            let _ = wait
                .output
                .send(CoreOutput::InteractionNeeded {
                    session_id,
                    request,
                })
                .await;

            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                tokio::select! {
                    _ = wait.cancellation.cancelled() => {
                        debug_log_session(session_id, "permission check cancelled");
                        return Err(ToolOutcome::Cancelled);
                    }
                    recv = tokio::time::timeout_at(deadline, wait.input.recv()) => {
                        match recv {
                            Ok(Some(CoreInput::InteractionResponse {
                                session_id: resp_sid,
                                response,
                            })) if resp_sid == session_id => {
                                let answer = response.response.trim().to_lowercase();
                                if answer == "y" || answer == "yes" {
                                    debug_log_session(
                                        session_id,
                                        format!("permission confirmed by user for tool `{}`", call.tool_name),
                                    );
                                    return Ok(());
                                }
                                debug_log_session(
                                    session_id,
                                    format!("permission rejected by user for tool `{}`", call.tool_name),
                                );
                                return Err(ToolOutcome::Error {
                                    message: format!("permission denied by user: {reason}"),
                                });
                            }
                            Ok(Some(CoreInput::Cancel {
                                session_id: cancel_sid,
                            })) if cancel_sid == session_id => {
                                debug_log_session(session_id, "permission check interrupted by cancel input");
                                return Err(ToolOutcome::Cancelled);
                            }
                            Ok(Some(CoreInput::Signal { session_id: signal_sid, signal }))
                                if signal_sid == session_id
                                    && matches!(signal, SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill) =>
                            {
                                debug_log_session(session_id, "permission check interrupted by session signal");
                                return Err(ToolOutcome::Cancelled);
                            }
                            Ok(Some(other)) => {
                                wait.deferred_inputs.push_back(other);
                                continue;
                            }
                            Ok(None) => {
                                debug_log_session(session_id, "permission check failed: input channel closed");
                                return Err(ToolOutcome::Error {
                                    message: "input channel closed during permission check".into(),
                                });
                            }
                            Err(_) => {
                                debug_log_session(session_id, "permission check timed out waiting for response");
                                return Err(ToolOutcome::Error {
                                    message: format!(
                                        "permission check timed out (no response within 30s): {reason}"
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Execute a tool call directly within the core.
///
/// For interactive tools, sets up a channel and emits `InteractionNeeded`.
/// Returns the tool result as a `ToolOutcome`.
async fn execute_tool_call(
    call: &PendingToolCall,
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) -> ToolOutcome {
    let (cancel_tx, cancellation) = CancellationChannel::new_pair();
    let auto_approve_permissions = {
        let Some(session) = sessions.get_mut(&session_id) else {
            return ToolOutcome::Error {
                message: "session not found".into(),
            };
        };
        if session.interrupted {
            return ToolOutcome::Cancelled;
        }
        let auto_approve_permissions = session.auto_approve_permissions;
        session.cancel_tx = Some(cancel_tx.clone());
        auto_approve_permissions
    };
    debug_log_session(
        session_id,
        format!("starting tool execution for `{}`", call.tool_name),
    );

    // Only check permissions for bash tool — other tools are safe by design.
    if call.tool_name == "bash" {
        if let Some(checker) = engine
            .permission_checker
            .as_deref()
            .filter(|_| !auto_approve_permissions)
        {
            let permission_outcome = {
                let Some(session) = sessions.get(&session_id) else {
                    return ToolOutcome::Error {
                        message: "session not found".into(),
                    };
                };
                check_permission(
                    checker,
                    call,
                    session,
                    session_id,
                    PermissionWait {
                        output: io.output,
                        input: io.input,
                        deferred_inputs: io.deferred_inputs,
                        cancellation: &cancellation,
                    },
                )
                .await
            };
            if let Err(outcome) = permission_outcome {
                if matches!(outcome, ToolOutcome::Cancelled) {
                    interrupt_session(sessions, io.deferred_inputs, session_id);
                }
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.cancel_tx = None;
                }
                return outcome;
            }
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
        let (archive_root, max_context_window) = match sessions.get(&session_id) {
            Some(session) => (session.archive_root.clone(), session.max_context_window),
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

        let status =
            wait_on_child_session(engine.session_tree, session_id, child_id, non_blocking).await;
        if let Some(session) = sessions.get_mut(&session_id) {
            session.cancel_tx = None;
        }
        return match status {
            Some(status) => ToolOutcome::Success {
                output: serde_json::to_string(&status).unwrap_or_else(|_| "unknown".into()),
            },
            None => ToolOutcome::Success {
                output: "null".into(),
            },
        };
    }

    let (tool, filesystem, working_directory, plan_store, cancellation) = {
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
                 to mark it as completed.",
                prompt_parts.join("; ")
            );
            session.history.push(Message::user(prompt));
        }
    }
}

/// Handle the result of an LLM turn: either complete with text or process tool calls.
///
/// This function handles the tool execution loop: when the LLM requests tools,
/// it executes them and calls the LLM again until the LLM produces text.
async fn handle_llm_turn(
    sessions: &mut HashMap<SessionId, SessionContext>,
    session_id: SessionId,
    io: &mut CoreIo<'_>,
    engine: &mut EngineState<'_>,
) -> TurnOutcome {
    let turn_start = std::time::Instant::now();
    let mut accumulated_usage: Option<quine_llm::TokenUsage> = None;
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
            let _ = io
                .output
                .send(CoreOutput::SessionError { session_id, error })
                .await;
            session.state = SessionState::Idle;
            return TurnOutcome::Failed("session error".into());
        }

        let Some(should_auto_compact) = sessions.get(&session_id).map(|session| {
            compaction::should_auto_compact(session.max_context_window, session.last_input_tokens)
        }) else {
            return TurnOutcome::Failed("session not found".into());
        };
        if should_auto_compact {
            let Some(session) = sessions.get_mut(&session_id) else {
                return TurnOutcome::Failed("session not found".into());
            };
            if let Err(error) = compact_session_history(
                &**engine.provider,
                session,
                session_id,
                CompactionTrigger::Auto,
            )
            .await
            {
                let _ = io
                    .output
                    .send(CoreOutput::SessionError { session_id, error })
                    .await;
                session.state = SessionState::Idle;
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
                    usage: accumulated_usage.clone(),
                })
                .await;
            return TurnOutcome::Cancelled;
        }
        let history = session.history.clone();
        let tools = session.tools.clone();
        match call_llm_interruptible(&**engine.provider, history, tools, session_id, io, sessions)
            .await
        {
            Ok(None) => {
                debug_log_session(session_id, "LLM turn interrupted");
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
                        usage: accumulated_usage.clone(),
                    })
                    .await;
                return TurnOutcome::Cancelled;
            }
            Ok(Some(LlmCallResult {
                turn: LlmTurnResult::Text(full_text),
                usage,
            })) => {
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
                    session.history.push(Message::assistant(&full_text));
                }
                debug_log_session(
                    session_id,
                    format!(
                        "LLM turn completed with text output ({} chars)",
                        full_text.len()
                    ),
                );

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
                        usage: accumulated_usage,
                    })
                    .await;
                return TurnOutcome::Completed(Some(full_text));
            }
            Ok(Some(LlmCallResult {
                turn: LlmTurnResult::ToolCalls { text_before, calls },
                usage,
            })) => {
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
                }

                let debug = debug_enabled();
                debug_log_session(
                    session_id,
                    format!("LLM requested {} tool call(s)", calls.len()),
                );

                // Execute each tool call directly
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
                                usage: accumulated_usage.clone(),
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

                    // Emit ToolRequest for informational purposes
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
                    let result = execute_tool_call(call, sessions, session_id, io, engine).await;
                    let tool_duration_us = tool_start.elapsed().as_micros() as u64;

                    // Append tool result to history
                    let (tool_output, is_error) = match &result {
                        ToolOutcome::Success { output } => (output.clone(), false),
                        ToolOutcome::Error { message } => (message.clone(), true),
                        ToolOutcome::Cancelled => {
                            ("Tool execution was cancelled".to_string(), true)
                        }
                    };

                    // Emit ToolResult with timing info
                    let _ = io
                        .output
                        .send(CoreOutput::ToolResult {
                            session_id,
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            content: tool_output.clone(),
                            is_error,
                            duration_us: tool_duration_us,
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
                            format!("tool `{}` result ({status}): {}", call.tool_name, preview),
                        );
                    }

                    if let Some(session) = sessions.get_mut(&session_id) {
                        let history_output = match prepare_tool_result_for_history(
                            session,
                            session_id,
                            &call.tool_use_id,
                            &call.tool_name,
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
                                    .send(CoreOutput::SessionError {
                                        session_id,
                                        error: error.clone(),
                                    })
                                    .await;
                                let _ = io
                                    .output
                                    .send(CoreOutput::TurnComplete {
                                        session_id,
                                        duration_us,
                                        usage: accumulated_usage.clone(),
                                    })
                                    .await;
                                return TurnOutcome::Failed(error.to_string());
                            }
                        };
                        session.history.push(Message::tool_result(
                            &call.tool_use_id,
                            &history_output,
                            is_error,
                        ));
                    }

                    if matches!(result, ToolOutcome::Cancelled) {
                        debug_log_session(
                            session_id,
                            "LLM turn aborted because tool execution was cancelled",
                        );
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.cancel_tx = None;
                            session.state = SessionState::Idle;
                        }
                        let duration_us = turn_start.elapsed().as_micros() as u64;
                        let _ = io
                            .output
                            .send(CoreOutput::TurnComplete {
                                session_id,
                                duration_us,
                                usage: accumulated_usage.clone(),
                            })
                            .await;
                        return TurnOutcome::Cancelled;
                    }

                    // Plan execution integration: after update_plan, emit
                    // progress and inject prompts for newly ready actions.
                    if call.tool_name == "plan" {
                        let is_update = call.arguments.get("operation").and_then(|v| v.as_str())
                            == Some("update_plan");

                        if is_update {
                            if let Some(plan_id_str) =
                                call.arguments.get("plan_id").and_then(|v| v.as_str())
                            {
                                if let Some(action_id_str) =
                                    call.arguments.get("action_id").and_then(|v| v.as_str())
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

                // Call LLM again with tool results
                debug_log_session(session_id, "continuing LLM turn after tool results");
                continue;
            }
            Err(error) => {
                debug_log_session(session_id, format!("LLM turn failed: {error}"));
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
                }
                let _ = io
                    .output
                    .send(CoreOutput::SessionError { session_id, error })
                    .await;
                return TurnOutcome::Failed("session error".into());
            }
        }
    }
}

/// Run the core event loop, processing inputs and emitting outputs.
///
/// The `provider` is used to send conversation history to the LLM and
/// stream back responses. Tools are executed directly within the core.
pub async fn run_core_loop(
    handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    permission_checker: Option<Arc<dyn PermissionChecker>>,
) {
    run_core_loop_with_compaction(
        handle,
        provider,
        permission_checker,
        None,
        std::env::temp_dir().join("quine-core-compactions"),
        None,
    )
    .await;
}

pub async fn run_core_loop_with_compaction(
    mut handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    permission_checker: Option<Arc<dyn PermissionChecker>>,
    memory_service: Option<Arc<dyn MemoryService>>,
    archive_root: PathBuf,
    max_context_window: Option<u64>,
) {
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();
    let mut session_tree = SessionTree::new();
    let mut deferred_inputs = VecDeque::new();
    debug_log("core event loop started");

    loop {
        let input = match deferred_inputs.pop_front() {
            Some(input) => input,
            None => match handle.input.recv().await {
                Some(input) => input,
                None => break,
            },
        };
        match input {
            CoreInput::CreateSession {
                session_id,
                system_prompt,
                working_directory,
                skills,
                plan_mode,
                auto_approve_permissions,
                initial_messages,
                reply,
            } => {
                debug_log_session(
                    session_id,
                    format!(
                        "received CreateSession (plan_mode={}, auto_approve_permissions={}, skills={})",
                        plan_mode,
                        auto_approve_permissions,
                        skills.len()
                    ),
                );
                if sessions.contains_key(&session_id) {
                    let _ = reply.send(Err("session already exists".into()));
                    continue;
                }

                let work_dir = working_directory
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
                let work_dir_display = work_dir.display().to_string();

                match SessionContext::new(
                    SessionInit {
                        session_id,
                        system_prompt,
                        skills,
                        working_directory: work_dir,
                        plan_mode,
                        initial_messages,
                        auto_approve_permissions,
                        archive_root: archive_root.clone(),
                        max_context_window,
                        memory_service: memory_service.clone(),
                    },
                    &provider,
                    &permission_checker,
                )
                .await
                {
                    Ok(ctx) => {
                        debug_log_session(
                            session_id,
                            format!(
                                "session created with working_directory={}",
                                work_dir_display
                            ),
                        );
                        sessions.insert(session_id, ctx);
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Idle,
                            })
                            .await;
                        let _ = reply.send(Ok(()));
                    }
                    Err(e) => {
                        debug_log_session(session_id, format!("session creation failed: {e}"));
                        let _ = reply.send(Err(format!("failed to create session: {e}")));
                    }
                }
            }

            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                debug_log_session(
                    session_id,
                    format!("received UserMessage ({} chars)", content.len()),
                );
                if sessions.contains_key(&session_id) {
                    {
                        let session = sessions.get_mut(&session_id).unwrap();
                        session.interrupted = false;
                        session.state = SessionState::Streaming;
                        session.history.push(Message::user(&content));
                    }
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Streaming,
                        })
                        .await;
                    let mut io = CoreIo {
                        output: &handle.output,
                        input: &mut handle.input,
                        input_tx: &handle.input_tx,
                        deferred_inputs: &mut deferred_inputs,
                    };
                    let mut engine = EngineState {
                        provider: &provider,
                        permission_checker: &permission_checker,
                        session_tree: &mut session_tree,
                    };
                    let turn_outcome =
                        handle_llm_turn(&mut sessions, session_id, &mut io, &mut engine).await;
                    if session_tree.parent_of(session_id).is_some() {
                        finalize_child_session(
                            &mut sessions,
                            &mut session_tree,
                            session_id,
                            turn_outcome,
                            &handle.output,
                        )
                        .await;
                    }
                } else {
                    debug_log_session(session_id, "user message targeted unknown session");
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionError {
                            session_id,
                            error: CoreError::SessionNotFound,
                        })
                        .await;
                }
            }

            CoreInput::CompactSession { session_id, reply } => {
                debug_log_session(session_id, "received CompactSession request");
                let result = if let Some(session) = sessions.get_mut(&session_id) {
                    compact_session_history(
                        provider.as_ref(),
                        session,
                        session_id,
                        CompactionTrigger::Manual,
                    )
                    .await
                    .map(|_| ())
                    .map_err(|error| error.to_string())
                } else {
                    Err("session not found".into())
                };
                let _ = reply.send(result);
            }

            CoreInput::ToolResult {
                session_id,
                tool_use_id,
                result,
            } => {
                debug_log_session(
                    session_id,
                    format!("received external ToolResult for tool_use_id={tool_use_id}"),
                );
                // Legacy path: the harness sent a tool result.
                // With the new architecture, tools are executed in-core,
                // but we still accept external tool results for backward compat.
                if let Some(actual_state) = sessions.get(&session_id).map(|s| s.state) {
                    if actual_state != SessionState::AwaitingToolResult {
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::InvalidState {
                                    expected: SessionState::AwaitingToolResult,
                                    actual: actual_state,
                                },
                            })
                            .await;
                    } else {
                        let (output_text, is_error) = match &result {
                            ToolOutcome::Success { output } => (output.clone(), false),
                            ToolOutcome::Error { message } => (message.clone(), true),
                            ToolOutcome::Cancelled => {
                                ("Tool execution was cancelled".to_string(), true)
                            }
                        };
                        let history_output = {
                            let session = sessions.get(&session_id).unwrap();
                            match prepare_tool_result_for_history(
                                session,
                                session_id,
                                &tool_use_id,
                                "external",
                                &output_text,
                                is_error,
                            )
                            .await
                            {
                                Ok(output) => output,
                                Err(error) => {
                                    let _ = handle
                                        .output
                                        .send(CoreOutput::SessionError { session_id, error })
                                        .await;
                                    continue;
                                }
                            }
                        };
                        {
                            let session = sessions.get_mut(&session_id).unwrap();
                            session.history.push(Message::tool_result(
                                &tool_use_id,
                                &history_output,
                                is_error,
                            ));
                            session.state = SessionState::Streaming;
                        }
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;

                        let mut io = CoreIo {
                            output: &handle.output,
                            input: &mut handle.input,
                            input_tx: &handle.input_tx,
                            deferred_inputs: &mut deferred_inputs,
                        };
                        let mut engine = EngineState {
                            provider: &provider,
                            permission_checker: &permission_checker,
                            session_tree: &mut session_tree,
                        };
                        let turn_outcome =
                            handle_llm_turn(&mut sessions, session_id, &mut io, &mut engine).await;
                        if session_tree.parent_of(session_id).is_some() {
                            finalize_child_session(
                                &mut sessions,
                                &mut session_tree,
                                session_id,
                                turn_outcome,
                                &handle.output,
                            )
                            .await;
                        }
                    }
                } else {
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionError {
                            session_id,
                            error: CoreError::SessionNotFound,
                        })
                        .await;
                }
            }

            CoreInput::InteractionResponse { .. } => {
                debug_log("received unhandled InteractionResponse at top-level core loop");
                // Interaction responses are handled within execute_tool_call.
                // If we get one here, it means no tool is waiting for it.
            }

            CoreInput::Cancel { session_id } => {
                debug_log_session(session_id, "received Cancel");
                if sessions.contains_key(&session_id) {
                    interrupt_session(&mut sessions, &mut deferred_inputs, session_id);
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Idle,
                        })
                        .await;
                }
            }

            CoreInput::Shutdown => {
                debug_log("received Shutdown; exiting core event loop");
                break;
            }

            CoreInput::SpawnSession {
                parent_id,
                child_id,
                task,
                system_prompt,
                inheritance,
                reply,
            } => {
                debug_log_session(
                    parent_id,
                    format!(
                        "received SpawnSession for child={child_id:?} (task_len={}, inherit_history={}, inherit_filesystem={}, has_system_prompt={})",
                        task.len(),
                        inheritance.history,
                        inheritance.filesystem,
                        system_prompt.is_some()
                    ),
                );
                debug_log_session(
                    child_id,
                    format!("received SpawnSession for task ({} chars)", task.len()),
                );
                if sessions.contains_key(&child_id) {
                    let _ = reply.send(Err("session already exists".into()));
                    continue;
                }

                let mut io = CoreIo {
                    output: &handle.output,
                    input: &mut handle.input,
                    input_tx: &handle.input_tx,
                    deferred_inputs: &mut deferred_inputs,
                };
                let mut engine = EngineState {
                    provider: &provider,
                    permission_checker: &permission_checker,
                    session_tree: &mut session_tree,
                };
                let result = start_child_session(
                    &mut sessions,
                    &mut io,
                    &mut engine,
                    parent_id,
                    child_id,
                    task,
                    system_prompt,
                    archive_root.clone(),
                    max_context_window,
                )
                .await;
                match result {
                    Ok(()) => {
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        debug_log_session(
                            parent_id,
                            format!(
                                "child session creation failed for child={child_id:?}: {error}"
                            ),
                        );
                        debug_log_session(
                            child_id,
                            format!("child session creation failed: {error}"),
                        );
                        let _ = reply.send(Err(error));
                    }
                }
            }
            CoreInput::Signal { session_id, signal } => {
                debug_log_session(session_id, format!("received Signal::{signal:?}"));
                match signal {
                    SessionSignal::Continue => {
                        if let Some(session) = sessions.get_mut(&session_id) {
                            session.interrupted = false;
                            if session.state == SessionState::Paused {
                                session.state = SessionState::Idle;
                            }
                            let _ = handle
                                .output
                                .send(CoreOutput::SessionStateChanged {
                                    session_id,
                                    state: session.state,
                                })
                                .await;
                        }
                    }
                    SessionSignal::Stop | SessionSignal::Term | SessionSignal::Kill => {
                        if sessions.contains_key(&session_id) {
                            interrupt_session(&mut sessions, &mut deferred_inputs, session_id);
                            let _ = handle
                                .output
                                .send(CoreOutput::SessionStateChanged {
                                    session_id,
                                    state: SessionState::Idle,
                                })
                                .await;
                        }
                    }
                }
            }
            CoreInput::SendMessage { .. } => {
                debug_log("received SendMessage; no-op in current core loop");
            }
            CoreInput::WaitSession {
                parent_id,
                child_id,
                reply,
                non_blocking,
            } => {
                let result =
                    wait_on_child_session(&mut session_tree, parent_id, child_id, non_blocking)
                        .await;
                let _ = reply.send(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{create_channels, ChannelConfig};
    use crate::permission::PermissionError;
    use crate::session::{ExitStatus, InheritanceFlags};
    use std::pin::Pin;
    use tempfile::TempDir;
    use tokio::sync::oneshot;

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

    struct ConfirmChecker;

    #[async_trait::async_trait]
    impl PermissionChecker for ConfirmChecker {
        async fn check(
            &self,
            _tool_name: &str,
            _arguments: &serde_json::Value,
            _context: &PermissionContext,
        ) -> Result<PermissionDecision, PermissionError> {
            Ok(PermissionDecision::RequiresConfirmation {
                risk_score: 0.9,
                reason: "test confirmation".into(),
            })
        }
    }

    #[tokio::test]
    async fn execute_tool_call_consumes_cancel_for_non_interactive_tool() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let permission_checker: Option<Arc<dyn PermissionChecker>> = None;
        let session = SessionContext::new(
            SessionInit {
                session_id: SessionId::new(),
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                initial_messages: Vec::new(),
                auto_approve_permissions: false,
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                memory_service: None,
            },
            &provider,
            &permission_checker,
        )
        .await
        .unwrap();

        let call = PendingToolCall {
            tool_use_id: "toolu_cancel".into(),
            tool_name: "bash".into(),
            arguments: serde_json::json!({"command": "sleep 5"}),
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
            permission_checker: &permission_checker,
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
        let permission_checker: Option<Arc<dyn PermissionChecker>> = None;
        let session = SessionContext::new(
            SessionInit {
                session_id: SessionId::new(),
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                initial_messages: Vec::new(),
                auto_approve_permissions: false,
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                memory_service: None,
            },
            &provider,
            &permission_checker,
        )
        .await
        .unwrap();

        let call = PendingToolCall {
            tool_use_id: "toolu_cancel".into(),
            tool_name: "bash".into(),
            arguments: serde_json::json!({"command": "sleep 5"}),
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
            permission_checker: &permission_checker,
            session_tree: &mut session_tree,
        };
        let result =
            execute_tool_call(&call, &mut sessions, session_id, &mut io, &mut engine).await;

        assert!(matches!(result, ToolOutcome::Cancelled));
        assert!(matches!(
            deferred_inputs.pop_front(),
            Some(CoreInput::UserMessage {
                session_id,
                content
            }) if session_id == other_session_id && content == "hello"
        ));
    }

    #[tokio::test]
    async fn execute_tool_call_consumes_stop_signal_for_non_interactive_tool() {
        let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider::empty());
        let permission_checker: Option<Arc<dyn PermissionChecker>> = None;
        let session = SessionContext::new(
            SessionInit {
                session_id: SessionId::new(),
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                initial_messages: Vec::new(),
                auto_approve_permissions: false,
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                memory_service: None,
            },
            &provider,
            &permission_checker,
        )
        .await
        .unwrap();

        let call = PendingToolCall {
            tool_use_id: "toolu_signal_cancel".into(),
            tool_name: "bash".into(),
            arguments: serde_json::json!({"command": "sleep 5"}),
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
            permission_checker: &permission_checker,
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
        let permission_checker: Option<Arc<dyn PermissionChecker>> = None;
        let session = SessionContext::new(
            SessionInit {
                session_id: SessionId::new(),
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                initial_messages: Vec::new(),
                auto_approve_permissions: false,
                archive_root: std::env::temp_dir().join("quine-core-compaction-tests"),
                max_context_window: None,
                memory_service: None,
            },
            &provider,
            &permission_checker,
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
        let permission_checker: Option<Arc<dyn PermissionChecker>> = None;
        let archive_root = std::env::temp_dir().join(format!(
            "quine-core-compaction-tests-{:?}",
            SessionId::new()
        ));
        let session = SessionContext::new(
            SessionInit {
                session_id: SessionId::new(),
                system_prompt: None,
                skills: Vec::new(),
                working_directory: std::env::current_dir().unwrap_or_default(),
                plan_mode: false,
                initial_messages: Vec::new(),
                auto_approve_permissions: false,
                archive_root: archive_root.clone(),
                max_context_window: None,
                memory_service: None,
            },
            &provider,
            &permission_checker,
        )
        .await
        .unwrap();
        let session_id = SessionId::new();
        let oversized = "x".repeat(compaction::MAX_TOOL_RESULT_CHARS_IN_HISTORY + 128);

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
        assert!(output.contains("[... elided ...]"));

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
        let loop_handle = tokio::spawn(run_core_loop(core, provider, None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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
            })
            .await
            .unwrap();

        let status = tokio::time::timeout(std::time::Duration::from_secs(10), wait_reply_rx)
            .await
            .expect("wait_child should not deadlock")
            .unwrap()
            .expect("child should exit successfully");

        match status {
            ExitStatus::Success { output } => {
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
    async fn create_session_and_shutdown() {
        let (harness, core) = create_channels(ChannelConfig::default());

        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(MockProvider::empty()), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

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
    async fn user_message_produces_turn_complete() {
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;

        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(MockProvider::empty()), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
                reply: reply_tx,
            })
            .await
            .unwrap();
        reply_rx.await.unwrap().unwrap();

        // Drain the SessionStateChanged from creation
        let _ = output.recv().await.unwrap();

        harness
            .input
            .send(CoreInput::UserMessage {
                session_id,
                content: "hello".into(),
            })
            .await
            .unwrap();

        // Expect: SessionStateChanged(Streaming), TextComplete, TurnComplete
        let event = output.recv().await.unwrap();
        assert!(matches!(
            event,
            CoreOutput::SessionStateChanged {
                state: SessionState::Streaming,
                ..
            }
        ));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TextComplete { .. }));

        let event = output.recv().await.unwrap();
        assert!(matches!(event, CoreOutput::TurnComplete { .. }));

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_session_id_returns_error() {
        let (harness, core) = create_channels(ChannelConfig::default());

        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(MockProvider::empty()), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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

        let provider = MockProvider::new("Hello from the LLM!");
        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(provider), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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
                content: "hello".into(),
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
    async fn tool_call_executed_in_core() {
        // Provider that returns a tool call for read_file on first send, then text on second
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

        let provider = ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        };
        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(provider), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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
            })
            .await
            .unwrap();

        // Collect events until TurnComplete
        let mut got_tool_request = false;
        let mut got_text_complete = false;
        let mut got_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(event)) => match event {
                    CoreOutput::ToolRequest { tool_name, .. } => {
                        assert_eq!(tool_name, "bash");
                        got_tool_request = true;
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
        assert!(got_text_complete, "should have received TextComplete");
        assert!(got_turn_complete, "should have received TurnComplete");

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn stop_signal_interrupts_running_tool_and_skips_remaining_tool_calls() {
        struct TwoToolProvider {
            call_count: std::sync::atomic::AtomicU32,
            marker_path: std::path::PathBuf,
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
                            tool_use_id: "tc_sleep".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({"command": "sleep 5"}),
                        }),
                        Ok(LlmEvent::ToolCall {
                            tool_use_id: "tc_marker".into(),
                            tool_name: "bash".into(),
                            arguments: serde_json::json!({
                                "command": format!(
                                    "python -c \"from pathlib import Path; Path(r#\\\"{}\\\"#).write_text('ran')\"",
                                    self.marker_path.display()
                                )
                            }),
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

        let temp_dir = TempDir::new().unwrap();
        let marker_path = temp_dir.path().join("interrupt-marker.txt");
        let (harness, core) = create_channels(ChannelConfig::default());
        let mut output = harness.output;
        let provider = TwoToolProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
            marker_path: marker_path.clone(),
        };
        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(provider), None));

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
                auto_approve_permissions: false,
                initial_messages: Vec::new(),
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
                content: "run tools".into(),
            })
            .await
            .unwrap();

        let mut saw_sleep_tool = false;
        let mut saw_marker_tool = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::ToolRequest { tool_use_id, .. }))
                    if tool_use_id == "tc_sleep" =>
                {
                    saw_sleep_tool = true;
                    harness
                        .input
                        .send(CoreInput::Signal {
                            session_id,
                            signal: SessionSignal::Stop,
                        })
                        .await
                        .unwrap();
                }
                Ok(Some(CoreOutput::ToolRequest { tool_use_id, .. }))
                    if tool_use_id == "tc_marker" =>
                {
                    saw_marker_tool = true;
                }
                Ok(Some(CoreOutput::TurnComplete { .. })) => {
                    saw_turn_complete = true;
                    break;
                }
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(_) => panic!("timeout waiting for interrupt flow events"),
            }
        }

        assert!(
            saw_sleep_tool,
            "expected first tool request before interrupt"
        );
        assert!(
            !saw_marker_tool,
            "second tool should not run after interrupt"
        );
        assert!(saw_turn_complete, "interrupted turn should still complete");
        assert!(
            !marker_path.exists(),
            "remaining pending tool work should not run after interrupt"
        );

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }

    #[tokio::test]
    async fn auto_approve_session_bypasses_permission_prompt() {
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
        let checker: Arc<dyn PermissionChecker> = Arc::new(ConfirmChecker);
        let provider = ToolThenTextProvider {
            call_count: std::sync::atomic::AtomicU32::new(0),
        };
        let loop_handle = tokio::spawn(run_core_loop(core, Arc::new(provider), Some(checker)));

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
                auto_approve_permissions: true,
                initial_messages: Vec::new(),
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
            })
            .await
            .unwrap();

        let mut saw_interaction_needed = false;
        let mut saw_turn_complete = false;

        loop {
            match tokio::time::timeout(std::time::Duration::from_secs(10), output.recv()).await {
                Ok(Some(CoreOutput::InteractionNeeded { .. })) => {
                    saw_interaction_needed = true;
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
            "auto-approve session should not prompt"
        );
        assert!(saw_turn_complete, "turn should complete successfully");

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
                auto_approve_permissions: false,
                initial_messages: vec![seeded_message.clone()],
                reply: reply_tx,
            })
            .await
            .unwrap();

        assert!(reply_rx.await.unwrap().is_ok());

        harness.input.send(CoreInput::Shutdown).await.unwrap();
        loop_handle.await.unwrap();
    }
}
