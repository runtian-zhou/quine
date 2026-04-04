# 045 Tool Permission Integration — Implementation Plan

Short summary: Adopt the shared permission engine across Quine’s current tools by adding per-tool permission request builders and conservative classifications, while preserving existing `Tool` trait boundaries.

## Open Questions

- None. This draft is intentionally scoped to Feature 3 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

pending — Reviewed the current QA draft and found blocking alignment gaps. The QA plan still needs concrete executable scenarios, including exact daemon/chat commands, round-by-round prompts, and expected permission/tool outputs before this implementation plan can mark agreement.

## Proposed Design

- Tie tool integration to the concrete tool modules that exist today under `crates/quine-core/src/tool/`:
  - `read.rs`
  - `find.rs`
  - `write.rs` (which currently backs the `apply_patch` tool name)
  - `bash.rs`
  - `spawn.rs`, `subagent.rs`, `signal.rs`, `wait_child.rs`, `send_message.rs`, and `recv_message.rs`
  - `ask_user.rs` and `plan.rs` as special interactive/planning-adjacent cases
- Keep the `Tool` trait in `crates/quine-core/src/tool/mod.rs` unchanged. Instead, implement internal permission-request builders close to each tool so the current trait boundary remains stable.
- Add a small internal adapter pattern for tool permission integration:
  - parse arguments into the tool’s existing typed input struct
  - derive a `PermissionRequest` from those typed arguments plus `ExecutionContext`
  - call the shared evaluator before running the mutating/external action
  - map denied decisions onto existing `ToolError::PermissionDenied`
  - preserve existing tool output behavior for successful executions
- Align request construction with current metadata already exposed to the LLM:
  - `read.rs` and `find.rs` should emit read-scoped requests rooted in `ExecutionContext.working_directory` and the session filesystem
  - `write.rs` should emit write-scoped requests for target paths and patch operations
  - `bash.rs` should emit execute-scoped requests, leaving detailed command-risk refinement to Feature 6
  - `spawn.rs`, `subagent.rs`, `signal.rs`, and `wait_child.rs` should emit process/agent-control requests
  - `send_message.rs` and `recv_message.rs` should be reviewed for whether they require permission coverage now or remain governed by existing session topology constraints
  - `ask_user.rs` likely remains allowed as an interaction surface rather than being subjected to the same external-action policy as file/process tools
- Keep plan-mode runtime behavior and tool-definition filtering in sync:
  - `engine.rs` and `tool/mod.rs` already omit certain mutating tools from `built_in_tool_definitions(plan_mode)`
  - this feature should ensure runtime permission checks do not drift from that existing availability behavior
  - any tool still present in non-plan sessions must construct permission requests consistent with its `is_read_only()` and `is_idempotent()` metadata
- Reuse the existing session/filesystem abstractions rather than introducing ad hoc path checks inside each tool:
  - `ExecutionContext.filesystem` and `SessionFilesystem::resolve_path` should feed request construction for file tools
  - later filesystem-boundary hardening in Feature 5 should build on these request shapes instead of rewriting tool integration again
- Preserve conservative defaults:
  - tools currently marked read-only remain low-risk candidates but still use the shared evaluator
  - write, execute, and agent-control tools should not auto-allow absent explicit policy or safe defaults from the evaluator
  - do not broaden concurrent or idempotent behavior merely because a permission request exists

## File-by-File Changes

- `crates/quine-core/src/tool/read.rs`
  - Add request construction for file reads using resolved target paths and read scope.
- `crates/quine-core/src/tool/find.rs`
  - Add request construction for directory traversal/search operations using read scope and requested path roots.
- `crates/quine-core/src/tool/write.rs`
  - Add request construction for patch/write operations using write scope and final target paths.
- `crates/quine-core/src/tool/bash.rs`
  - Add execute-scoped permission request construction using raw command text and working-directory context.
- `crates/quine-core/src/tool/spawn.rs`
  - Add agent/process-control request construction for child-session creation.
- `crates/quine-core/src/tool/subagent.rs`
  - Add agent-control request construction for autonomous child execution.
- `crates/quine-core/src/tool/signal.rs`
  - Add process-control request construction for session signaling.
- `crates/quine-core/src/tool/wait_child.rs`, `send_message.rs`, `recv_message.rs`, and `ask_user.rs`
  - Review whether each tool needs explicit permission requests now; document and implement the minimal consistent policy path.
- `crates/quine-core/src/tool/mod.rs`
  - Add shared internal helper(s) for evaluate-before-execute flow if multiple tools would otherwise duplicate the same evaluator plumbing.
- `crates/quine-core/src/engine.rs`
  - Ensure runtime dispatch still surfaces denials cleanly as tool results/errors and that plan-mode tool availability does not drift from runtime enforcement.
- `crates/quine-core/src/permission/request.rs`
  - Extend request metadata only as needed to represent current concrete tools.
- Colocated tests in each affected tool module
  - Add request-construction and permission-path tests next to the tool implementation.
- `crates/quine-core/tests/`
  - Add representative cross-tool integration tests for allow, deny, and ask outcomes.

## Validation Plan

- Colocated unit tests for tool request construction:
  - `read.rs` and `find.rs` build read-scoped requests with correct target paths
  - `write.rs` builds write-scoped requests for actual patch targets
  - `bash.rs` builds execute-scoped requests carrying command text
  - `spawn.rs`, `subagent.rs`, and `signal.rs` build process/agent-control requests
- Runtime parity tests:
  - tools marked `is_read_only()` continue to construct low-risk/read requests rather than mutating ones
  - mutating tools in `built_in_tool_definitions(false)` are gated by runtime permission checks in non-plan sessions
  - tools omitted in plan mode remain unavailable there and do not rely solely on evaluator behavior for safety
- Integration tests across representative categories:
  - read tool allowed under default/read-safe policy
  - write tool denied or prompted under conservative policy
  - process/agent-control tool denied or prompted unless explicitly allowed
- Daemon/harness-backed coverage:
  - at least one end-to-end session exercising `bash` and `apply_patch`/`write` permission checks through the real runtime dispatch path
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Reviewed `features/plans/045-tool-permission-integration-qa.md` and found one blocking mismatch with `.claude/commands/feature-planning.md`: the QA scenarios are still too abstract to execute without inventing missing details.
- Required QA-plan revisions before agreement:
  - replace the current scenario bullets with concrete, runnable cases that name the exact test entry points or commands
  - for the required daemon-backed coverage, include the exact command(s) to start the local daemon and connect to it
  - because this feature changes `quine-core`, include at least one multi-round daemon/chat scenario with the exact round-by-round messages to send
  - for each daemon/chat round, specify the expected assistant response text shape, permission status text, and the expected tool activity or denial text
  - make the allow / deny / ask coverage concrete by stating which tool is used in each case, how policy is configured for that case, and what observable result proves the evaluator decision propagated correctly
- Once the QA doc is revised with those concrete details, this implementation doc can re-review the latest revision and, if aligned, move to `agreed`.
