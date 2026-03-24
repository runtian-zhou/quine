---
status: in-progress
---

# Permission Checker Agent for Tool Execution

## Overview

Add a permission checking layer that intercepts tool execution requests (starting with `BashTool`) and evaluates them for risk before allowing execution. Two checker implementations: an LLM-based agent that scores commands for danger, and a rule-based checker that matches against known dangerous patterns. If a command is deemed dangerous, the user is prompted for confirmation via the existing `InteractionChannel` before proceeding.

## Requirements

### 1. `PermissionChecker` Trait (`quine-core/src/permission/mod.rs`)

```rust
#[async_trait]
pub trait PermissionChecker: Send + Sync {
    /// Evaluate a tool call and return a permission decision.
    async fn check(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        context: &PermissionContext,
    ) -> Result<PermissionDecision, PermissionError>;
}
```

**`PermissionDecision`**:
```rust
pub enum PermissionDecision {
    /// Safe to execute without confirmation.
    Allow,
    /// Potentially dangerous — requires user confirmation.
    /// Includes a risk score (0.0–1.0) and explanation.
    RequiresConfirmation {
        risk_score: f64,
        reason: String,
    },
    /// Blocked outright — must not execute.
    Deny {
        reason: String,
    },
}
```

**`PermissionContext`**: metadata about the current session/tool invocation:
```rust
pub struct PermissionContext {
    pub session_id: SessionId,
    pub working_directory: PathBuf,
}
```

**`PermissionError`**: thiserror enum for checker failures (LLM unavailable, parse error, etc.)

### 2. LLM-Based Checker (`quine-core/src/permission/llm_checker.rs`)

An agent that uses the existing `LlmProvider` to evaluate tool calls:

- Sends the tool name and arguments to the LLM with a system prompt asking it to evaluate the risk.
- System prompt instructs the LLM to respond with a JSON object: `{ "score": 0.0-1.0, "reason": "...", "decision": "allow"|"confirm"|"deny" }`.
- Thresholds: score < 0.3 → `Allow`, 0.3–0.7 → `RequiresConfirmation`, > 0.7 → `Deny`.
- Uses a separate, lightweight LLM call (not part of the main conversation).
- Falls back to `RequiresConfirmation` if the LLM response can't be parsed.
- Constructor takes a `Box<dyn LlmProvider>` — can use the same provider as the main agent or a different (cheaper/faster) one.

### 3. Rule-Based Checker (`quine-core/src/permission/rule_checker.rs`)

Pattern-matching checker that doesn't require an LLM:

- Maintains a list of dangerous patterns/commands with associated risk scores.
- **High risk (deny/confirm)**: `rm -rf /`, `sudo`, `chmod 777`, `mkfs`, `dd if=`, `:(){:|:&};:`, `> /dev/sda`, `shutdown`, `reboot`, `kill -9 1`, pipe to `sh`/`bash` from `curl`/`wget`, `git push --force`, `git reset --hard`, `DROP TABLE`, `DELETE FROM` without WHERE.
- **Medium risk (confirm)**: `rm -rf` (any path), `chmod`, `chown`, `mv /`, `git push`, network commands (`curl`, `wget` — data exfiltration risk), `pip install`, `npm install -g`, package installation commands.
- **Low risk (allow)**: `ls`, `cat`, `echo`, `pwd`, `grep`, `find`, `cargo build`, `cargo test`, `git status`, `git log`, `git diff`, read-only operations.
- Configurable: users can add custom allow/deny patterns.
- Pattern matching: regex-based on the full command string.

### 4. Composite Checker (`quine-core/src/permission/composite.rs`)

Chains multiple checkers and takes the most restrictive decision:

```rust
pub struct CompositeChecker {
    checkers: Vec<Box<dyn PermissionChecker>>,
}
```

- Runs all checkers in order.
- Final decision = most restrictive across all results (Deny > RequiresConfirmation > Allow).
- If any checker returns `Deny`, the final decision is `Deny`.
- If any checker returns `RequiresConfirmation`, the highest risk score and its reason are used.

### 5. Integration with Engine (`quine-core/src/engine.rs`)

- Add a `PermissionChecker` to the core engine (optional — if `None`, all tools are allowed).
- Before executing any tool in `execute_tool_call()`:
  1. Call `checker.check(tool_name, &arguments, &context)`.
  2. If `Allow` → proceed.
  3. If `RequiresConfirmation` → send an `InteractionRequest` via the tool's `InteractionChannel` asking the user: "Tool `{tool_name}` with args `{args}` scored {risk_score} risk: {reason}. Allow? [y/N]". If the user confirms → proceed. If denied → return `ToolError::PermissionDenied`.
  4. If `Deny` → return `ToolError::PermissionDenied` with the reason.
- The permission check is tool-agnostic — it applies to `BashTool` primarily but the trait works for any tool.

### 6. Configuration

- `run_core_loop` accepts an `Option<Box<dyn PermissionChecker>>`.
- Default setup in harness: `CompositeChecker` with `RuleBasedChecker` always enabled, `LlmChecker` optionally enabled via `PERMISSION_LLM_ENABLED=true` env var.
- `CreateSession` or engine config determines which checkers are active.

### 7. Crate Structure

```
crates/quine-core/src/
  permission/
    mod.rs              # PermissionChecker trait, PermissionDecision, PermissionContext, PermissionError
    rule_checker.rs     # RuleBasedChecker
    llm_checker.rs      # LlmChecker
    composite.rs        # CompositeChecker
```

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Unit tests for `RuleBasedChecker`: dangerous commands return `Deny`/`RequiresConfirmation`, safe commands return `Allow`.
- Unit tests for `LlmChecker`: mock the provider, verify scoring and decision mapping.
- Unit tests for `CompositeChecker`: verify most-restrictive-wins logic.
- Integration test: bash tool with `rm -rf /` is intercepted, user confirmation requested.
- Existing tests continue to pass (engine tests use `None` for the checker).

## Non-Goals (Deferred)

- Per-user permission profiles.
- Persistent allow/deny lists across sessions.
- Permission caching (same command approved once → auto-approve).
- UI for managing permission rules.
