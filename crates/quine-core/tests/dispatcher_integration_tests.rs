//! Integration tests for agent interaction tools through the Dispatcher.
//!
//! These tests exercise the Dispatcher's public API directly (spawn, signals,
//! message queues, introspection, scheduling) without requiring a live LLM.
//! They cover the test plan groups: Subagent Spawning, Slash Commands,
//! AgentManage, Signals, MessageQueue, and Cross-Feature Combinations.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use tempfile::TempDir;

use quine_core::agent::*;
use quine_core::config::Config;
use quine_core::dispatcher::*;
use quine_core::llm_types::*;
use quine_core::log::ConversationLog;
use quine_core::provider::LlmProvider;

// ─── Mock LLM Provider ──────────────────────────────────────────────────────

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn complete(&self, _request: CompletionRequest) -> anyhow::Result<CompletionResponse> {
        Ok(CompletionResponse {
            content: "mock response".to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 50,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        })
    }

    async fn complete_stream(
        &self,
        _request: CompletionRequest,
    ) -> anyhow::Result<BoxStream<'static, anyhow::Result<StreamEvent>>> {
        anyhow::bail!("streaming not supported in mock")
    }
}

// ─── Mock UI ─────────────────────────────────────────────────────────────────

struct MockUI;

struct MockSpinner;

impl SpinnerHandle for MockSpinner {
    fn stop(&mut self) {}
}

impl DispatcherUI for MockUI {
    fn print_assistant_message(&self, _content: &str) {}
    fn print_delta(&self, _text: &str) {}
    fn print_stream_start(&self) {}
    fn print_tool_call(&self, _name: &str, _args: &serde_json::Value) {}
    fn print_tool_result(&self, _success: bool, _output: &str) {}
    fn start_spinner(&self, _label: &str) -> Box<dyn SpinnerHandle> {
        Box::new(MockSpinner)
    }
    fn print_info(&self, _message: &str) {}
    fn print_error(&self, _message: &str) {}
    fn normal_prompt(&self) -> &str {
        "> "
    }
    fn answer_prompt(&self) -> &str {
        "? "
    }
}

// ─── Test Helpers ────────────────────────────────────────────────────────────

fn make_dispatcher(tmp: &TempDir) -> Dispatcher {
    let provider: Arc<dyn LlmProvider> = Arc::new(MockProvider);
    let mut config = Config::new("mock", "mock-model");
    config.working_dir = tmp.path().to_path_buf();
    config.log_dir = tmp.path().join(".quine").join("logs");

    let conv_log = ConversationLog::new(
        "mock-model".to_string(),
        "mock".to_string(),
        "test system prompt".to_string(),
    );

    Dispatcher::new(
        provider,
        config,
        "mock-model".to_string(),
        "test system prompt".to_string(),
        conv_log,
        vec![],
        false,
        Box::new(MockUI),
    )
}

fn default_spawn_options(prompt: &str) -> SpawnOptions {
    SpawnOptions {
        prompt: prompt.to_string(),
        ..SpawnOptions::default()
    }
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 1: Subagent Spawning & Lifecycle
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn sub_01_basic_subagent_spawn() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(
            root_id,
            "tc-1".to_string(),
            default_spawn_options("List files"),
        )
        .unwrap();

    assert_ne!(
        child_id, root_id,
        "child should have different ID than root"
    );
    assert_eq!(
        disp.children_of(root_id).len(),
        1,
        "root should have exactly 1 child after spawn"
    );
    assert_eq!(
        disp.children_of(root_id)[0],
        child_id,
        "root's child should be the spawned agent"
    );
}

#[test]
fn sub_02_named_subagent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let options = SpawnOptions {
        prompt: "List files".to_string(),
        name: Some("file-finder".to_string()),
        ..SpawnOptions::default()
    };
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    let desc = disp.descriptor(child_id).unwrap();
    assert_eq!(
        desc.name.as_deref(),
        Some("file-finder"),
        "child agent should have name 'file-finder'"
    );
}

#[test]
fn sub_03_subagent_with_max_turns() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("Say hello");
    options
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 2, hard: 2 });
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    let usage = disp.get_resource_usage(child_id).unwrap();
    assert_eq!(usage.total_turns, 0, "new agent should have 0 turns");
}

#[test]
fn sub_04_subagent_with_max_tool_calls() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("Read files");
    options.resource_limits.set(
        ResourceKind::MaxToolCalls,
        ResourceLimit { soft: 1, hard: 1 },
    );
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // Resource limit should be set on the child
    let limit_result = disp.set_resource_limit(
        child_id,
        ResourceKind::MaxToolCalls,
        ResourceLimit { soft: 1, hard: 1 },
    );
    assert!(
        limit_result.is_ok(),
        "setting resource limit should succeed"
    );
}

#[test]
fn sub_05_subagent_with_priority() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("Say hello");
    options.priority = -5;
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // Verify priority is reflected in agent info
    // Note: priority is set via the scheduler at spawn_llm_call time.
    // The SpawnOptions.priority is applied in dispatch_subagent; for direct
    // spawn() we need to set_priority manually.
    disp.set_priority(child_id, -5).unwrap();
    let agents = disp.list_agents();
    let child_info = agents.iter().find(|a| a.id == child_id).unwrap();
    assert_eq!(child_info.priority, -5, "child priority should be -5");
}

#[test]
fn sub_06_multiple_subagents() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_1 = disp
        .spawn(
            root_id,
            "tc-1".to_string(),
            default_spawn_options("Read Cargo.toml"),
        )
        .unwrap();
    let child_2 = disp
        .spawn(
            root_id,
            "tc-2".to_string(),
            default_spawn_options("List files"),
        )
        .unwrap();

    let children = disp.children_of(root_id);
    assert_eq!(children.len(), 2, "root should have exactly 2 children");
    assert!(
        children.contains(&child_1),
        "children should include child_1"
    );
    assert!(
        children.contains(&child_2),
        "children should include child_2"
    );
}

#[test]
fn sub_07_subagent_share_context_true() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("Use Todo");
    options.flags = CloneFlags::CLONE_CONTEXT | CloneFlags::CLONE_WORKDIR | CloneFlags::CLONE_PERMS;
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // Child should exist and be in the agents list
    let agents = disp.list_agents();
    assert!(
        agents.iter().any(|a| a.id == child_id),
        "child with shared context should appear in agent list"
    );
}

#[test]
fn sub_08_subagent_share_context_false() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("Say hello");
    options.flags = CloneFlags::CLONE_WORKDIR | CloneFlags::CLONE_PERMS; // no CLONE_CONTEXT
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    let agents = disp.list_agents();
    assert!(
        agents.iter().any(|a| a.id == child_id),
        "child without shared context should still appear in agent list"
    );
}

#[test]
fn sub_09_subagent_copy_history() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("What did I just ask?");
    options.flags = CloneFlags::CLONE_WORKDIR | CloneFlags::CLONE_PERMS | CloneFlags::COPY_HISTORY;
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // Child should exist and have inherited history
    assert!(
        disp.descriptor(child_id).is_some(),
        "child with COPY_HISTORY should exist"
    );
}

#[test]
fn sub_10_subagent_scheduling_policy_background() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(
            root_id,
            "tc-1".to_string(),
            default_spawn_options("Say hello"),
        )
        .unwrap();

    disp.set_scheduling_policy(child_id, SchedulingPolicy::Background)
        .unwrap();

    let agents = disp.list_agents();
    let child_info = agents.iter().find(|a| a.id == child_id).unwrap();
    assert_eq!(
        child_info.policy,
        SchedulingPolicy::Background,
        "child scheduling policy should be Background"
    );
}

#[test]
fn sub_11_nested_spawn_blocked() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Spawn child with allow_nested_spawn=false and max_depth=1 (so child at depth 1 cannot spawn)
    let mut options = default_spawn_options("hello");
    options.allow_nested_spawn = false;
    options.max_depth = 1; // root is at depth 0, child at depth 1 — so grandchild would exceed
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // Attempting to spawn from child should fail due to depth limit
    let mut nested_opts = default_spawn_options("nested");
    nested_opts.max_depth = 1; // depth limit 1, child is at depth 1
    let result = disp.spawn(child_id, "tc-2".to_string(), nested_opts);
    assert!(
        result.is_err(),
        "nested spawn should fail when depth limit is 1 and parent is at depth 1"
    );
}

#[test]
fn sub_12_nested_spawn_allowed() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("hello");
    options.allow_nested_spawn = true;
    options.max_depth = 3;
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    let mut nested_opts = default_spawn_options("nested hello");
    nested_opts.max_depth = 3;
    let grandchild_id = disp
        .spawn(child_id, "tc-2".to_string(), nested_opts)
        .unwrap();

    assert_eq!(
        disp.parent_of(grandchild_id),
        Some(child_id),
        "grandchild's parent should be child"
    );
    assert_eq!(
        disp.children_of(child_id),
        vec![grandchild_id],
        "child should have grandchild as its only child"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 2: Slash Commands for Agent Introspection
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn ps_01_root_only() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let agents = disp.list_agents();
    assert_eq!(agents.len(), 1, "/ps with root only should show 1 agent");
    assert_eq!(agents[0].id, AgentId(0), "root agent should have ID 0");
}

#[test]
fn ps_02_after_spawning_subagent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    disp.spawn(
        root_id,
        "tc-1".to_string(),
        default_spawn_options("read Cargo.toml"),
    )
    .unwrap();

    let agents = disp.list_agents();
    assert_eq!(
        agents.len(),
        2,
        "/ps after spawning should show root + child"
    );
}

#[test]
fn ps_03_tree_root_only() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let tree = disp.agent_tree().unwrap();
    assert_eq!(tree.info.id, AgentId(0), "tree root should be agent 0");
    assert!(
        tree.children.is_empty(),
        "root-only tree should have no children"
    );
}

#[test]
fn ps_04_tree_after_subagents() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    disp.spawn(root_id, "tc-1".to_string(), default_spawn_options("task1"))
        .unwrap();
    disp.spawn(root_id, "tc-2".to_string(), default_spawn_options("task2"))
        .unwrap();

    let tree = disp.agent_tree().unwrap();
    assert_eq!(
        tree.children.len(),
        2,
        "tree should show 2 children after spawning 2 subagents"
    );
}

#[test]
fn ps_05_kill_no_args() {
    // This is tested at the command parsing level
    use quine_core::commands::{handle_command, CommandResult, SessionUsage};
    use quine_core::permissions::PermissionManager;

    let mut log = ConversationLog::new("m".into(), "p".into(), "s".into());
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Continue)),
        "/kill with no args should return Continue (prints usage)"
    );
}

#[test]
fn ps_06_kill_invalid_id() {
    use quine_core::commands::{handle_command, CommandResult, SessionUsage};
    use quine_core::permissions::PermissionManager;

    let mut log = ConversationLog::new("m".into(), "p".into(), "s".into());
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill abc", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Continue)),
        "/kill with invalid ID should return Continue (prints error)"
    );
}

#[test]
fn ps_07_kill_nonexistent_id() {
    use quine_core::commands::{handle_command, CommandResult, SessionUsage};
    use quine_core::permissions::PermissionManager;

    let mut log = ConversationLog::new("m".into(), "p".into(), "s".into());
    let mut messages = vec![];
    let usage = SessionUsage::default();
    let perms = PermissionManager::new();

    let result = handle_command("/kill 999", &mut log, &mut messages, &usage, &perms);
    assert!(
        matches!(result, Some(CommandResult::Kill { agent_id: 999, .. })),
        "/kill 999 should parse successfully and return Kill result for dispatcher to handle"
    );
}

#[tokio::test]
async fn ps_08_kill_root_with_term() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Sending Term to root should enqueue the signal (processed at safe point)
    let result = disp.send_signal(root_id, AgentSignal::Term).await;
    assert!(result.is_ok(), "sending Term to root should not error");
}

#[tokio::test]
async fn ps_09_kill_root_with_stop() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Stop).await;
    assert!(result.is_ok(), "sending Stop to root should not error");
}

#[tokio::test]
async fn ps_10_kill_root_with_kill() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Kill).await;
    assert!(result.is_ok(), "sending Kill to root should not error");
    // Root should be reset to WaitingUserInput, not removed
    let agents = disp.list_agents();
    assert_eq!(agents.len(), 1, "root should still exist after SIGKILL");
}

#[tokio::test]
async fn ps_11_kill_root_with_cont_when_not_stopped() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Continue).await;
    assert!(
        result.is_ok(),
        "sending Continue to non-stopped root should be a no-op"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 3: AgentManage Tool — All Actions (via Dispatcher public API)
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn am_01_list_agents() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let agents = disp.list_agents();
    assert_eq!(agents.len(), 1, "list_agents should return 1 agent (root)");
    assert_eq!(agents[0].id, AgentId(0), "root agent ID should be 0");
    assert_eq!(
        agents[0].name.as_deref(),
        Some("root"),
        "root agent name should be 'root'"
    );
}

#[test]
fn am_02_agent_tree() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let tree = disp.agent_tree().unwrap();
    assert_eq!(
        tree.info.id,
        AgentId(0),
        "agent_tree root should be agent 0"
    );
    assert!(
        tree.children.is_empty(),
        "agent_tree should have no children for root-only"
    );
}

#[test]
fn am_03_list_agents_after_subagent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    disp.spawn(root_id, "tc-1".to_string(), default_spawn_options("hello"))
        .unwrap();

    let agents = disp.list_agents();
    assert_eq!(
        agents.len(),
        2,
        "list_agents should return 2 after spawning a subagent"
    );
}

#[test]
fn am_04_set_priority_on_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_priority(root_id, -5);
    assert!(result.is_ok(), "set_priority on root should succeed");

    let agents = disp.list_agents();
    let root_info = agents.iter().find(|a| a.id == root_id).unwrap();
    assert_eq!(root_info.priority, -5, "root priority should be -5");
}

#[test]
fn am_05_set_priority_invalid_agent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.set_priority(AgentId(999), 5);
    assert!(
        result.is_err(),
        "set_priority on nonexistent agent should fail"
    );
}

#[test]
fn am_07_set_scheduling_policy_exclusive() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_scheduling_policy(root_id, SchedulingPolicy::Exclusive);
    assert!(result.is_ok(), "set_scheduling_policy should succeed");

    let agents = disp.list_agents();
    let root_info = agents.iter().find(|a| a.id == root_id).unwrap();
    assert_eq!(
        root_info.policy,
        SchedulingPolicy::Exclusive,
        "root scheduling policy should be Exclusive"
    );
}

#[test]
fn am_08_set_scheduling_policy_background() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_scheduling_policy(root_id, SchedulingPolicy::Background);
    assert!(
        result.is_ok(),
        "set_scheduling_policy Background should succeed"
    );

    let agents = disp.list_agents();
    let root_info = agents.iter().find(|a| a.id == root_id).unwrap();
    assert_eq!(
        root_info.policy,
        SchedulingPolicy::Background,
        "root scheduling policy should be Background"
    );
}

#[test]
fn am_09_set_agent_group() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_agent_group(root_id, AgentGroupId(42));
    assert!(result.is_ok(), "set_agent_group should succeed");

    let desc = disp.descriptor(root_id).unwrap();
    assert_eq!(
        desc.group_id,
        AgentGroupId(42),
        "root should now be in group 42"
    );
}

#[test]
fn am_10_set_agent_group_invalid_agent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.set_agent_group(AgentId(999), AgentGroupId(42));
    assert!(
        result.is_err(),
        "set_agent_group on nonexistent agent should fail"
    );
}

#[test]
fn am_11_create_session() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let old_session = disp.descriptor(root_id).unwrap().session_id;
    let new_session = disp.create_session(root_id).unwrap();

    assert_ne!(
        new_session, old_session,
        "new session should have a different ID"
    );
    assert_eq!(
        disp.descriptor(root_id).unwrap().session_id,
        new_session,
        "root should now be in the new session"
    );
}

#[test]
fn am_12_create_session_invalid_agent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.create_session(AgentId(999));
    assert!(
        result.is_err(),
        "create_session on nonexistent agent should fail"
    );
}

#[test]
fn am_13_set_resource_limit_max_turns() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_resource_limit(
        root_id,
        ResourceKind::MaxTurns,
        ResourceLimit { soft: 10, hard: 20 },
    );
    assert!(
        result.is_ok(),
        "set_resource_limit for max_turns should succeed"
    );
}

#[test]
fn am_14_set_resource_limit_max_tool_calls() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.set_resource_limit(
        root_id,
        ResourceKind::MaxToolCalls,
        ResourceLimit { soft: 5, hard: 10 },
    );
    assert!(
        result.is_ok(),
        "set_resource_limit for max_tool_calls should succeed"
    );
}

#[test]
fn am_15_set_resource_limit_invalid_agent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.set_resource_limit(
        AgentId(999),
        ResourceKind::MaxTurns,
        ResourceLimit { soft: 5, hard: 10 },
    );
    assert!(
        result.is_err(),
        "set_resource_limit on nonexistent agent should fail"
    );
}

#[test]
fn am_16_get_resource_usage_root() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let usage = disp.get_resource_usage(root_id).unwrap();
    assert_eq!(
        usage.total_input_tokens, 0,
        "root should start with 0 input tokens"
    );
    assert_eq!(
        usage.total_output_tokens, 0,
        "root should start with 0 output tokens"
    );
    assert_eq!(usage.total_turns, 0, "root should start with 0 turns");
    assert_eq!(
        usage.total_tool_calls, 0,
        "root should start with 0 tool calls"
    );
    assert_eq!(
        usage.children_spawned, 0,
        "root should start with 0 children spawned"
    );
}

#[test]
fn am_17_get_resource_usage_invalid() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let result = disp.get_resource_usage(AgentId(999));
    assert!(
        result.is_err(),
        "get_resource_usage on nonexistent agent should fail"
    );
}

#[test]
fn am_18_children_of_no_children() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let children = disp.children_of(root_id);
    assert!(
        children.is_empty(),
        "root with no children should return empty list (analogous to wait any_child)"
    );
}

#[test]
fn am_19_children_of_with_children() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child = disp
        .spawn(root_id, "tc-1".to_string(), default_spawn_options("hello"))
        .unwrap();

    let children = disp.children_of(root_id);
    assert_eq!(children.len(), 1, "root should have 1 child");
    assert_eq!(
        children[0], child,
        "the child should match the spawned agent"
    );
}

#[test]
fn am_20_scheduling_policy_invalid_agent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.set_scheduling_policy(AgentId(999), SchedulingPolicy::Normal);
    assert!(
        result.is_err(),
        "set_scheduling_policy on nonexistent agent should fail"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 4: Signal Sending
// ═══════════════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn sig_01_send_signal_term_to_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Term).await;
    assert!(result.is_ok(), "sending Term to root should succeed");
}

#[tokio::test]
async fn sig_02_send_signal_kill_to_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Kill).await;
    assert!(result.is_ok(), "sending Kill to root should succeed");
    // Root survives SIGKILL (reset to WaitingUserInput)
    let agents = disp.list_agents();
    assert_eq!(agents.len(), 1, "root should survive SIGKILL");
}

#[tokio::test]
async fn sig_03_send_signal_stop_to_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Stop).await;
    assert!(result.is_ok(), "sending Stop to root should succeed");
}

#[tokio::test]
async fn sig_04_send_signal_cont_to_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Continue).await;
    assert!(
        result.is_ok(),
        "sending Continue to root (not stopped) should be a no-op"
    );
}

#[tokio::test]
async fn sig_05_send_signal_int_to_root() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp.send_signal(root_id, AgentSignal::Interrupt).await;
    assert!(result.is_ok(), "sending Interrupt to root should succeed");
}

#[tokio::test]
async fn sig_06_send_signal_resource_warning() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let result = disp
        .send_signal(root_id, AgentSignal::ResourceWarning)
        .await;
    assert!(result.is_ok(), "sending ResourceWarning should succeed");
}

#[tokio::test]
async fn sig_08_send_signal_to_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    // send_signal on nonexistent agent should be a no-op (no agent to enqueue on)
    let result = disp.send_signal(AgentId(999), AgentSignal::Term).await;
    assert!(
        result.is_ok(),
        "sending signal to nonexistent agent should not error (just no-op)"
    );
}

#[tokio::test]
async fn sig_10_send_signal_to_group() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Spawn two agents in the same group
    let mut opts1 = default_spawn_options("task1");
    opts1.group_id = Some(AgentGroupId(42));
    let _child1 = disp.spawn(root_id, "tc-1".to_string(), opts1).unwrap();

    let mut opts2 = default_spawn_options("task2");
    opts2.group_id = Some(AgentGroupId(42));
    let _child2 = disp.spawn(root_id, "tc-2".to_string(), opts2).unwrap();

    let agents_in_group = disp.agents_in_group(AgentGroupId(42));
    assert_eq!(agents_in_group.len(), 2, "group 42 should have 2 agents");

    let result = disp
        .send_signal_to_group(AgentGroupId(42), AgentSignal::Term)
        .await;
    assert!(result.is_ok(), "sending signal to group should succeed");
}

#[tokio::test]
async fn sig_11_send_signal_to_empty_group() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp
        .send_signal_to_group(AgentGroupId(9999), AgentSignal::Term)
        .await;
    assert!(
        result.is_ok(),
        "sending signal to empty group should succeed (0 agents affected)"
    );
}

#[tokio::test]
async fn sig_12_spawn_subagent_then_kill_it() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(root_id, "tc-1".to_string(), default_spawn_options("hello"))
        .unwrap();

    assert_eq!(
        disp.list_agents().len(),
        2,
        "should have root + child before kill"
    );

    let result = disp.send_signal(child_id, AgentSignal::Kill).await;
    assert!(result.is_ok(), "killing child should succeed");

    // Child should be removed (SIGKILL is immediate for subagents)
    let agents = disp.list_agents();
    assert_eq!(agents.len(), 1, "should have only root after killing child");
    assert_eq!(agents[0].id, root_id, "remaining agent should be root");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 5: MessageQueue Tool
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn mq_01_create_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.create_message_queue(MessageQueueName("test_q".to_string()), 64);
    assert!(result.is_ok(), "creating queue 'test_q' should succeed");
}

#[test]
fn mq_02_create_with_custom_capacity() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.create_message_queue(MessageQueueName("small_q".to_string()), 5);
    assert!(
        result.is_ok(),
        "creating queue with capacity 5 should succeed"
    );
}

#[test]
fn mq_03_create_duplicate_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("dup_q".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let result = disp.create_message_queue(name, 64);
    assert!(result.is_err(), "creating duplicate queue should fail");
}

#[test]
fn mq_04_send_message() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("sq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let msg = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("hello world"),
    };
    let result = disp.send_message(&name, msg);
    assert!(result.is_ok(), "sending message should succeed");
}

#[test]
fn mq_05_send_with_priority() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("pq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let msg = AgentMessage {
        sender: AgentId(0),
        priority: 100,
        payload: serde_json::json!("msg"),
    };
    let result = disp.send_message(&name, msg);
    assert!(
        result.is_ok(),
        "sending message with priority should succeed"
    );
}

#[test]
fn mq_06_receive_message() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("rq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let msg = AgentMessage {
        sender: AgentId(0),
        priority: 5,
        payload: serde_json::json!("test_payload"),
    };
    disp.send_message(&name, msg).unwrap();

    let received = disp.receive_message(&name).unwrap();
    assert!(
        received.is_some(),
        "should receive a message from non-empty queue"
    );
    let received = received.unwrap();
    assert_eq!(received.sender, AgentId(0), "sender should be agent 0");
    assert_eq!(received.priority, 5, "priority should be 5");
    assert_eq!(
        received.payload,
        serde_json::json!("test_payload"),
        "payload should be 'test_payload'"
    );
}

#[test]
fn mq_07_receive_from_empty_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("eq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let received = disp.receive_message(&name).unwrap();
    assert!(
        received.is_none(),
        "receiving from empty queue should return None"
    );
}

#[test]
fn mq_08_receive_from_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.receive_message(&MessageQueueName("nonexistent".to_string()));
    assert!(
        result.is_err(),
        "receiving from nonexistent queue should fail"
    );
}

#[test]
fn mq_09_queue_length() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("lq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    for i in 0..3 {
        let msg = AgentMessage {
            sender: AgentId(0),
            priority: 0,
            payload: serde_json::json!(format!("msg{}", i)),
        };
        disp.send_message(&name, msg).unwrap();
    }

    let len = disp.message_queue_len(&name).unwrap();
    assert_eq!(len, 3, "queue should have exactly 3 messages");
}

#[test]
fn mq_10_queue_length_empty() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("elq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let len = disp.message_queue_len(&name).unwrap();
    assert_eq!(len, 0, "empty queue should have 0 messages");
}

#[test]
fn mq_11_delete_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("dq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    let result = disp.delete_message_queue(&name);
    assert!(result.is_ok(), "deleting existing queue should succeed");
}

#[test]
fn mq_12_delete_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let result = disp.delete_message_queue(&MessageQueueName("nonexistent".to_string()));
    assert!(result.is_err(), "deleting nonexistent queue should fail");
}

#[test]
fn mq_13_send_to_nonexistent_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);

    let msg = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("hello"),
    };
    let result = disp.send_message(&MessageQueueName("no_such_q".to_string()), msg);
    assert!(result.is_err(), "sending to nonexistent queue should fail");
}

#[test]
fn mq_14_send_to_full_queue() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("full_q".to_string());

    disp.create_message_queue(name.clone(), 1).unwrap();

    let msg1 = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("first"),
    };
    disp.send_message(&name, msg1).unwrap();

    let msg2 = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("second"),
    };
    let result = disp.send_message(&name, msg2);
    assert!(
        result.is_err(),
        "sending to full queue (capacity=1) should fail"
    );
}

#[test]
fn mq_15_fifo_ordering() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("fifo_q".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();

    let msg1 = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("first"),
    };
    let msg2 = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("second"),
    };
    disp.send_message(&name, msg1).unwrap();
    disp.send_message(&name, msg2).unwrap();

    let r1 = disp.receive_message(&name).unwrap().unwrap();
    assert_eq!(
        r1.payload,
        serde_json::json!("first"),
        "first receive should return 'first'"
    );

    let r2 = disp.receive_message(&name).unwrap().unwrap();
    assert_eq!(
        r2.payload,
        serde_json::json!("second"),
        "second receive should return 'second'"
    );
}

#[test]
fn mq_16_delete_then_access() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let name = MessageQueueName("dtq".to_string());

    disp.create_message_queue(name.clone(), 64).unwrap();
    disp.delete_message_queue(&name).unwrap();

    let msg = AgentMessage {
        sender: AgentId(0),
        priority: 0,
        payload: serde_json::json!("hello"),
    };
    let result = disp.send_message(&name, msg);
    assert!(result.is_err(), "sending to deleted queue should fail");
}

// ═══════════════════════════════════════════════════════════════════════════════
// Group 6: Cross-Feature Combinations
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn combo_01_subagent_ps_visible() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(root_id, "tc-1".to_string(), default_spawn_options("task"))
        .unwrap();

    // /ps should show both
    let agents = disp.list_agents();
    assert_eq!(agents.len(), 2, "should see root + child in /ps");

    // /tree should show hierarchy
    let tree = disp.agent_tree().unwrap();
    assert_eq!(tree.children.len(), 1, "tree should show 1 child");
    assert_eq!(
        tree.children[0].info.id, child_id,
        "tree child should be the spawned agent"
    );
}

#[test]
fn combo_02_named_subagent_in_list() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let options = SpawnOptions {
        prompt: "work".to_string(),
        name: Some("worker".to_string()),
        ..SpawnOptions::default()
    };
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    let agents = disp.list_agents();
    let child_info = agents.iter().find(|a| a.id == child_id).unwrap();
    assert_eq!(
        child_info.name.as_deref(),
        Some("worker"),
        "list_agents should show 'worker' name"
    );
}

#[test]
fn combo_03_subagent_set_priority() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(root_id, "tc-1".to_string(), default_spawn_options("task"))
        .unwrap();

    disp.set_priority(child_id, -10).unwrap();
    let agents = disp.list_agents();
    let child_info = agents.iter().find(|a| a.id == child_id).unwrap();
    assert_eq!(child_info.priority, -10, "child priority should be -10");
}

#[test]
fn combo_04_subagent_resource_usage() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(root_id, "tc-1".to_string(), default_spawn_options("task"))
        .unwrap();

    let usage = disp.get_resource_usage(child_id).unwrap();
    assert_eq!(usage.total_turns, 0, "new child should have 0 turns");
    assert_eq!(
        usage.total_tool_calls, 0,
        "new child should have 0 tool calls"
    );
}

#[test]
fn combo_05_subagent_message_queue_ipc() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Create a queue for IPC
    let queue_name = MessageQueueName("ipc".to_string());
    disp.create_message_queue(queue_name.clone(), 64).unwrap();

    // Spawn a child agent
    let child_id = disp
        .spawn(
            root_id,
            "tc-1".to_string(),
            default_spawn_options("send hello"),
        )
        .unwrap();

    // Simulate the child sending a message
    let msg = AgentMessage {
        sender: child_id,
        priority: 0,
        payload: serde_json::json!("hello from child"),
    };
    disp.send_message(&queue_name, msg).unwrap();

    // Root receives the message
    let received = disp.receive_message(&queue_name).unwrap().unwrap();
    assert_eq!(
        received.sender, child_id,
        "message sender should be the child agent"
    );
    assert_eq!(
        received.payload,
        serde_json::json!("hello from child"),
        "payload should match what child sent"
    );
}

#[tokio::test]
async fn combo_06_group_signal_to_subagents() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Spawn 2 subagents in same group
    let mut opts1 = default_spawn_options("task1");
    opts1.group_id = Some(AgentGroupId(10));
    let _child1 = disp.spawn(root_id, "tc-1".to_string(), opts1).unwrap();

    let mut opts2 = default_spawn_options("task2");
    opts2.group_id = Some(AgentGroupId(10));
    let _child2 = disp.spawn(root_id, "tc-2".to_string(), opts2).unwrap();

    // Verify both are in the group
    let in_group = disp.agents_in_group(AgentGroupId(10));
    assert_eq!(in_group.len(), 2, "group 10 should have 2 agents");

    // Send SIGKILL to group — both should be removed
    disp.send_signal_to_group(AgentGroupId(10), AgentSignal::Kill)
        .await
        .unwrap();

    // Both children should be removed
    let agents = disp.list_agents();
    assert_eq!(
        agents.len(),
        1,
        "only root should remain after killing group"
    );
}

#[test]
fn combo_07_session_creation_and_listing() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let new_session = disp.create_session(root_id).unwrap();

    let agents = disp.list_agents();
    let root_info = agents.iter().find(|a| a.id == root_id).unwrap();
    assert_eq!(
        root_info.session_id, new_session,
        "root session_id should be the newly created session"
    );
}

#[test]
fn combo_08_resource_limit_set_on_subagent() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut options = default_spawn_options("multi-turn task");
    options
        .resource_limits
        .set(ResourceKind::MaxTurns, ResourceLimit { soft: 1, hard: 1 });
    let child_id = disp.spawn(root_id, "tc-1".to_string(), options).unwrap();

    // The limit is set — resource checking happens during the event loop
    // Verify the child exists and has the limit
    assert!(
        disp.get_resource_usage(child_id).is_ok(),
        "child should exist"
    );
}

#[test]
fn combo_09_tree_with_deep_nesting() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut opts1 = default_spawn_options("level 1");
    opts1.allow_nested_spawn = true;
    opts1.max_depth = 5;
    let child = disp.spawn(root_id, "tc-1".to_string(), opts1).unwrap();

    let mut opts2 = default_spawn_options("level 2");
    opts2.max_depth = 5;
    let grandchild = disp.spawn(child, "tc-2".to_string(), opts2).unwrap();

    let tree = disp.agent_tree().unwrap();
    assert_eq!(tree.children.len(), 1, "root should have 1 child");
    assert_eq!(
        tree.children[0].children.len(),
        1,
        "child should have 1 grandchild"
    );
    assert_eq!(
        tree.children[0].children[0].info.id, grandchild,
        "grandchild should be at depth 2 in tree"
    );
}

#[test]
fn combo_10_subagent_exclusive_scheduling() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let child_id = disp
        .spawn(
            root_id,
            "tc-1".to_string(),
            default_spawn_options("exclusive task"),
        )
        .unwrap();

    disp.set_scheduling_policy(child_id, SchedulingPolicy::Exclusive)
        .unwrap();

    let agents = disp.list_agents();
    let child_info = agents.iter().find(|a| a.id == child_id).unwrap();
    assert_eq!(
        child_info.policy,
        SchedulingPolicy::Exclusive,
        "child should have Exclusive scheduling policy"
    );
}

// ═══════════════════════════════════════════════════════════════════════════════
// Additional edge cases
// ═══════════════════════════════════════════════════════════════════════════════

#[test]
fn edge_spawn_increments_parent_children_spawned() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    disp.spawn(root_id, "tc-1".to_string(), default_spawn_options("a"))
        .unwrap();
    disp.spawn(root_id, "tc-2".to_string(), default_spawn_options("b"))
        .unwrap();

    let usage = disp.get_resource_usage(root_id).unwrap();
    assert_eq!(
        usage.children_spawned, 2,
        "root should have children_spawned=2 after spawning 2 subagents"
    );
}

#[test]
fn edge_set_agent_group_moves_between_groups() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Root starts in group 0
    let initial_group = disp.descriptor(root_id).unwrap().group_id;
    assert_eq!(initial_group, AgentGroupId(0), "root starts in group 0");

    // Move to group 42
    disp.set_agent_group(root_id, AgentGroupId(42)).unwrap();
    assert_eq!(
        disp.descriptor(root_id).unwrap().group_id,
        AgentGroupId(42),
        "root should be in group 42"
    );

    // Old group should be empty
    assert!(
        disp.agents_in_group(AgentGroupId(0)).is_empty(),
        "group 0 should be empty after moving root out"
    );

    // New group should have root
    assert_eq!(
        disp.agents_in_group(AgentGroupId(42)),
        vec![root_id],
        "group 42 should contain root"
    );
}

#[test]
fn edge_agents_in_session() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let agents_in_session = disp.agents_in_session(SessionId(0));
    assert_eq!(
        agents_in_session.len(),
        1,
        "session 0 should have 1 agent (root)"
    );
    assert!(
        agents_in_session.contains(&root_id),
        "session 0 should contain root"
    );
}

#[test]
fn edge_spawn_depth_limit_exceeded() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Spawn with max_depth=1 (only 1 level of children allowed)
    let mut opts = default_spawn_options("level 1");
    opts.max_depth = 2; // allow depth 0->1
    let child = disp.spawn(root_id, "tc-1".to_string(), opts).unwrap();

    // Try to spawn from child with max_depth=1 (child is at depth 1, so depth >= max)
    let mut nested_opts = default_spawn_options("level 2");
    nested_opts.max_depth = 1; // depth limit is 1, child is already at depth 1
    let result = disp.spawn(child, "tc-2".to_string(), nested_opts);
    assert!(
        result.is_err(),
        "spawning at depth >= max_depth should fail"
    );
}

#[test]
fn edge_max_children_limit() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Set MaxChildren limit on root
    disp.set_resource_limit(
        root_id,
        ResourceKind::MaxChildren,
        ResourceLimit { soft: 1, hard: 2 },
    )
    .unwrap();

    disp.spawn(root_id, "tc-1".to_string(), default_spawn_options("a"))
        .unwrap();
    disp.spawn(root_id, "tc-2".to_string(), default_spawn_options("b"))
        .unwrap();

    // Third spawn should fail (hard limit = 2)
    let result = disp.spawn(root_id, "tc-3".to_string(), default_spawn_options("c"));
    assert!(
        result.is_err(),
        "spawning beyond MaxChildren hard limit should fail"
    );
}

#[tokio::test]
async fn edge_kill_subagent_removes_descendants() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut opts = default_spawn_options("parent");
    opts.max_depth = 5;
    let child = disp.spawn(root_id, "tc-1".to_string(), opts).unwrap();

    let mut nested_opts = default_spawn_options("child");
    nested_opts.max_depth = 5;
    let _grandchild = disp.spawn(child, "tc-2".to_string(), nested_opts).unwrap();

    assert_eq!(
        disp.list_agents().len(),
        3,
        "should have root + child + grandchild"
    );

    // Kill the child — should also remove grandchild
    disp.send_signal(child, AgentSignal::Kill).await.unwrap();

    let agents = disp.list_agents();
    assert_eq!(
        agents.len(),
        1,
        "only root should remain after killing subtree"
    );
    assert_eq!(agents[0].id, root_id, "remaining agent should be root");
}

#[test]
fn edge_message_queue_length_nonexistent() {
    let tmp = TempDir::new().unwrap();
    let disp = make_dispatcher(&tmp);

    let result = disp.message_queue_len(&MessageQueueName("nope".to_string()));
    assert!(result.is_err(), "length of nonexistent queue should fail");
}

#[test]
fn edge_set_priority_range() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    // Set to minimum priority
    disp.set_priority(root_id, -10).unwrap();
    let agents = disp.list_agents();
    assert_eq!(agents[0].priority, -10, "priority should be -10 (highest)");

    // Set to maximum priority
    disp.set_priority(root_id, 10).unwrap();
    let agents = disp.list_agents();
    assert_eq!(agents[0].priority, 10, "priority should be 10 (lowest)");
}

#[test]
fn edge_create_multiple_sessions() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let s1 = disp.create_session(root_id).unwrap();
    let s2 = disp.create_session(root_id).unwrap();

    assert_ne!(s1, s2, "each create_session should produce a unique ID");
    assert_eq!(
        disp.descriptor(root_id).unwrap().session_id,
        s2,
        "root should be in the most recently created session"
    );
}

#[test]
fn edge_agent_tree_from_child() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let mut opts = default_spawn_options("child");
    opts.max_depth = 5;
    let child = disp.spawn(root_id, "tc-1".to_string(), opts).unwrap();

    let mut nested_opts = default_spawn_options("grandchild");
    nested_opts.max_depth = 5;
    let grandchild = disp.spawn(child, "tc-2".to_string(), nested_opts).unwrap();

    // Get tree rooted at child
    let tree = disp.agent_tree_from(child).unwrap();
    assert_eq!(tree.info.id, child, "subtree root should be child");
    assert_eq!(tree.children.len(), 1, "child should have 1 descendant");
    assert_eq!(
        tree.children[0].info.id, grandchild,
        "child's descendant should be grandchild"
    );
}

#[test]
fn edge_set_all_resource_kinds() {
    let tmp = TempDir::new().unwrap();
    let mut disp = make_dispatcher(&tmp);
    let root_id = disp.root_id();

    let kinds = [
        ResourceKind::TotalTokens,
        ResourceKind::InputTokens,
        ResourceKind::OutputTokens,
        ResourceKind::WallTime,
        ResourceKind::MaxTurns,
        ResourceKind::MaxToolCalls,
        ResourceKind::MaxChildren,
        ResourceKind::MaxDepth,
    ];

    for kind in &kinds {
        let result = disp.set_resource_limit(
            root_id,
            *kind,
            ResourceLimit {
                soft: 100,
                hard: 200,
            },
        );
        assert!(
            result.is_ok(),
            "set_resource_limit for {:?} should succeed",
            kind
        );
    }
}
