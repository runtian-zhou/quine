# Tool Read-Only, Idempotent, and Concurrent Multi-Call — Implementation Plan

Short summary: add tool-level metadata for read-only and idempotent behavior, surface those semantics to the model/tool registry, and allow a single assistant turn with multiple tool calls to execute safe calls concurrently when every requested tool is marked eligible.

## Open Questions

- Resolved with QA: concurrent multi-tool execution should preserve the original request order in conversation history, emitted events, and UI presentation even when completion order differs.
- Resolved with QA: batch concurrency should require every call in the batch to be eligible under a conservative policy, meaning read-only and idempotent at the tool level plus any engine-level exclusions for interactive or special-managed tools.
- Resolved with QA: tools whose safety depends on arguments should keep trait metadata conservative and rely on dispatch-time gating in `engine.rs`.
- Resolved with QA: no new user-facing debug or status surface is required in the first pass. The metadata must be propagated through provider payloads plus internal registry/tests, and any existing logging may include it opportunistically without creating a new UX contract.
- Resolved with QA: `wait_child` stays fully excluded from concurrent batches in the first implementation, even for argument combinations that might appear observationally safe.

## Agreement Status

agreed — this doc reflects the same plan as the QA doc: provider/internal metadata only for the first pass, deterministic request-order output, a first-pass concurrent allowlist limited to `read_file` and `find`, and full first-pass exclusion of `wait_child` and other engine-managed tools.

## Proposed Design

- Extend the `quine-core` `Tool` trait with crate-owned metadata methods for `is_read_only()` and `is_idempotent()`, both defaulting to `false` so existing tools remain sequential unless explicitly reviewed.
- Add matching fields to the tool definition payload passed to `quine-llm` so providers can expose these semantics to models that support tool annotations, while preserving backward compatibility for providers that ignore unknown fields or unsupported annotations.
- Keep the first-pass built-in safe set explicit and narrow: `read_file` and `find`. Treat `wait_child` as fully excluded from concurrent batches in the first implementation.
- Add a single crate-private batch-eligibility helper in `crates/quine-core/src/engine.rs` that evaluates the full tool-call batch from one assistant turn. The helper should return eligible only when every call targets a registered tool whose trait metadata is read-only and idempotent, and no call is interactive, unknown, permission-sensitive, or special engine-managed.
- Dispatch eligible batches concurrently with `tokio` task coordination, but emit any externally visible ordering-sensitive artifacts deterministically: `ToolRequest` notifications first in request order, then `ToolResult` history entries, output events, and follow-up `Message::tool_result(...)` values in that same request order.
- Preserve existing sequential execution for any mixed or ineligible batch, including any batch containing `plan`, `ask_user`, `bash`, `spawn`, `signal`, `send_message`, `subagent`, `wait_child`, or unknown tools.
- Keep permission checks conservative and centralized. Concurrency should reuse existing permission and cancellation flow rather than introducing a bypass path.

## Implementation Steps

- Add conservative tool metadata defaults in `quine-core` and propagate them into `quine-llm::ToolDefinition`.
- Mark the first-pass safe built-in tools explicitly and leave all other tools on the sequential default unless separately justified.
- Implement one engine-level helper that decides whether an entire assistant-turn batch is concurrency-eligible.
- Add a concurrent dispatch path for eligible batches while preserving deterministic request-order event and history emission.
- Cover metadata export, overlap, fallback, cancellation, and ordered output with focused tests before running workspace validation.

## File-by-File Changes

- `crates/quine-core/src/tool/mod.rs`
  - Add `Tool` trait metadata methods: `is_read_only()` and `is_idempotent()`.
  - Extend tool-definition generation to include the new flags in `quine_llm::ToolDefinition` once that struct is updated.
  - Add focused unit coverage for registry/tool-definition serialization of the new metadata.
- `crates/quine-llm/src/types.rs`
  - Extend `ToolDefinition` with serializable `read_only` and `idempotent` booleans.
  - Preserve serde defaults so older persisted or provider-side payload expectations stay compatible.
- `crates/quine-llm/src/anthropic.rs`
  - Review outbound tool-definition mapping and include the new flags only if the provider wire format supports them; otherwise safely ignore them without changing request validity.
- `crates/quine-llm/src/openai_compat.rs`
  - Mirror the same provider adaptation decision as above.
- `crates/quine-core/src/tool/read.rs`
  - Mark the tool read-only and idempotent.
- `crates/quine-core/src/tool/find.rs`
  - Mark the tool read-only and idempotent.
- `crates/quine-core/src/tool/wait_child.rs`
  - Keep the tool sequential-only in the first pass; do not add concurrent eligibility.
- `crates/quine-core/src/tool/ask_user.rs`
  - Leave non-concurrent and non-idempotent.
- `crates/quine-core/src/tool/plan.rs`
  - Leave non-read-only because it mutates shared plan state.
- `crates/quine-core/src/engine.rs`
  - Add a batch-eligibility helper for concurrent tool calls.
  - Add a concurrent execution path for eligible batches using cloned execution context pieces and per-call cancellation channels.
  - Ensure `ToolRequest` events can still be emitted before execution starts and `ToolResult` events are emitted deterministically in input order.
  - Keep cancellation semantics defined for concurrent batches: a session cancel should signal all in-flight calls in the batch.
  - Keep plan integration and special-case tool handling on the sequential path.
  - Add unit/integration tests for sequential fallback and successful concurrent execution.
- `crates/quine-cli/src/tui/app.rs`
  - Review only if current UI assumptions break when multiple tool requests begin before any result arrives. Prefer no behavior change, but document if ordering-sensitive adjustments are required.
- `CLAUDE.md`
  - Update the tool-pattern description so the bootstrapping contract records the new tool metadata expectations and the concurrency rule for safe multi-tool batches.

## Validation Plan

- Add `quine-core` unit tests for tool metadata defaults and tool-definition export.
- Add engine tests covering:
  - multiple eligible tool calls execute concurrently and both results are preserved in original order
  - mixed safe/unsafe tool batches fall back to sequential execution
  - concurrent-batch cancel propagates to all running calls
  - special tools such as `plan` still run sequentially
- Add `quine-llm` serialization tests confirming the new tool-definition fields round-trip cleanly and default correctly.
- Run targeted checks first:
  - `cargo test -p quine-core engine::tests`
  - `cargo test -p quine-core tool::tests`
  - `cargo test -p quine-llm`
- Then run workspace validation:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- The narrowed first-pass safe set is clear and testable: QA will treat only `read_file` and `find` as concurrency-eligible built-ins unless this doc is revised again. Please keep the implementation and tests explicit about that allowlist so future additions do not silently inherit concurrency.
- The batch-policy helper in `crates/quine-core/src/engine.rs` is the right shape. QA will look for one centralized eligibility decision covering trait metadata plus engine-managed exclusions, rather than duplicated checks in multiple dispatch branches.
- Deterministic external ordering is now a firm contract. QA will require evidence that `ToolRequest` notifications are emitted in request order before execution begins and that `ToolResult` events, history entries, and follow-up tool-result messages are also surfaced in that same request order even when completion order differs.
- Please include a negative test case that is broader than read-vs-write, such as an unknown tool or a special engine-managed tool (`plan`, `ask_user`, `spawn`, `signal`, `send_message`, `subagent`, or `wait_child`) appearing in the batch. QA needs that to verify the fallback rule is truly policy-based and not only mutability-based.
- For concurrency proof, QA will expect at least one deterministic overlap test using a barrier, channel, or shared state so both eligible tool calls are known to have started before either completes. Timing-only sleeps are not sufficient evidence.
- For cancellation, QA will require a test showing that canceling an eligible concurrent batch reaches every in-flight call and leaves no orphaned work running after the turn ends.
- QA and implementation are aligned that no new user-facing debug/status surface is required in the first pass and that `wait_child` remains sequential-only.
