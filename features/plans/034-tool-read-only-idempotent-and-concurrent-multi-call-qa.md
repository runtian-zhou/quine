# Tool Read-Only, Idempotent, and Concurrent Multi-Call — QA Plan

Short summary: verify that tools can advertise read-only and idempotent safety flags, that only eligible multi-tool batches run concurrently, and that the daemon/CLI preserve deterministic output ordering and cancellation behavior.

## Open Questions

- Resolved with implementation: tool metadata is required for provider payloads plus internal registry/tests in the first pass, but there is no new user-facing debug/status surface requirement.
- Resolved with implementation: `wait_child` is fully excluded from first-pass concurrent batches.
- Resolved with implementation: for tools whose safety depends on arguments, trait metadata stays conservative and any narrower eligibility decision happens in the engine-level dispatch helper.
- Resolved with implementation: concurrent batches must emit externally visible `ToolResult` artifacts in request order, not completion order.

## Agreement Status

agreed — this doc matches the implementation plan on scope and behavior: provider/internal metadata only for the first pass, deterministic request-order output, a first-pass concurrent allowlist limited to `read_file` and `find`, and full first-pass exclusion of `wait_child` and other engine-managed tools.

## Test Strategy

- Validate the change at three layers:
  - `quine-llm` unit tests for tool-definition serialization of `read_only` and `idempotent`
  - `quine-core` unit/integration tests for one centralized batch-eligibility decision, actual concurrent execution, stable request-order output, and cancellation fanout
  - daemon/CLI scenarios proving a real chat turn with multiple safe tool calls behaves correctly end to end
- Prefer deterministic concurrency probes over timing-based guesses. Use barriers, channels, or shared state to prove overlap and to show both eligible tools start before either completes.
- Treat concurrency eligibility as allowlist-based in the first pass: QA will assume only built-in `read_file` and `find` are concurrent-safe unless the implementation doc is revised again.
- Treat any batch containing an unsafe, interactive, unknown, permission-sensitive, or special engine-managed tool as sequential unless the implementation explicitly documents otherwise.
- Capture ordering evidence at two stages, not only in the final assistant text: `ToolRequest` emission before execution begins, then `ToolResult`/history/message emission in original request order after completion.

## Scenarios

- **Scenario 1: Tool-definition metadata roundtrip**
  - Start/use local daemon: not required; run unit tests.
  - Exact command: `cargo test -p quine-llm`
  - Expected result: tests verify `ToolDefinition` defaults `read_only=false` and `idempotent=false`, and explicit `true` values serialize/deserialize without loss.

- **Scenario 2: Eligible batch executes concurrently in core tests**
  - Start/use local daemon: not required; run unit/integration tests.
  - Exact command: `cargo test -p quine-core concurrent_tool -- --nocapture`
  - Expected result: a test with two synthetic safe tools records that both executions begin before either completes, proving overlap; final assertions show results are stored and surfaced in the original request order.

- **Scenario 3: Mixed or special-managed batch falls back to sequential execution**
  - Start/use local daemon: not required; run unit/integration tests.
  - Exact command: `cargo test -p quine-core sequential_fallback -- --nocapture`
  - Expected result: a test batch containing one safe tool and one ineligible tool records no overlap, and assertions show the second call does not start until the first completes.
  - Required coverage: include at least one case using a plainly unsafe/mutating tool and one case using either an unknown tool or a special engine-managed tool such as `plan`, `ask_user`, or `wait_child`.

- **Scenario 4: Concurrent batch cancellation fans out to all in-flight calls**
  - Start/use local daemon: not required; run unit/integration tests.
  - Exact command: `cargo test -p quine-core concurrent_cancel -- --nocapture`
  - Expected result: after dispatching an eligible batch, cancellation causes every running tool future to observe cancellation and the session ends the turn without leaving orphaned in-flight work.

- **Scenario 5: Multi-round local daemon chat with safe concurrent calls**
  - How to start local daemon: in terminal A run `cargo run --bin quine-harness`.
  - How to connect: in terminal B run `cargo run --bin quine -- chat`.
  - Exact round-by-round messages to send:
    - Round 1 user message: `In one turn, use both read_file on CLAUDE.md and find for '*.rs' under crates/quine-core/src/tool, then tell me the first line of CLAUDE.md and how many matching files you found.`
  - Expected response and activity:
    - Status text: the UI shows one assistant turn entering streaming/tool activity.
    - Tool activity: exactly two tool requests in the same assistant turn, one for `read_file` and one for `find`.
    - Tool behavior: both calls are accepted as safe and dispatched concurrently.
    - Final response text: a concise answer containing the first line of `CLAUDE.md` and a count of matching `*.rs` files.
    - Error text: none.
  - Additional expected ordering: if the UI surfaces tool results individually, they appear associated with the original request order, even if internal completion order differs.

- **Scenario 6: Multi-round local daemon chat with unsafe mixed batch fallback**
  - How to start local daemon: in terminal A run `cargo run --bin quine-harness`.
  - How to connect: in terminal B run `cargo run --bin quine -- chat`.
  - Exact round-by-round messages to send:
    - Round 1 user message: `In one turn, read CLAUDE.md and also update a scratch file with the text hello, then summarize both actions.`
  - Expected response and activity:
    - Tool activity: the assistant may request `read_file` plus `write`, or equivalent safe/unsafe pair, in the same turn.
    - Dispatch rule: because one call is not safe, the full batch executes sequentially.
    - Final response text: confirms the file read and write outcome.
    - Error text: none if the workspace permits the write; otherwise the expected write error is reported and no concurrent scheduling occurs.

- **Scenario 7: Plan mode remains read-only and non-broadened**
  - How to start one-off flow: run `cargo run --bin quine -- chat --plan "Design a change and inspect the repo"` or the repo’s equivalent plan-mode invocation.
  - Exact prompt/message: `Inspect the codebase and explain which tools are available.`
  - Expected result:
    - Final response text: mentions only read-oriented exploration behavior.
    - Tool activity: no mutating tools such as `write` are available.
    - Error text: none.
    - Additional check: only verify metadata labels in logs/debug output if an existing surface already shows tool-definition metadata; do not require a new debug/status UI unless the implementation doc changes.

## Required Evidence

- Unit-test output showing tool-definition metadata defaulting and roundtripping.
- Core-test output proving true overlap for an eligible batch and no overlap for a mixed batch.
- Evidence that ordered `ToolRequest` and `ToolResult` handling remains deterministic.
- Evidence that cancellation reaches all in-flight calls in a concurrent batch.
- Transcript or captured log from a local daemon chat where one assistant turn issues two safe calls and returns a coherent final answer.
- Workspace validation output for:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- The implementation plan now narrows the first-pass concurrent safe set to `read_file` and `find`; QA will verify that allowlist directly and treat `wait_child` as sequential-only in the first pass.
- QA will validate that the engine uses one centralized batch-eligibility helper rather than scattered per-call checks, because consistency of the safety policy depends on that structure.
- The implementation now states deterministic external ordering as a contract: `ToolRequest` notifications in request order before execution, and `ToolResult`/history/message emission in that same request order after completion. QA will capture evidence for both request and result ordering, not only the final transcript order.
- QA will require at least one negative case with an unknown or special engine-managed tool in the batch, not just a plainly mutating tool like `write`, to prove the fallback rule is broader than read-vs-write.
- QA will require deterministic overlap evidence using barriers, channels, or shared state rather than timing-only sleeps.
- QA and implementation are now aligned that there is no first-pass requirement for a new debug/status UI exposing tool metadata; validation should focus on provider payloads, internal registry behavior, tests, and any existing logs that already surface tool definitions.
