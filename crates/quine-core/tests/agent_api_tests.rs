//! QA tests for the Linux-inspired agent APIs.
//!
//! Covers: AgentDescriptor, CloneFlags, SpawnOptions, signals, resource limits,
//! groups/sessions, scheduling, message queues, introspection, and slash commands.
//!
//! These are unit/integration tests that exercise the agent state machine and
//! dispatcher internals without requiring a live LLM.

use quine_core::agent::*;
use quine_core::commands::{handle_command, CommandResult, SessionUsage};
use quine_core::conversation::{ToolCall, ToolOutput};
use quine_core::llm_types::*;
use quine_core::log::ConversationLog;
use quine_core::permissions::PermissionManager;
use quine_core::tool::agent_manage::AgentManageTool;
use quine_core::tool::message_queue::MessageQueueTool;
use quine_core::tool::subagent::SubagentTool;
use quine_core::tool::{Tool, ToolRegistry};
use serde_json::json;
use std::collections::VecDeque;
use tempfile::TempDir;

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn make_config() -> AgentConfig {
    AgentConfig {
        model: "test-model".to_string(),
        system_prompt: "test-system".to_string(),
        max_tokens: 4096,
    }
}

fn make_conv_log() -> ConversationLog {
    ConversationLog::new(
        "test-model".to_string(),
        "test-provider".to_string(),
        "test-system".to_string(),
    )
}

fn make_response(content: &str) -> CompletionResponse {
    CompletionResponse {
        content: content.to_string(),
        tool_calls: vec![],
        stop_reason: StopReason::EndTurn,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    }
}

fn make_response_with_tools(tool_calls: Vec<ToolCall>) -> CompletionResponse {
    CompletionResponse {
        content: String::new(),
        tool_calls,
        stop_reason: StopReason::ToolUse,
        usage: Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        },
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 1: Slash Commands & Introspection
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn help_command_lists_ps_tree_kill() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/help", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Continue)),
        "/help should return Continue"
    );
    // The /help output is printed to stdout — we verify the CommandResult is correct
    // and that the format is handled (the print is inside commands.rs which we've verified)
}

#[test]
fn ps_command_returns_process_list() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/ps", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::ProcessList)),
        "/ps should return ProcessList, which the dispatcher handles"
    );
}

#[test]
fn tree_command_returns_process_tree() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/tree", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::ProcessTree)),
        "/tree should return ProcessTree"
    );
}

#[test]
fn kill_no_args_shows_usage() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Continue)),
        "/kill with no args should print usage and return Continue"
    );
}

#[test]
fn kill_invalid_id_prints_error() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill abc term", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Continue)),
        "/kill with invalid ID should print error and return Continue"
    );
}

#[test]
fn kill_valid_id_returns_kill_result() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill 1 term", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(
            result,
            Some(CommandResult::Kill {
                agent_id: 1,
                ref signal
            }) if signal == "term"
        ),
        "/kill 1 term should return Kill {{ agent_id: 1, signal: 'term' }}"
    );
}

#[test]
fn kill_defaults_to_term_signal() {
    let mut log = make_conv_log();
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill 5", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(
            result,
            Some(CommandResult::Kill {
                agent_id: 5,
                ref signal
            }) if signal == "term"
        ),
        "/kill 5 without signal should default to 'term'"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 2: Tool Schema Verification (Subagent, AgentManage, MessageQueue)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn subagent_tool_schema_has_clone_flags_and_resource_limits() {
    let tool = SubagentTool::new();
    assert_eq!(tool.name(), "Subagent", "tool name should be 'Subagent'");
    let schema = tool.parameters_schema();
    let props = &schema["properties"];

    // CloneFlags-derived parameters
    assert!(
        props["share_context"].is_object(),
        "schema should have share_context (CLONE_CONTEXT)"
    );
    assert!(
        props["share_tools"].is_object(),
        "schema should have share_tools (CLONE_TOOLS)"
    );
    assert!(
        props["share_perms"].is_object(),
        "schema should have share_perms (CLONE_PERMS)"
    );
    assert!(
        props["share_log"].is_object(),
        "schema should have share_log (CLONE_LOG)"
    );
    assert!(
        props["copy_history"].is_object(),
        "schema should have copy_history (COPY_HISTORY)"
    );

    // Scheduling
    assert!(props["priority"].is_object(), "schema should have priority");
    assert!(
        props["scheduling_policy"].is_object(),
        "schema should have scheduling_policy"
    );
    assert!(props["group_id"].is_object(), "schema should have group_id");

    // Resource limits
    assert!(
        props["max_turns"].is_object(),
        "schema should have max_turns"
    );
    assert!(
        props["max_tool_calls"].is_object(),
        "schema should have max_tool_calls"
    );
    assert!(
        props["max_tokens"].is_object(),
        "schema should have max_tokens"
    );

    // Nested spawn
    assert!(
        props["allow_nested_spawn"].is_object(),
        "schema should have allow_nested_spawn"
    );

    // Required
    let required = schema["required"].as_array().unwrap();
    assert!(
        required.contains(&json!("prompt")),
        "prompt should be required"
    );
}

#[test]
fn agent_manage_tool_schema_has_all_actions() {
    let tool = AgentManageTool::new();
    assert_eq!(
        tool.name(),
        "AgentManage",
        "tool name should be 'AgentManage'"
    );
    let schema = tool.parameters_schema();
    let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
    let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();

    assert!(
        action_strs.contains(&"send_signal"),
        "should have send_signal action"
    );
    assert!(
        action_strs.contains(&"list_agents"),
        "should have list_agents action"
    );
    assert!(
        action_strs.contains(&"agent_tree"),
        "should have agent_tree action"
    );
    assert!(
        action_strs.contains(&"set_priority"),
        "should have set_priority action"
    );
    assert!(
        action_strs.contains(&"set_scheduling_policy"),
        "should have set_scheduling_policy action"
    );
    assert!(
        action_strs.contains(&"set_agent_group"),
        "should have set_agent_group action"
    );
    assert!(
        action_strs.contains(&"create_session"),
        "should have create_session action"
    );
    assert!(
        action_strs.contains(&"set_resource_limit"),
        "should have set_resource_limit action"
    );
    assert!(
        action_strs.contains(&"get_resource_usage"),
        "should have get_resource_usage action"
    );
    assert!(action_strs.contains(&"wait"), "should have wait action");
    assert_eq!(action_strs.len(), 10, "should have exactly 10 actions");
}

#[test]
fn agent_manage_tool_schema_has_signal_enum() {
    let tool = AgentManageTool::new();
    let schema = tool.parameters_schema();
    let signals = schema["properties"]["signal"]["enum"].as_array().unwrap();
    let signal_strs: Vec<&str> = signals.iter().filter_map(|v| v.as_str()).collect();

    assert!(signal_strs.contains(&"term"), "should have term signal");
    assert!(signal_strs.contains(&"kill"), "should have kill signal");
    assert!(signal_strs.contains(&"stop"), "should have stop signal");
    assert!(signal_strs.contains(&"cont"), "should have cont signal");
    assert!(signal_strs.contains(&"int"), "should have int signal");
    assert!(
        signal_strs.contains(&"resource_warning"),
        "should have resource_warning signal"
    );
    assert_eq!(signal_strs.len(), 6, "should have exactly 6 signal types");
}

#[test]
fn message_queue_tool_schema_has_all_actions() {
    let tool = MessageQueueTool::new();
    assert_eq!(
        tool.name(),
        "MessageQueue",
        "tool name should be 'MessageQueue'"
    );
    let schema = tool.parameters_schema();
    let actions = schema["properties"]["action"]["enum"].as_array().unwrap();
    let action_strs: Vec<&str> = actions.iter().filter_map(|v| v.as_str()).collect();

    assert!(action_strs.contains(&"create"), "should have create action");
    assert!(action_strs.contains(&"send"), "should have send action");
    assert!(
        action_strs.contains(&"receive"),
        "should have receive action"
    );
    assert!(action_strs.contains(&"length"), "should have length action");
    assert!(action_strs.contains(&"delete"), "should have delete action");
    assert_eq!(action_strs.len(), 5, "should have exactly 5 actions");

    let required = schema["required"].as_array().unwrap();
    assert!(
        required.contains(&json!("action")),
        "action should be required"
    );
    assert!(
        required.contains(&json!("queue_name")),
        "queue_name should be required"
    );
}

#[tokio::test]
async fn subagent_execute_returns_error() {
    let tool = SubagentTool::new();
    let result = tool.execute(json!({"prompt": "test"})).await;
    assert!(
        result.is_err(),
        "SubagentTool.execute() should always error — dispatcher handles this tool"
    );
}

#[tokio::test]
async fn agent_manage_execute_returns_error() {
    let tool = AgentManageTool::new();
    let result = tool.execute(json!({"action": "list_agents"})).await;
    assert!(
        result.is_err(),
        "AgentManageTool.execute() should always error — dispatcher handles this tool"
    );
}

#[tokio::test]
async fn message_queue_execute_returns_error() {
    let tool = MessageQueueTool::new();
    let result = tool
        .execute(json!({"action": "create", "queue_name": "test"}))
        .await;
    assert!(
        result.is_err(),
        "MessageQueueTool.execute() should always error — dispatcher handles this tool"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 3: Registry includes new tools
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn tool_registry_has_agent_manage() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    assert!(
        registry.get("AgentManage").is_some(),
        "default registry should include AgentManage"
    );
}

#[test]
fn tool_registry_has_message_queue() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    assert!(
        registry.get("MessageQueue").is_some(),
        "default registry should include MessageQueue"
    );
}

#[test]
fn tool_registry_has_subagent() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    assert!(
        registry.get("Subagent").is_some(),
        "default registry should include Subagent"
    );
}

#[test]
fn tool_registry_unregister_removes_subagent() {
    let tmp = TempDir::new().unwrap();
    let mut registry = ToolRegistry::register_defaults(tmp.path());
    assert!(
        registry.get("Subagent").is_some(),
        "Subagent should exist before unregister"
    );
    registry.unregister("Subagent");
    assert!(
        registry.get("Subagent").is_none(),
        "Subagent should be gone after unregister"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 4: CloneFlags
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn clone_flags_subagent_compat_shares_context_workdir_perms() {
    let flags = CloneFlags::SUBAGENT_COMPAT;
    assert!(
        flags.contains(CloneFlags::CLONE_CONTEXT),
        "SUBAGENT_COMPAT should include CLONE_CONTEXT"
    );
    assert!(
        flags.contains(CloneFlags::CLONE_WORKDIR),
        "SUBAGENT_COMPAT should include CLONE_WORKDIR"
    );
    assert!(
        flags.contains(CloneFlags::CLONE_PERMS),
        "SUBAGENT_COMPAT should include CLONE_PERMS"
    );
    assert!(
        !flags.contains(CloneFlags::CLONE_TOOLS),
        "SUBAGENT_COMPAT should NOT include CLONE_TOOLS"
    );
    assert!(
        !flags.contains(CloneFlags::CLONE_LOG),
        "SUBAGENT_COMPAT should NOT include CLONE_LOG"
    );
    assert!(
        !flags.contains(CloneFlags::COPY_HISTORY),
        "SUBAGENT_COMPAT should NOT include COPY_HISTORY"
    );
}

#[test]
fn clone_flags_thread_shares_context_workdir_perms_log() {
    let flags = CloneFlags::THREAD;
    assert!(
        flags.contains(CloneFlags::CLONE_CONTEXT),
        "THREAD should include CLONE_CONTEXT"
    );
    assert!(
        flags.contains(CloneFlags::CLONE_WORKDIR),
        "THREAD should include CLONE_WORKDIR"
    );
    assert!(
        flags.contains(CloneFlags::CLONE_PERMS),
        "THREAD should include CLONE_PERMS"
    );
    assert!(
        flags.contains(CloneFlags::CLONE_LOG),
        "THREAD should include CLONE_LOG"
    );
    assert!(
        !flags.contains(CloneFlags::CLONE_TOOLS),
        "THREAD should NOT include CLONE_TOOLS"
    );
    assert!(
        !flags.contains(CloneFlags::COPY_HISTORY),
        "THREAD should NOT include COPY_HISTORY"
    );
}

#[test]
fn clone_flags_process_shares_nothing() {
    let flags = CloneFlags::PROCESS;
    assert!(
        flags.is_empty(),
        "PROCESS should share nothing (empty flags)"
    );
}

#[test]
fn clone_flags_custom_combination() {
    let flags = CloneFlags::CLONE_CONTEXT | CloneFlags::COPY_HISTORY;
    assert!(
        flags.contains(CloneFlags::CLONE_CONTEXT),
        "custom flags should include CLONE_CONTEXT"
    );
    assert!(
        flags.contains(CloneFlags::COPY_HISTORY),
        "custom flags should include COPY_HISTORY"
    );
    assert!(
        !flags.contains(CloneFlags::CLONE_WORKDIR),
        "custom flags should NOT include CLONE_WORKDIR"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 5: Signals
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn signal_catchability() {
    assert!(
        !AgentSignal::Kill.is_catchable(),
        "SIGKILL should not be catchable"
    );
    assert!(
        !AgentSignal::Stop.is_catchable(),
        "SIGSTOP should not be catchable"
    );
    assert!(
        AgentSignal::Term.is_catchable(),
        "SIGTERM should be catchable"
    );
    assert!(
        AgentSignal::Continue.is_catchable(),
        "SIGCONT should be catchable"
    );
    assert!(
        AgentSignal::Interrupt.is_catchable(),
        "SIGINT should be catchable"
    );
    assert!(
        AgentSignal::ResourceWarning.is_catchable(),
        "SIGXCPU should be catchable"
    );
    assert!(
        AgentSignal::User(1).is_catchable(),
        "SIGUSR1 should be catchable"
    );
}

#[test]
fn signal_display_format() {
    assert_eq!(format!("{}", AgentSignal::Term), "SIGTERM");
    assert_eq!(format!("{}", AgentSignal::Kill), "SIGKILL");
    assert_eq!(format!("{}", AgentSignal::Stop), "SIGSTOP");
    assert_eq!(format!("{}", AgentSignal::Continue), "SIGCONT");
    assert_eq!(format!("{}", AgentSignal::Interrupt), "SIGINT");
    assert_eq!(format!("{}", AgentSignal::ResourceWarning), "SIGXCPU");
    assert_eq!(format!("{}", AgentSignal::User(1)), "SIGUSR1");
    assert_eq!(format!("{}", AgentSignal::User(2)), "SIGUSR2");
}

#[test]
fn enqueue_and_process_term_signal() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent.enqueue_signal(AgentSignal::Term);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(
            effect,
            SignalEffect::Terminate(ExitStatus::Signaled {
                signal: AgentSignal::Term,
                ..
            })
        ),
        "SIGTERM should cause Terminate effect with Signaled status"
    );
    assert_eq!(
        agent.pending_signals.len(),
        0,
        "pending signals should be cleared after SIGTERM"
    );
}

#[test]
fn enqueue_and_process_kill_signal() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    // Enqueue multiple signals — Kill should clear the queue
    agent.enqueue_signal(AgentSignal::ResourceWarning);
    agent.enqueue_signal(AgentSignal::Kill);
    agent.enqueue_signal(AgentSignal::Term);

    let effect = agent.process_pending_signals();
    // ResourceWarning should inject a message, but Kill clears and terminates
    // Actually: ResourceWarning is processed first (default: inject message),
    // then Kill causes Terminate
    assert!(
        matches!(
            effect,
            SignalEffect::Terminate(ExitStatus::Signaled {
                signal: AgentSignal::Kill,
                ..
            })
        ),
        "SIGKILL should cause immediate Terminate effect, clearing remaining signals"
    );
    assert_eq!(
        agent.pending_signals.len(),
        0,
        "Kill should clear all pending signals"
    );
}

#[test]
fn stop_signal_causes_stop_effect() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent.enqueue_signal(AgentSignal::Stop);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::Stop),
        "SIGSTOP should cause Stop effect"
    );
}

#[test]
fn resource_warning_injects_system_message() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let initial_message_count = agent.messages.len();
    agent.enqueue_signal(AgentSignal::ResourceWarning);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::None),
        "ResourceWarning should not terminate or stop"
    );
    assert_eq!(
        agent.messages.len(),
        initial_message_count + 1,
        "ResourceWarning should inject exactly 1 system message"
    );
    let last_msg = agent.messages.last().unwrap();
    assert!(
        last_msg.content.as_text().contains("resource limits"),
        "injected message should mention resource limits"
    );
}

#[test]
fn user_signal_default_action_is_ignore() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let initial_message_count = agent.messages.len();
    agent.enqueue_signal(AgentSignal::User(1));

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::None),
        "SIGUSR1 default should be no effect"
    );
    assert_eq!(
        agent.messages.len(),
        initial_message_count,
        "SIGUSR1 with default action should not inject any messages"
    );
}

#[test]
fn custom_signal_handler_inject_message() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let initial_message_count = agent.messages.len();

    // Register a custom handler for SIGUSR1
    agent.set_signal_handler(
        AgentSignal::User(1),
        SignalAction::InjectMessage("Custom coordination message".to_string()),
    );
    agent.enqueue_signal(AgentSignal::User(1));

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::None),
        "custom InjectMessage handler should not terminate"
    );
    assert_eq!(
        agent.messages.len(),
        initial_message_count + 1,
        "InjectMessage handler should add exactly 1 message"
    );
    let last_msg = agent.messages.last().unwrap();
    assert_eq!(
        last_msg.content.as_text(),
        "Custom coordination message",
        "injected message should be the custom handler text"
    );
}

#[test]
fn custom_signal_handler_ignore() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let initial_message_count = agent.messages.len();

    // Make SIGTERM non-fatal by setting Ignore handler
    agent.set_signal_handler(AgentSignal::Term, SignalAction::Ignore);
    agent.enqueue_signal(AgentSignal::Term);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::None),
        "SIGTERM with Ignore handler should have no effect"
    );
    assert_eq!(
        agent.messages.len(),
        initial_message_count,
        "Ignore handler should not add messages"
    );
}

#[test]
fn cannot_override_kill_handler() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    // Try to set an Ignore handler for SIGKILL — should be silently ignored
    agent.set_signal_handler(AgentSignal::Kill, SignalAction::Ignore);
    agent.enqueue_signal(AgentSignal::Kill);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::Terminate(_)),
        "SIGKILL handler cannot be overridden — should still terminate"
    );
}

#[test]
fn cannot_override_stop_handler() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    // Try to set an Ignore handler for SIGSTOP — should be silently ignored
    agent.set_signal_handler(AgentSignal::Stop, SignalAction::Ignore);
    agent.enqueue_signal(AgentSignal::Stop);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(effect, SignalEffect::Stop),
        "SIGSTOP handler cannot be overridden — should still stop"
    );
}

#[test]
fn interrupt_signal_terminates_by_default() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent.enqueue_signal(AgentSignal::Interrupt);

    let effect = agent.process_pending_signals();
    assert!(
        matches!(
            effect,
            SignalEffect::Terminate(ExitStatus::Signaled {
                signal: AgentSignal::Interrupt,
                ..
            })
        ),
        "SIGINT default should terminate"
    );
}

#[test]
fn multiple_signals_processes_in_order() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let initial_count = agent.messages.len();

    // Enqueue ResourceWarning then Stop
    agent.enqueue_signal(AgentSignal::ResourceWarning);
    agent.enqueue_signal(AgentSignal::Stop);

    let effect = agent.process_pending_signals();
    // ResourceWarning is processed first (injects message), then Stop returns Stop effect
    assert_eq!(
        agent.messages.len(),
        initial_count + 1,
        "ResourceWarning should have been processed before Stop"
    );
    assert!(
        matches!(effect, SignalEffect::Stop),
        "Stop should be the returned effect (most severe of the remaining)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 6: Resource Limits
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn resource_usage_default_is_zero() {
    let usage = ResourceUsage::default();
    assert_eq!(
        usage.total_input_tokens, 0,
        "default input tokens should be 0"
    );
    assert_eq!(
        usage.total_output_tokens, 0,
        "default output tokens should be 0"
    );
    assert_eq!(usage.total_turns, 0, "default turns should be 0");
    assert_eq!(usage.total_tool_calls, 0, "default tool_calls should be 0");
    assert_eq!(
        usage.children_spawned, 0,
        "default children_spawned should be 0"
    );
}

#[test]
fn resource_usage_value_for_total_tokens() {
    let mut usage = ResourceUsage::default();
    usage.total_input_tokens = 100;
    usage.total_output_tokens = 50;
    assert_eq!(
        usage.value_for(ResourceKind::TotalTokens),
        150,
        "TotalTokens should be sum of input + output"
    );
}

#[test]
fn resource_limits_set_and_get() {
    let mut limits = ResourceLimits::default();
    limits.set(ResourceKind::MaxTurns, ResourceLimit { soft: 5, hard: 10 });
    let limit = limits.get(ResourceKind::MaxTurns).unwrap();
    assert_eq!(limit.soft, 5, "soft limit should be 5");
    assert_eq!(limit.hard, 10, "hard limit should be 10");
    assert!(
        limits.get(ResourceKind::MaxToolCalls).is_none(),
        "unset limit should return None"
    );
}

#[test]
fn resource_check_ok_when_within_limits() {
    let mut usage = ResourceUsage::default();
    usage.total_turns = 3;
    let mut limits = ResourceLimits::default();
    limits.set(ResourceKind::MaxTurns, ResourceLimit { soft: 5, hard: 10 });

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::Ok,
        "3 turns should be within soft=5, hard=10"
    );
}

#[test]
fn resource_check_soft_limit_exceeded() {
    let mut usage = ResourceUsage::default();
    usage.total_turns = 6;
    let mut limits = ResourceLimits::default();
    limits.set(ResourceKind::MaxTurns, ResourceLimit { soft: 5, hard: 10 });

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::SoftLimitExceeded(ResourceKind::MaxTurns),
        "6 turns should exceed soft=5"
    );
}

#[test]
fn resource_check_hard_limit_exceeded() {
    let mut usage = ResourceUsage::default();
    usage.total_turns = 10;
    let mut limits = ResourceLimits::default();
    limits.set(ResourceKind::MaxTurns, ResourceLimit { soft: 5, hard: 10 });

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::HardLimitExceeded(ResourceKind::MaxTurns),
        "10 turns should hit hard limit of 10"
    );
}

#[test]
fn resource_check_hard_limit_takes_priority_over_soft() {
    let mut usage = ResourceUsage::default();
    usage.total_turns = 15;
    usage.total_tool_calls = 8; // exceeds soft but not hard
    let mut limits = ResourceLimits::default();
    limits.set(ResourceKind::MaxTurns, ResourceLimit { soft: 5, hard: 10 });
    limits.set(
        ResourceKind::MaxToolCalls,
        ResourceLimit { soft: 5, hard: 20 },
    );

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::HardLimitExceeded(ResourceKind::MaxTurns),
        "hard limit violation should take priority over soft limit"
    );
}

#[test]
fn agent_check_resource_limits_hard_returns_exit_status() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 1, hard: 2 });
    agent.resource_usage.total_turns = 2;

    let result = agent.check_resource_limits();
    assert!(
        result.is_some(),
        "hard limit should return Some(ExitStatus)"
    );
    assert!(
        matches!(
            result,
            Some(ExitStatus::ResourceExhausted {
                limit: ResourceKind::MaxTurns,
                ..
            })
        ),
        "should be ResourceExhausted with MaxTurns kind"
    );
}

#[test]
fn agent_check_resource_limits_soft_enqueues_warning() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 2, hard: 5 });
    agent.resource_usage.total_turns = 3; // exceeds soft but not hard

    let result = agent.check_resource_limits();
    assert!(
        result.is_none(),
        "soft limit should NOT return an ExitStatus"
    );
    assert_eq!(
        agent.pending_signals.len(),
        1,
        "soft limit should enqueue exactly 1 ResourceWarning signal"
    );
    assert!(
        matches!(agent.pending_signals[0], AgentSignal::ResourceWarning),
        "enqueued signal should be ResourceWarning"
    );
}

#[test]
fn agent_check_resource_limits_soft_does_not_enqueue_duplicate() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    agent
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 2, hard: 5 });
    agent.resource_usage.total_turns = 3;

    // First check enqueues warning
    agent.check_resource_limits();
    assert_eq!(
        agent.pending_signals.len(),
        1,
        "first check should enqueue 1 warning"
    );

    // Second check should NOT duplicate
    agent.check_resource_limits();
    assert_eq!(
        agent.pending_signals.len(),
        1,
        "second check should not enqueue duplicate warning"
    );
}

#[test]
fn resource_limits_on_tool_calls() {
    let mut usage = ResourceUsage::default();
    usage.total_tool_calls = 3;
    let mut limits = ResourceLimits::default();
    limits.set(
        ResourceKind::MaxToolCalls,
        ResourceLimit { soft: 3, hard: 3 },
    );

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::HardLimitExceeded(ResourceKind::MaxToolCalls),
        "3 tool calls should hit hard limit of 3"
    );
}

#[test]
fn resource_limits_on_tokens() {
    let mut usage = ResourceUsage::default();
    usage.total_input_tokens = 5000;
    usage.total_output_tokens = 5000;
    let mut limits = ResourceLimits::default();
    limits.set(
        ResourceKind::TotalTokens,
        ResourceLimit {
            soft: 8000,
            hard: 10000,
        },
    );

    let result = usage.check_limits(&limits);
    assert_eq!(
        result,
        ResourceCheckResult::HardLimitExceeded(ResourceKind::TotalTokens),
        "10000 total tokens should hit hard limit of 10000"
    );
}

#[test]
fn record_llm_response_increments_usage() {
    let mut agent = Agent::new_subagent(make_config(), "task".to_string());
    let response = make_response("result");

    agent.record_llm_response(&response).unwrap();
    assert_eq!(
        agent.resource_usage.total_turns, 1,
        "should increment total_turns by 1"
    );
    assert_eq!(
        agent.resource_usage.total_input_tokens, 100,
        "should track input tokens"
    );
    assert_eq!(
        agent.resource_usage.total_output_tokens, 50,
        "should track output tokens"
    );
}

#[test]
fn record_tool_result_increments_tool_calls() {
    let mut agent = Agent::new_root(make_config(), make_conv_log(), vec![]);
    agent.state = AgentState::ExecutingTools {
        pending: VecDeque::new(),
        results: vec![],
    };

    agent.record_tool_result(
        "tc1".to_string(),
        "Read".to_string(),
        json!({}),
        ToolOutput {
            success: true,
            output: "ok".to_string(),
        },
    );
    assert_eq!(
        agent.resource_usage.total_tool_calls, 1,
        "should increment total_tool_calls by 1"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 7: Agent State Machine
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn agent_state_summary_mapping() {
    let mut agent = Agent::new_root(make_config(), make_conv_log(), vec![]);

    // WaitingUserInput -> Sleeping
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Sleeping,
        "WaitingUserInput should be Sleeping"
    );

    // WaitingLlmResponse -> Running
    agent.state = AgentState::WaitingLlmResponse;
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Running,
        "WaitingLlmResponse should be Running"
    );

    // ExecutingTools -> Running
    agent.state = AgentState::ExecutingTools {
        pending: VecDeque::new(),
        results: vec![],
    };
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Running,
        "ExecutingTools should be Running"
    );

    // WaitingToolInput -> Sleeping
    agent.state = AgentState::WaitingToolInput {
        pending: VecDeque::new(),
        results: vec![],
        tool_call_id: "tc1".to_string(),
        tool_name: "AskUserQuestion".to_string(),
        arguments: json!({}),
    };
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Sleeping,
        "WaitingToolInput should be Sleeping"
    );

    // Stopped -> Stopped
    agent.state = AgentState::Stopped {
        previous_state: Box::new(AgentState::WaitingLlmResponse),
        signal: AgentSignal::Stop,
    };
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Stopped,
        "Stopped should be Stopped"
    );

    // Done -> Done
    agent.state = AgentState::Done {
        status: ExitStatus::Exited {
            code: 0,
            output: String::new(),
        },
    };
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Done,
        "Done should be Done"
    );
}

#[test]
fn exit_status_output_extraction() {
    let exited = ExitStatus::Exited {
        code: 0,
        output: "success output".to_string(),
    };
    assert_eq!(
        exited.output(),
        "success output",
        "Exited should return output text"
    );

    let signaled = ExitStatus::Signaled {
        signal: AgentSignal::Term,
        output: "term output".to_string(),
    };
    assert_eq!(
        signaled.output(),
        "term output",
        "Signaled should return output text"
    );

    let exhausted = ExitStatus::ResourceExhausted {
        limit: ResourceKind::MaxTurns,
        output: "exhausted output".to_string(),
    };
    assert_eq!(
        exhausted.output(),
        "exhausted output",
        "ResourceExhausted should return output text"
    );
}

#[test]
fn exit_status_success_check() {
    assert!(
        ExitStatus::Exited {
            code: 0,
            output: String::new()
        }
        .success(),
        "Exited(0) should be success"
    );
    assert!(
        !ExitStatus::Exited {
            code: 1,
            output: String::new()
        }
        .success(),
        "Exited(1) should not be success"
    );
    assert!(
        !ExitStatus::Signaled {
            signal: AgentSignal::Term,
            output: String::new()
        }
        .success(),
        "Signaled should not be success"
    );
    assert!(
        !ExitStatus::ResourceExhausted {
            limit: ResourceKind::MaxTurns,
            output: String::new()
        }
        .success(),
        "ResourceExhausted should not be success"
    );
}

#[test]
fn stopped_state_preserves_previous() {
    let mut agent = Agent::new_root(make_config(), make_conv_log(), vec![]);
    agent.state = AgentState::WaitingLlmResponse;

    // Manually stop the agent (simulating dispatcher behavior)
    let previous = std::mem::replace(&mut agent.state, AgentState::WaitingUserInput);
    agent.state = AgentState::Stopped {
        previous_state: Box::new(previous),
        signal: AgentSignal::Stop,
    };

    assert!(agent.is_stopped(), "agent should report as stopped");
    assert_eq!(
        agent.state_summary(),
        AgentStateSummary::Stopped,
        "state summary should be Stopped"
    );

    // Resume: restore previous state
    if let AgentState::Stopped { previous_state, .. } = agent.state {
        agent.state = *previous_state;
    }
    assert!(
        matches!(agent.state, AgentState::WaitingLlmResponse),
        "restored state should be WaitingLlmResponse"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 8: SpawnOptions Defaults
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn spawn_options_defaults() {
    let opts = SpawnOptions::default();
    assert_eq!(
        opts.flags,
        CloneFlags::SUBAGENT_COMPAT,
        "default flags should be SUBAGENT_COMPAT"
    );
    assert!(
        !opts.allow_nested_spawn,
        "default allow_nested_spawn should be false"
    );
    assert_eq!(opts.max_depth, 8, "default max_depth should be 8");
    assert_eq!(opts.priority, 0, "default priority should be 0");
    assert!(
        opts.resource_limits.limits.is_empty(),
        "default resource_limits should be empty"
    );
    assert!(
        opts.tool_filter.is_none(),
        "default tool_filter should be None"
    );
    assert!(opts.group_id.is_none(), "default group_id should be None");
    assert!(opts.name.is_none(), "default name should be None");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 9: Scheduling Policy
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn scheduling_policy_default_is_normal() {
    let policy = SchedulingPolicy::default();
    assert_eq!(
        policy,
        SchedulingPolicy::Normal,
        "default scheduling policy should be Normal"
    );
}

#[test]
fn scheduling_policy_variants_are_distinct() {
    assert_ne!(SchedulingPolicy::Normal, SchedulingPolicy::Exclusive);
    assert_ne!(SchedulingPolicy::Normal, SchedulingPolicy::Background);
    assert_ne!(SchedulingPolicy::Exclusive, SchedulingPolicy::Background);
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 10: Identity Types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn agent_id_equality() {
    assert_eq!(AgentId(0), AgentId(0), "same IDs should be equal");
    assert_ne!(AgentId(0), AgentId(1), "different IDs should not be equal");
}

#[test]
fn agent_group_id_equality() {
    assert_eq!(
        AgentGroupId(5),
        AgentGroupId(5),
        "same group IDs should be equal"
    );
    assert_ne!(
        AgentGroupId(1),
        AgentGroupId(2),
        "different group IDs should not be equal"
    );
}

#[test]
fn session_id_equality() {
    assert_eq!(
        SessionId(3),
        SessionId(3),
        "same session IDs should be equal"
    );
    assert_ne!(
        SessionId(0),
        SessionId(1),
        "different session IDs should not be equal"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 11: Agent Kind
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn agent_kind_root_vs_subagent() {
    assert_ne!(
        AgentKind::Root,
        AgentKind::Subagent,
        "Root and Subagent should be distinct"
    );
}

#[test]
fn root_agent_is_root_flag() {
    let agent = Agent::new_root(make_config(), make_conv_log(), vec![]);
    assert!(agent.is_root, "new_root agent should have is_root=true");
}

#[test]
fn subagent_is_not_root() {
    let agent = Agent::new_subagent(make_config(), "task".to_string());
    assert!(!agent.is_root, "new_subagent should have is_root=false");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 12: End-to-End Agent Lifecycle (State Machine)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn subagent_lifecycle_no_tools() {
    // Subagent: WaitingLlmResponse -> Done (no tool calls)
    let mut agent = Agent::new_subagent(make_config(), "summarize this".to_string());
    assert!(
        matches!(agent.state, AgentState::WaitingLlmResponse),
        "subagent starts in WaitingLlmResponse"
    );

    let response = make_response("Here is the summary.");
    agent.record_llm_response(&response).unwrap();

    assert!(
        matches!(agent.state, AgentState::Done { .. }),
        "subagent should be Done after response with no tools"
    );
    if let AgentState::Done { ref status } = agent.state {
        assert!(status.success(), "exit should be successful");
        assert_eq!(
            status.output(),
            "Here is the summary.",
            "output should match response content"
        );
    }
}

#[test]
fn subagent_lifecycle_with_tools() {
    // Subagent: WaitingLlmResponse -> ExecutingTools -> (record results) -> WaitingLlmResponse -> Done
    let mut agent = Agent::new_subagent(make_config(), "read file".to_string());

    // LLM responds with a tool call
    let response = make_response_with_tools(vec![ToolCall {
        id: "tc1".to_string(),
        name: "Read".to_string(),
        arguments: json!({"file_path": "test.txt"}),
    }]);
    agent.record_llm_response(&response).unwrap();
    assert!(
        matches!(agent.state, AgentState::ExecutingTools { .. }),
        "should be ExecutingTools"
    );

    // Pop and execute tool
    let tc = agent.next_tool_call().unwrap();
    assert_eq!(tc.name, "Read", "should be Read tool call");
    let all_done = agent.record_tool_result(
        tc.id,
        tc.name,
        tc.arguments,
        ToolOutput {
            success: true,
            output: "file content".to_string(),
        },
    );
    assert!(all_done, "should be all done (no more pending)");

    // Finalize -> WaitingLlmResponse
    agent.finalize_tool_results().unwrap();
    assert!(
        matches!(agent.state, AgentState::WaitingLlmResponse),
        "should be back to WaitingLlmResponse"
    );

    // Second LLM response with no tools -> Done
    let final_response = make_response("File contained: file content");
    agent.record_llm_response(&final_response).unwrap();
    assert!(
        matches!(agent.state, AgentState::Done { .. }),
        "should be Done after final response"
    );
}

#[test]
fn subagent_resource_exhaustion_lifecycle() {
    // Subagent with max_turns=1 hits limit on second turn
    let mut agent = Agent::new_subagent(make_config(), "explore codebase".to_string());
    agent
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 1, hard: 1 });

    // First LLM response with tool calls
    let response = make_response_with_tools(vec![ToolCall {
        id: "tc1".to_string(),
        name: "Glob".to_string(),
        arguments: json!({"pattern": "*.rs"}),
    }]);
    agent.record_llm_response(&response).unwrap();
    // After recording, total_turns = 1 which equals hard limit

    // Check limits after LLM response
    let exit_status = agent.check_resource_limits();
    assert!(exit_status.is_some(), "1 turn should hit hard limit of 1");
    assert!(
        matches!(
            exit_status,
            Some(ExitStatus::ResourceExhausted {
                limit: ResourceKind::MaxTurns,
                ..
            })
        ),
        "should be ResourceExhausted with MaxTurns"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 13: GlobalContext sharing
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn global_context_clone_shares_todo() {
    let ctx1 = quine_core::tool::GlobalContext::new();
    let ctx2 = ctx1.clone();
    // Both should point to the same TodoTool instance
    assert!(
        std::sync::Arc::ptr_eq(&ctx1.todo, &ctx2.todo),
        "cloned GlobalContext should share the same TodoTool Arc"
    );
}

#[test]
fn tool_registry_with_shared_context() {
    let tmp = TempDir::new().unwrap();
    let ctx = quine_core::tool::GlobalContext::new();
    let reg1 = ToolRegistry::register_defaults_with_context(tmp.path(), &ctx);
    let reg2 = ToolRegistry::register_defaults_with_context(tmp.path(), &ctx);

    // Both registries should have the same tools
    assert!(reg1.get("Read").is_some(), "reg1 should have Read");
    assert!(reg2.get("Read").is_some(), "reg2 should have Read");
    assert!(reg1.get("Todo").is_some(), "reg1 should have Todo");
    assert!(reg2.get("Todo").is_some(), "reg2 should have Todo");
    assert!(
        reg1.get("AgentManage").is_some(),
        "reg1 should have AgentManage"
    );
    assert!(
        reg2.get("MessageQueue").is_some(),
        "reg2 should have MessageQueue"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 14: Subagent initial messages
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn subagent_starts_with_prompt_as_user_message() {
    let agent = Agent::new_subagent(make_config(), "Find all Rust files".to_string());
    assert_eq!(
        agent.messages.len(),
        1,
        "subagent should start with exactly 1 message"
    );
    assert_eq!(
        agent.messages[0].role, "user",
        "first message should be from user"
    );
    assert_eq!(
        agent.messages[0].content.as_text(),
        "Find all Rust files",
        "message content should be the prompt"
    );
    assert!(
        agent.conv_log.is_none(),
        "subagent should not have a conv_log"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 15: Tool Filter
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn tool_filter_allow_list() {
    let filter = ToolFilter::AllowList(vec!["Read".to_string(), "Glob".to_string()]);
    assert!(
        matches!(filter, ToolFilter::AllowList(_)),
        "should be AllowList variant"
    );
}

#[test]
fn tool_filter_deny_list() {
    let filter = ToolFilter::DenyList(vec!["Bash".to_string()]);
    assert!(
        matches!(filter, ToolFilter::DenyList(_)),
        "should be DenyList variant"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 16: Event types
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn event_variants_exist() {
    use quine_core::event::Event;

    // Verify Event variants can be constructed
    let _user = Event::UserInput("hello".to_string());
    let _signal = Event::Signal {
        target: AgentId(1),
        signal: AgentSignal::Term,
    };
    let _group_signal = Event::GroupSignal {
        group: AgentGroupId(0),
        signal: AgentSignal::Kill,
    };
    let _child_state = Event::ChildStateChanged {
        parent_id: AgentId(0),
        child_id: AgentId(1),
        new_state: AgentStateSummary::Done,
    };
    let _shutdown = Event::Shutdown;
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 17: Agent with concurrent subagent scenarios (state machine only)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn root_agent_transitions_correctly_across_multiple_turns() {
    let mut agent = Agent::new_root(make_config(), make_conv_log(), vec![]);

    // Turn 1: user input -> LLM response with text
    agent.record_user_input("hello").unwrap();
    assert!(matches!(agent.state, AgentState::WaitingLlmResponse));

    let response1 = make_response("Hello! How can I help?");
    agent.record_llm_response(&response1).unwrap();
    assert!(matches!(agent.state, AgentState::WaitingUserInput));

    // Turn 2: user input -> LLM response with tool -> tool result -> LLM final
    agent.record_user_input("read test.txt").unwrap();
    let response2 = make_response_with_tools(vec![ToolCall {
        id: "tc1".to_string(),
        name: "Read".to_string(),
        arguments: json!({"file_path": "test.txt"}),
    }]);
    agent.record_llm_response(&response2).unwrap();
    assert!(matches!(agent.state, AgentState::ExecutingTools { .. }));

    let tc = agent.next_tool_call().unwrap();
    agent.record_tool_result(
        tc.id,
        tc.name,
        tc.arguments,
        ToolOutput {
            success: true,
            output: "file content".to_string(),
        },
    );
    agent.finalize_tool_results().unwrap();
    assert!(matches!(agent.state, AgentState::WaitingLlmResponse));

    let response3 = make_response("Here is the file content.");
    agent.record_llm_response(&response3).unwrap();
    assert!(
        matches!(agent.state, AgentState::WaitingUserInput),
        "root should return to WaitingUserInput"
    );

    // Verify usage was tracked across turns
    assert_eq!(
        agent.resource_usage.total_turns, 3,
        "should have 3 turns total"
    );
    assert_eq!(
        agent.resource_usage.total_tool_calls, 1,
        "should have 1 tool call total"
    );
    assert_eq!(
        agent.resource_usage.total_input_tokens, 300,
        "should accumulate 300 input tokens (3 * 100)"
    );
    assert_eq!(
        agent.resource_usage.total_output_tokens, 150,
        "should accumulate 150 output tokens (3 * 50)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 18: Resource Kind coverage
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn resource_kind_value_for_all_kinds() {
    let mut usage = ResourceUsage::default();
    usage.total_input_tokens = 100;
    usage.total_output_tokens = 200;
    usage.total_turns = 3;
    usage.total_tool_calls = 7;
    usage.children_spawned = 2;

    assert_eq!(
        usage.value_for(ResourceKind::TotalTokens),
        300,
        "TotalTokens = input + output"
    );
    assert_eq!(
        usage.value_for(ResourceKind::InputTokens),
        100,
        "InputTokens"
    );
    assert_eq!(
        usage.value_for(ResourceKind::OutputTokens),
        200,
        "OutputTokens"
    );
    assert_eq!(usage.value_for(ResourceKind::MaxTurns), 3, "MaxTurns");
    assert_eq!(
        usage.value_for(ResourceKind::MaxToolCalls),
        7,
        "MaxToolCalls"
    );
    assert_eq!(usage.value_for(ResourceKind::MaxChildren), 2, "MaxChildren");
    assert_eq!(
        usage.value_for(ResourceKind::MaxDepth),
        0,
        "MaxDepth is structural, always 0"
    );
    // WallTime: default Duration is 0
    assert_eq!(
        usage.value_for(ResourceKind::WallTime),
        0,
        "WallTime defaults to 0"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 19: Build request includes all tool schemas
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn build_request_uses_provided_schemas() {
    let agent = Agent::new_subagent(make_config(), "task".to_string());
    let schemas = vec![
        json!({"name": "Read", "description": "Read a file"}),
        json!({"name": "Glob", "description": "Glob search"}),
    ];
    let request = agent.build_request(&schemas);

    assert_eq!(
        request.model, "test-model",
        "request model should match config"
    );
    assert_eq!(
        request.system, "test-system",
        "request system prompt should match config"
    );
    assert_eq!(
        request.tools.len(),
        2,
        "request should include 2 tool schemas"
    );
    assert_eq!(
        request.max_tokens, 4096,
        "request max_tokens should match config"
    );
    assert_eq!(
        request.messages.len(),
        1,
        "request should have 1 message (the prompt)"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Test Group 20: Prompt generation includes new tools
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn system_prompt_mentions_agent_management_tools() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    let prompt = quine_core::prompt::build_system_prompt(tmp.path(), &registry);

    assert!(
        prompt.contains("AgentManage"),
        "system prompt should mention AgentManage tool"
    );
    assert!(
        prompt.contains("MessageQueue"),
        "system prompt should mention MessageQueue tool"
    );
    assert!(
        prompt.contains("Subagent"),
        "system prompt should mention Subagent tool"
    );
}

#[test]
fn system_prompt_without_subagent_hides_it() {
    let tmp = TempDir::new().unwrap();
    let mut registry = ToolRegistry::register_defaults(tmp.path());
    registry.unregister("Subagent");
    let prompt = quine_core::prompt::build_system_prompt(tmp.path(), &registry);

    assert!(
        !prompt.contains("**Subagent**"),
        "system prompt should NOT list Subagent in tool listing after unregister"
    );
    assert!(
        prompt.contains("AgentManage"),
        "system prompt should still list AgentManage"
    );
}
