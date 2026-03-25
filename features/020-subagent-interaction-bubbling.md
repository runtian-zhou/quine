---
status: done
---

# Subagent Interaction Bubbling

## Overview

Subagents currently cannot ask the user questions — `AskUserTool` is not registered and `interaction_channel` is `None` in their `ExecutionContext`. This feature enables subagents to use `ask_user`, with questions bubbling up through the parent agent to the TUI. The TUI must clearly show **which agent** is requesting input so the user knows who they are responding to.

## Requirements

### 1. Add `source_label` to InteractionRequest

**File**: `crates/quine-core/src/tool/mod.rs`

Add a `source_label` field to `InteractionRequest` so the TUI can display which agent is asking:

```rust
pub struct InteractionRequest {
    pub prompt: String,
    pub kind: InteractionKind,
    pub options: Vec<SelectOption>,
    pub allow_freeform: bool,
    pub source_label: Option<String>, // e.g. "subagent: <task excerpt>"
}
```

Default to `None` (backward-compatible). The engine sets this when forwarding from a subagent.

### 2. Make SubagentTool interactive and pass the parent's channel

**File**: `crates/quine-core/src/tool/subagent.rs`

a. Override `is_interactive()` to return `true`:

```rust
fn is_interactive(&self) -> bool {
    true
}
```

This causes the parent engine to run SubagentTool in a spawned task with `tokio::select!` on an interaction channel (see `engine.rs:303-383`).

b. In `execute()`, extract the `interaction_channel` from the `ExecutionContext` and pass it to `run_subagent_inner()`:

```rust
async fn execute(&self, arguments: Value, context: &ExecutionContext) -> Result<ToolOutput, ToolError> {
    // ... parse arguments ...
    let channel = context.interaction_channel.clone();
    run_subagent_inner(/* ..., */ channel, /* ... */).await
}
```

c. Ensure `InteractionChannel` is `Clone`. It wraps an `mpsc::Sender` which is already `Clone`.

**File**: `crates/quine-core/src/tool/mod.rs`

```rust
#[derive(Clone)]
pub struct InteractionChannel {
    pub(crate) request_tx: mpsc::Sender<(InteractionRequest, oneshot::Sender<InteractionResponse>)>,
}
```

### 3. Register AskUserTool in subagent and thread the channel

**File**: `crates/quine-core/src/tool/subagent.rs`

In `run_subagent_inner()`:

a. Add `AskUserTool` to the subagent's `ToolRegistry` (~line 149):

```rust
registry.register(Arc::new(AskUserTool));
```

b. Accept the parent's `InteractionChannel` as a parameter and set it on the child `ExecutionContext` (~line 245):

```rust
let ctx = ExecutionContext {
    session_id,
    filesystem: Arc::clone(&filesystem),
    working_directory: working_directory.clone(),
    interaction_channel: parent_channel.clone(), // was: None
    plan_store: plan_store.clone(),
    core_input: None,
};
```

### 4. Annotate subagent interaction requests with source_label

**File**: `crates/quine-core/src/tool/subagent.rs`

When a subagent's `ask_user` sends an `InteractionRequest`, it goes through the parent's `InteractionChannel`. The parent engine picks it up and emits `CoreOutput::InteractionNeeded`.

To label the source, the subagent wrapper should intercept requests and set `source_label`. Create a thin wrapper channel that annotates requests before forwarding:

```rust
fn wrap_channel_with_label(
    parent: &InteractionChannel,
    label: String,
) -> InteractionChannel {
    let parent_tx = parent.request_tx.clone();
    let (child_tx, mut child_rx) = mpsc::channel::<(InteractionRequest, oneshot::Sender<InteractionResponse>)>(1);

    tokio::spawn(async move {
        while let Some((mut req, reply)) = child_rx.recv().await {
            req.source_label = Some(label.clone());
            let _ = parent_tx.send((req, reply)).await;
        }
    });

    InteractionChannel { request_tx: child_tx }
}
```

Call this when building the child `ExecutionContext`, using a truncated version of the subagent task as the label:

```rust
let label = format!("subagent: {}", truncate(&task_description, 60));
let child_channel = parent_channel
    .as_ref()
    .map(|ch| wrap_channel_with_label(ch, label));
```

### 5. Propagate source_label through the harness to the TUI

**File**: `crates/quine-harness/src/server.rs`

In `core_output_to_notification()` for `InteractionNeeded` (~line 808), include `source_label` in the JSON-RPC notification params:

```rust
CoreOutput::InteractionNeeded { session_id, request } => {
    let mut params = serde_json::json!({
        "session_id": session_id.to_string(),
        "prompt": request.prompt,
        "kind": format!("{:?}", request.kind),
        // ... existing fields ...
    });
    if let Some(label) = &request.source_label {
        params["source_label"] = serde_json::Value::String(label.clone());
    }
    // ...
}
```

### 6. Display source_label in the TUI

**File**: `crates/quine-cli/src/tui/app.rs`

a. Add `source_label` to `PendingInteraction`:

```rust
pub struct PendingInteraction {
    pub prompt: String,
    pub kind: InteractionKind,
    pub options: Vec<String>,
    pub allow_freeform: bool,
    pub source_label: Option<String>,
}
```

b. When receiving an `INTERACTION_NEEDED` notification (~line 608), extract `source_label` from params and include it in `PendingInteraction`.

c. Update `input_label()` (~line 332) to show the source:

```rust
pub fn input_label(&self) -> String {
    if let Some(interaction) = self.interaction_queue.front() {
        let source = interaction.source_label
            .as_deref()
            .unwrap_or("agent");
        let suffix = if self.interaction_queue.len() > 1 {
            format!(" (+{} pending)", self.interaction_queue.len() - 1)
        } else {
            String::new()
        };
        match interaction.kind {
            InteractionKind::Confirmation => format!("[{source}] ⚠ Permission{suffix}: "),
            InteractionKind::Question => format!("[{source}] ❓ Question{suffix}: "),
            // ... etc
        }
    } else {
        "> ".into()
    }
}
```

d. When pushing the interaction message to the conversation view, prefix with the source label so the user sees which agent is asking before they even reach the input prompt.

### 7. Display source_label in oneshot (run) mode

**File**: `crates/quine-cli/src/run.rs`

When printing `interaction needed` (~line 130), include the source_label if present:

```rust
if let Some(label) = params.get("source_label").and_then(|v| v.as_str()) {
    eprintln!("interaction needed [{label}]: {prompt}");
} else {
    eprintln!("interaction needed: {prompt}");
}
```

Same for `--json` mode: include `source_label` in the JSON output.

## Acceptance Criteria

- `cargo build`, `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --all -- --check` must pass
- Subagent can call `ask_user` and the question appears in the TUI
- TUI input label shows the source (e.g. `[subagent: summarize the codebase] ❓ Question: `)
- User response flows back to the subagent and execution continues
- Existing subagent tests pass (subagent without interaction channel still works)
- Existing interaction tests pass (parent agent interactions still work with `source_label: None`)
- Permission prompts from the parent agent show `[agent]` as the source

### Unit Tests

- `subagent_ask_user_bubbles_to_parent` (in `subagent.rs`): Mock LLM makes the subagent call `ask_user`. Verify the `InteractionRequest` arrives on the parent's channel with `source_label` set. Send a response and verify the subagent completes with the user's answer.
- `subagent_without_interaction_channel_still_works` (in `subagent.rs`): Existing behavior — subagent with `interaction_channel: None` works for non-interactive tools.
- `wrap_channel_with_label_sets_source` (in `subagent.rs`): Verify the wrapper sets `source_label` on forwarded requests.
- `pending_interaction_shows_source_label` (in `tui/app.rs`): Verify `input_label()` includes the source label when present.
- `pending_interaction_no_source_label_shows_agent` (in `tui/app.rs`): Verify `input_label()` shows "agent" as default when `source_label` is `None`.

## QA Test Cases (add to `.claude/qa-tests.md`)

```markdown
## subagent_ask_user
**Description**: Verify subagent can bubble up ask_user questions to the user with source label.
- **Flags**: `--json`
- **Turn 1**:
  - **Send**: `"Use the subagent tool to spawn a child agent with this task: 'Ask the user what their favorite color is using the ask_user tool, then report their answer.' Report what the subagent returned."`
  - **Expect**: JSON output contains `interaction_needed: true` and `source_label` containing `subagent`
  - **Extract**: `session_id` from JSON output
- **Turn 2**:
  - **Flags**: `--session <session_id from turn 1>` (via respond)
  - **Send**: `"blue"`
  - **Expect**: Output contains `blue`
```

## Non-Goals (Deferred)

- **Nested subagent interaction**: Sub-subagents bubbling questions through multiple layers — only one level deep is required.
- **Permission prompt bubbling from subagents**: Currently subagents auto-allow bash commands. Changing this is out of scope.
- **Concurrent interaction from multiple subagents**: If multiple subagents ask simultaneously, they queue in FIFO order. No priority or multiplexing.
