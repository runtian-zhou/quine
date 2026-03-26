---
status: done
---

# Plan Mode

## Overview

Add a "plan mode" to the quine agent that restricts the LLM to read-only exploration and structured plan output. In plan mode, the agent cannot modify files or run destructive commands — it can only read, search, and produce an implementation plan. This is useful for understanding a codebase and designing a strategy before committing to changes.

Plan mode is toggled via a TUI key binding or CLI flag. When active, the system prompt switches to a planning-focused prompt that instructs the LLM to explore and plan but not modify, and the tool set is restricted to read-only tools.

Reference: Claude Code's plan mode prompt at `https://github.com/Piebald-AI/claude-code-system-prompts/blob/main/system-prompts/agent-prompt-plan-mode-enhanced.md`.

## Requirements

### 1. Plan Mode System Prompt (`crates/quine-core/src/engine.rs`)

Add a constant `PLAN_MODE_SYSTEM_PROMPT`:

```rust
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
```

### 2. Plan Mode Flag in Session (`crates/quine-core/src/engine.rs`)

Add `plan_mode: bool` to `SessionContext`. When `plan_mode` is true:

- Use `PLAN_MODE_SYSTEM_PROMPT` instead of (or prepended to) the default/CLAUDE.md prompt
- Restrict the tool registry: only register `ReadTool`, `FindTool`, `BashTool`, `PlanTool`, and `AskUserTool` — omit `WriteTool`, `SpawnTool`, `SubagentTool`, `SignalTool`, `SendMessageTool`, `RecvMessageTool`, `WaitChildTool`
- The bash permission checker already handles dangerous commands, but plan mode adds an extra layer

### 3. Session Creation with Plan Mode (`crates/quine-core/src/channel.rs`, `crates/quine-harness/`)

Add `plan_mode: bool` to `CoreInput::CreateSession` and `SessionConfig`:

```rust
// channel.rs
CreateSession {
    session_id: SessionId,
    system_prompt: Option<String>,
    working_directory: Option<PathBuf>,
    skills: Vec<Skill>,
    plan_mode: bool,  // NEW
    reply: oneshot::Sender<Result<(), String>>,
}
```

```rust
// config.rs
pub struct SessionConfig {
    pub system_prompt: Option<String>,
    pub working_directory: Option<std::path::PathBuf>,
    pub skills: Vec<String>,
    pub plan_mode: bool,  // NEW
}
```

Update `SessionContext::new` to accept `plan_mode` and configure accordingly.

### 4. TUI Plan Mode Toggle (`crates/quine-cli/src/tui/`)

**App state** (`app.rs`): Add `pub plan_mode: bool` to `App`.

**Key binding** (`mod.rs`): Add `Ctrl+P` to toggle plan mode when idle:
```rust
KeyCode::Char('p') if modifiers.contains(KeyModifiers::CONTROL) => {
    if app.phase == AgentPhase::Idle {
        app.plan_mode = !app.plan_mode;
        // Push indicator to conversation
        let mode = if app.plan_mode { "ON" } else { "OFF" };
        app.messages.push(ConversationEntry::Error(
            format!("Plan mode: {mode}")
        ));
    }
    None
}
```

Note: toggling plan mode mid-session requires creating a new session with the new mode. The simpler approach: plan mode is set at session creation and indicated visually but not toggled mid-session. Instead, offer `--plan` CLI flag.

**Input label** (`app.rs`): When plan_mode is active, prefix the label:
```rust
if self.plan_mode {
    "[plan] > ".to_string()
} else {
    "> ".to_string()
}
```

**Visual indicator** (`ui.rs`): Show a `[PLAN MODE]` badge in the input box border or title when active.

### 5. CLI Flag (`crates/quine-cli/src/main.rs`)

Add `--plan` flag to the `Chat` command:

```rust
Chat {
    #[arg(long)]
    socket: Option<String>,
    #[arg(long)]
    plan: bool,  // NEW
    // ...
}
```

Pass `plan_mode: true` when creating the session. Same for the TUI chat entry point.

### 6. Server/Harness Updates

- `crates/quine-harness/src/server.rs`: Extract `plan_mode` from CREATE_SESSION params
- `crates/quine-harness/src/local.rs`: Forward `plan_mode` in `CoreInput::CreateSession`

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- `quine chat --plan` starts a plan-mode session with restricted tools and planning prompt
- In plan mode, WriteTool and destructive tools are not available
- The TUI shows `[plan]` prefix in the input label
- The LLM produces structured plans with file references and steps
- Existing non-plan sessions work unchanged
- New unit tests:
  - `plan_mode_restricts_tools` — verify only read-only tools registered
  - `plan_mode_system_prompt` — verify planning prompt is used
  - `plan_mode_label` — verify `[plan] >` input label

## QA Test Cases

```json
[
  {
    "name": "plan_mode_session",
    "description": "Plan mode session restricts to read-only and produces a plan",
    "steps": [
      "Start: quine chat --plan",
      "Send: How would I add a new grep tool to this project?",
      "Verify the agent explores the codebase (read_file, find, bash ls/cat)",
      "Verify the agent does NOT write or modify files",
      "Verify the response includes an implementation plan with file paths"
    ]
  },
  {
    "name": "plan_mode_no_write",
    "description": "Plan mode prevents file modifications",
    "steps": [
      "Start plan mode session",
      "Ask the agent to create a new file",
      "Verify it refuses or the write tool is not available"
    ]
  }
]
```

## Non-Goals (Deferred)

- Mid-session plan mode toggle (requires session recreation)
- Plan mode with automatic transition to implementation mode
- Plan diff/review before applying changes
- Plan export to markdown file
- Collaborative plan editing between agents
