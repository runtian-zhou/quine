use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
use tokio::sync::{mpsc, oneshot};

use crate::channel::{CoreHandle, CoreInput, CoreOutput, ToolOutcome};
use crate::error::CoreError;
use crate::filesystem::OverlayFilesystem;
use crate::permission::{PermissionChecker, PermissionContext, PermissionDecision};
use crate::planner::scheduler::{get_ready_actions, render_plan};
use crate::session::{SessionId, SessionState};
use crate::skill::Skill;
use crate::tool::{
    ask_user::AskUserTool, bash::BashTool, find::FindTool, plan::PlanTool, read::ReadTool,
    recv_message::RecvMessageTool, send_message::SendMessageTool, signal::SignalTool,
    skill_template::SkillTemplateTool, spawn::SpawnTool, subagent::SubagentTool,
    wait_child::WaitChildTool, write::WriteTool, ExecutionContext, InteractionChannel,
    InteractionKind, InteractionRequest, InteractionResponse, ToolRegistry,
};

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
    /// Sender for pending interaction responses (tool_use_id -> sender).
    pending_interaction: Option<oneshot::Sender<InteractionResponse>>,
    /// Shared plan store for this session.
    plan_store: crate::tool::plan::PlanStore,
}

impl SessionContext {
    async fn new(
        system_prompt: Option<String>,
        skills: Vec<Skill>,
        working_directory: PathBuf,
        provider: &Arc<dyn LlmProvider>,
        permission_checker: &Option<Arc<dyn PermissionChecker>>,
    ) -> Result<Self, CoreError> {
        let session_dir = std::env::temp_dir()
            .join("quine-sessions")
            .join(uuid::Uuid::new_v4().to_string());

        let filesystem = Arc::new(
            OverlayFilesystem::new(working_directory.clone(), session_dir)
                .await
                .map_err(|e| CoreError::Internal {
                    message: format!("failed to create overlay filesystem: {e}"),
                })?,
        );

        let plan_store = crate::tool::plan::new_plan_store();

        let mut tool_registry = ToolRegistry::new();
        tool_registry.register(Arc::new(ReadTool));
        tool_registry.register(Arc::new(WriteTool));
        tool_registry.register(Arc::new(BashTool));
        tool_registry.register(Arc::new(FindTool));
        tool_registry.register(Arc::new(AskUserTool));
        tool_registry.register(Arc::new(PlanTool::new(plan_store.clone())));
        tool_registry.register(Arc::new(SubagentTool::new(
            Arc::clone(provider),
            permission_checker.clone(),
        )));
        tool_registry.register(Arc::new(SpawnTool));
        tool_registry.register(Arc::new(WaitChildTool));
        tool_registry.register(Arc::new(SignalTool));
        tool_registry.register(Arc::new(SendMessageTool));
        tool_registry.register(Arc::new(RecvMessageTool));

        // Register skill template tools.
        for skill in &skills {
            for tool_def in &skill.tool_definitions {
                tool_registry.register(Arc::new(SkillTemplateTool::new(tool_def.clone())));
            }
        }

        let tools = tool_registry.tool_definitions();

        // Build combined system prompt from base + skill prompts.
        let combined_prompt = {
            let mut prompt_parts = Vec::new();
            if let Some(base) = &system_prompt {
                prompt_parts.push(base.clone());
            }
            for skill in &skills {
                if let Some(sp) = &skill.system_prompt {
                    prompt_parts.push(format!("\n## Skill: {}\n{}", skill.meta.name, sp));
                }
            }
            if prompt_parts.is_empty() {
                None
            } else {
                Some(prompt_parts.join("\n"))
            }
        };

        let mut history = Vec::new();
        if let Some(prompt) = &combined_prompt {
            history.push(Message::system(prompt.clone()));
        }

        Ok(Self {
            state: SessionState::Idle,
            system_prompt: combined_prompt,
            history,
            tools,
            tool_registry,
            filesystem,
            working_directory,
            pending_interaction: None,
            plan_store,
        })
    }
}

/// Send the current conversation to the LLM and stream the response.
async fn call_llm(
    provider: &dyn LlmProvider,
    session: &SessionContext,
    session_id: SessionId,
    output: &tokio::sync::mpsc::Sender<CoreOutput>,
) -> Result<LlmCallResult, CoreError> {
    let stream_result = provider
        .send(&session.history, &session.tools)
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
            Ok(LlmEvent::TextDelta { text }) => {
                full_text.push_str(&text);
                let _ = output
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

/// Check permissions for a tool call, optionally requesting user confirmation.
///
/// Returns `Ok(())` if the tool is allowed to proceed, or `Err(ToolOutcome)` with
/// the denial/cancellation outcome.
async fn check_permission(
    checker: &dyn PermissionChecker,
    call: &PendingToolCall,
    session: &SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    input: &mut mpsc::Receiver<CoreInput>,
) -> Result<(), ToolOutcome> {
    let context = PermissionContext {
        session_id,
        working_directory: session.working_directory.clone(),
    };

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
        PermissionDecision::Allow => Ok(()),
        PermissionDecision::Deny { reason } => Err(ToolOutcome::Error {
            message: format!("permission denied: {reason}"),
        }),
        PermissionDecision::RequiresConfirmation { risk_score, reason } => {
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

            let _ = output
                .send(CoreOutput::InteractionNeeded {
                    session_id,
                    request,
                })
                .await;

            // Wait for user response
            loop {
                match input.recv().await {
                    Some(CoreInput::InteractionResponse {
                        session_id: resp_sid,
                        response,
                    }) if resp_sid == session_id => {
                        let answer = response.response.trim().to_lowercase();
                        if answer == "y" || answer == "yes" {
                            return Ok(());
                        }
                        return Err(ToolOutcome::Error {
                            message: format!("permission denied by user: {reason}"),
                        });
                    }
                    Some(CoreInput::Cancel {
                        session_id: cancel_sid,
                    }) if cancel_sid == session_id => {
                        return Err(ToolOutcome::Cancelled);
                    }
                    Some(_) => continue,
                    None => {
                        return Err(ToolOutcome::Error {
                            message: "input channel closed during permission check".into(),
                        });
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
    session: &mut SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    input: &mut mpsc::Receiver<CoreInput>,
    permission_checker: Option<&dyn PermissionChecker>,
) -> ToolOutcome {
    // Only check permissions for bash tool — other tools are safe by design.
    if call.tool_name == "bash" {
        if let Some(checker) = permission_checker {
            if let Err(outcome) =
                check_permission(checker, call, session, session_id, output, input).await
            {
                return outcome;
            }
        }
    }

    let tool = match session.tool_registry.get(&call.tool_name) {
        Some(t) => Arc::clone(t),
        None => {
            return ToolOutcome::Error {
                message: format!("unknown tool: {}", call.tool_name),
            };
        }
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
            filesystem: Arc::clone(&session.filesystem),
            working_directory: session.working_directory.clone(),
            interaction_channel: Some(channel),
            plan_store: session.plan_store.clone(),
            core_input: None,
        };

        let args = call.arguments.clone();
        let mut tool_handle = tokio::spawn(async move { tool.execute(args, &ctx).await });

        let output_clone = output.clone();
        let sid = session_id;

        // Poll: either the tool finishes or it needs interaction
        loop {
            tokio::select! {
                result = &mut tool_handle => {
                    return match result {
                        Ok(Ok(tool_output)) => {
                            if tool_output.is_error {
                                ToolOutcome::Error { message: tool_output.content }
                            } else {
                                ToolOutcome::Success { output: tool_output.content }
                            }
                        }
                        Ok(Err(tool_err)) => {
                            ToolOutcome::Error { message: tool_err.to_string() }
                        }
                        Err(join_err) => {
                            ToolOutcome::Error {
                                message: format!("tool task panicked: {join_err}"),
                            }
                        }
                    };
                }
                interaction = req_rx.recv() => {
                    if let Some((request, reply_tx)) = interaction {
                        // Emit InteractionNeeded
                        let _ = output_clone.send(CoreOutput::InteractionNeeded {
                            session_id: sid,
                            request,
                        }).await;

                        // Wait for CoreInput::InteractionResponse from the harness
                        loop {
                            match input.recv().await {
                                Some(CoreInput::InteractionResponse { session_id: resp_sid, response }) if resp_sid == sid => {
                                    let _ = reply_tx.send(response);
                                    break;
                                }
                                Some(CoreInput::Cancel { session_id: cancel_sid }) if cancel_sid == sid => {
                                    // Drop the reply sender to cancel the tool
                                    drop(reply_tx);
                                    return ToolOutcome::Cancelled;
                                }
                                Some(_) => {
                                    // Ignore other messages while waiting for interaction response
                                    continue;
                                }
                                None => {
                                    return ToolOutcome::Error {
                                        message: "input channel closed".into(),
                                    };
                                }
                            }
                        }
                    }
                }
            }
        }
    } else {
        // Non-interactive tool: execute directly
        let ctx = ExecutionContext {
            session_id,
            filesystem: Arc::clone(&session.filesystem),
            working_directory: session.working_directory.clone(),
            interaction_channel: None,
            plan_store: session.plan_store.clone(),
            core_input: None,
        };

        match tool.execute(call.arguments.clone(), &ctx).await {
            Ok(tool_output) => {
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
            Err(tool_err) => ToolOutcome::Error {
                message: tool_err.to_string(),
            },
        }
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
    provider: &dyn LlmProvider,
    session: &mut SessionContext,
    session_id: SessionId,
    output: &mpsc::Sender<CoreOutput>,
    input: &mut mpsc::Receiver<CoreInput>,
    permission_checker: Option<&dyn PermissionChecker>,
) {
    let turn_start = std::time::Instant::now();
    let mut accumulated_usage: Option<quine_llm::TokenUsage> = None;

    loop {
        match call_llm(provider, session, session_id, output).await {
            Ok(LlmCallResult {
                turn: LlmTurnResult::Text(full_text),
                usage,
            }) => {
                // Accumulate usage from this LLM call.
                if let Some(u) = usage {
                    let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                    acc.input_tokens += u.input_tokens;
                    acc.output_tokens += u.output_tokens;
                }

                session.history.push(Message::assistant(&full_text));

                let _ = output
                    .send(CoreOutput::TextComplete {
                        session_id,
                        full_text,
                    })
                    .await;

                session.state = SessionState::Idle;
                let duration_ms = turn_start.elapsed().as_millis() as u64;
                let _ = output
                    .send(CoreOutput::TurnComplete {
                        session_id,
                        duration_ms,
                        usage: accumulated_usage,
                    })
                    .await;
                break;
            }
            Ok(LlmCallResult {
                turn: LlmTurnResult::ToolCalls { text_before, calls },
                usage,
            }) => {
                // Accumulate usage from this LLM call.
                if let Some(u) = usage {
                    let acc = accumulated_usage.get_or_insert(quine_llm::TokenUsage::default());
                    acc.input_tokens += u.input_tokens;
                    acc.output_tokens += u.output_tokens;
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
                session.history.push(Message::assistant_tool_use(
                    text_before.clone(),
                    tool_use_requests,
                ));

                let debug = std::env::var("QUINE_DEBUG").is_ok();

                // Execute each tool call directly
                for call in &calls {
                    if debug {
                        eprintln!(
                            "[tool] calling {} (id={}) args={}",
                            call.tool_name,
                            call.tool_use_id,
                            serde_json::to_string(&call.arguments).unwrap_or_default()
                        );
                    }

                    // Emit ToolRequest for informational purposes
                    let _ = output
                        .send(CoreOutput::ToolRequest {
                            session_id,
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            arguments: call.arguments.clone(),
                        })
                        .await;

                    let tool_start = std::time::Instant::now();
                    let result = execute_tool_call(
                        call,
                        session,
                        session_id,
                        output,
                        input,
                        permission_checker,
                    )
                    .await;
                    let tool_duration_ms = tool_start.elapsed().as_millis() as u64;

                    // Append tool result to history
                    let (tool_output, is_error) = match &result {
                        ToolOutcome::Success { output } => (output.clone(), false),
                        ToolOutcome::Error { message } => (message.clone(), true),
                        ToolOutcome::Cancelled => {
                            ("Tool execution was cancelled".to_string(), true)
                        }
                    };

                    // Emit ToolResult with timing info
                    let _ = output
                        .send(CoreOutput::ToolResult {
                            session_id,
                            tool_use_id: call.tool_use_id.clone(),
                            tool_name: call.tool_name.clone(),
                            is_error,
                            duration_ms: tool_duration_ms,
                        })
                        .await;

                    if debug {
                        let status = if is_error { "ERROR" } else { "OK" };
                        let preview = if tool_output.len() > 200 {
                            format!("{}...[{} bytes]", &tool_output[..200], tool_output.len())
                        } else {
                            tool_output.clone()
                        };
                        eprintln!("[tool] {} result ({}): {}", call.tool_name, status, preview);
                    }

                    session.history.push(Message::tool_result(
                        &call.tool_use_id,
                        &tool_output,
                        is_error,
                    ));

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
                                        session,
                                        session_id,
                                        output,
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
                continue;
            }
            Err(error) => {
                session.state = SessionState::Idle;
                let _ = output
                    .send(CoreOutput::SessionError { session_id, error })
                    .await;
                break;
            }
        }
    }
}

/// Run the core event loop, processing inputs and emitting outputs.
///
/// The `provider` is used to send conversation history to the LLM and
/// stream back responses. Tools are executed directly within the core.
pub async fn run_core_loop(
    mut handle: CoreHandle,
    provider: Arc<dyn LlmProvider>,
    permission_checker: Option<Arc<dyn PermissionChecker>>,
) {
    let mut sessions: HashMap<SessionId, SessionContext> = HashMap::new();

    while let Some(input) = handle.input.recv().await {
        match input {
            CoreInput::CreateSession {
                session_id,
                system_prompt,
                working_directory,
                skills,
                reply,
            } => {
                if sessions.contains_key(&session_id) {
                    let _ = reply.send(Err("session already exists".into()));
                    continue;
                }

                let work_dir = working_directory
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

                match SessionContext::new(
                    system_prompt,
                    skills,
                    work_dir,
                    &provider,
                    &permission_checker,
                )
                .await
                {
                    Ok(ctx) => {
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
                        let _ = reply.send(Err(format!("failed to create session: {e}")));
                    }
                }
            }

            CoreInput::UserMessage {
                session_id,
                content,
            } => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Streaming;
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Streaming,
                        })
                        .await;

                    session.history.push(Message::user(&content));

                    handle_llm_turn(
                        &*provider,
                        session,
                        session_id,
                        &handle.output,
                        &mut handle.input,
                        permission_checker.as_deref(),
                    )
                    .await;
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

            CoreInput::ToolResult {
                session_id,
                tool_use_id,
                result,
            } => {
                // Legacy path: the harness sent a tool result.
                // With the new architecture, tools are executed in-core,
                // but we still accept external tool results for backward compat.
                if let Some(session) = sessions.get_mut(&session_id) {
                    if session.state != SessionState::AwaitingToolResult {
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionError {
                                session_id,
                                error: CoreError::InvalidState {
                                    expected: SessionState::AwaitingToolResult,
                                    actual: session.state,
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
                        session.history.push(Message::tool_result(
                            &tool_use_id,
                            &output_text,
                            is_error,
                        ));

                        session.state = SessionState::Streaming;
                        let _ = handle
                            .output
                            .send(CoreOutput::SessionStateChanged {
                                session_id,
                                state: SessionState::Streaming,
                            })
                            .await;

                        handle_llm_turn(
                            &*provider,
                            session,
                            session_id,
                            &handle.output,
                            &mut handle.input,
                            permission_checker.as_deref(),
                        )
                        .await;
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
                // Interaction responses are handled within execute_tool_call.
                // If we get one here, it means no tool is waiting for it.
            }

            CoreInput::Cancel { session_id } => {
                if let Some(session) = sessions.get_mut(&session_id) {
                    session.state = SessionState::Idle;
                    // Drop any pending interaction
                    session.pending_interaction.take();
                    let _ = handle
                        .output
                        .send(CoreOutput::SessionStateChanged {
                            session_id,
                            state: SessionState::Idle,
                        })
                        .await;
                }
            }

            CoreInput::Shutdown => break,

            // IPC / process-control variants — handled by the harness layer.
            CoreInput::SpawnSession { reply, .. } => {
                let _ = reply.send(Err("not implemented in core loop".into()));
            }
            CoreInput::Signal { .. } | CoreInput::SendMessage { .. } => {}
            CoreInput::WaitSession { reply, .. } => {
                let _ = reply.send(None);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::{create_channels, ChannelConfig};
    use std::pin::Pin;
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
}
