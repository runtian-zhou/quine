# 050 Headless and Background Permission Behavior — QA Plan

Short summary: Verify Quine Feature 8 non-interactive permission behavior for headless, scheduled, and background sessions, ensuring unresolved prompts fail safely with explicit operator-visible outcomes.

## Open Questions

- None. This plan stays scoped to Feature 8 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/050-headless-and-background-permission-behavior-implementation.md` revision and confirmed the paired docs now match on explicit prompt-behavior modeling, deterministic non-interactive failure semantics, exact daemon/CLI validation, and cleared pending-approval expectations. Both docs are aligned and have no unresolved open questions.

## Test Strategy

- Focus on integration and daemon-level behavior because this slice is primarily orchestration semantics in `quine-core`, `quine-harness`, and CLI session startup paths.
- Require both targeted automated tests and at least one real local-daemon execution path.
- Use exact commands that another agent can run from the workspace root without inventing missing setup.
- Treat permission-denied / prompt-unavailable outcomes as the success case for headless and background flows when the request would otherwise resolve to `ask`.
- Keep pass criteria limited to fail-safe non-interactive semantics; remote responders and new approval UIs remain out of scope.

## Scenarios

- **Automated integration — session bootstrap modes**
  - **Command**: `cargo test -p quine-core permission:: -- --nocapture`
  - **Expected added coverage**: targeted tests that prove session/bootstrap inputs initialize prompt behavior as interactive for `quine chat`, non-interactive for `quine run`, and scheduled/background for scheduler-created sessions.
  - **Expected result**: the test binary exits successfully; test names and assertions explicitly verify the runtime `PermissionContext` (or equivalent internal permission state) records the intended prompt behavior for each startup path.
  - **Failure signal**: any assertion that a headless or scheduled path still uses interactive prompting semantics, or any missing dedicated startup-path assertion, blocks sign-off.

- **Automated integration — headless `ask` resolves fail-safe**
  - **Command**: `cargo test -p quine-core permission:: -- --nocapture`
  - **Trigger under test**: an evaluation case whose local/tool decision would be `ask` in an interactive session.
  - **Expected result**: the targeted test proves the same request becomes an explicit deny or explicit non-interactive failure in headless mode, while the interactive control test remains eligible for approval routing.
  - **Expected observable assertions**: no branch upgrades `ask` to `allow`; the resulting error/outcome is permission-specific rather than a generic transport/runtime failure.
  - **Failure signal**: a passing path that allows the action without a responder, or a result that is indistinguishable from an unrelated infrastructure error.

- **Automated integration — background / scheduled session fails safe and clears pending approval**
  - **Command**: `cargo test -p quine-harness scheduler -- --nocapture`
  - **Trigger under test**: a scheduler or background-run path that creates a child session and hits a permission decision that would otherwise require prompting.
  - **Expected result**: the targeted test proves the session is created with scheduled/background prompt behavior, the action is denied or fails explicitly, and the session state does not retain a lingering `pending_approval` after completion.
  - **Expected observable assertions**: scheduler/background session creation path is exercised directly; the final state shows no unresolved approval request ID and the run terminates deterministically.
  - **Failure signal**: any pending approval left behind after the background run finishes, or any hang/wait-for-input behavior.

- **Real daemon scenario — one-shot run with no responder attached**
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-050.sock`
  - **Scenario command**: `cargo run --bin quine -- run --json --socket /tmp/quine-feature-050.sock "Use a tool action that requires approval in default mode; if permission is denied because no prompt is available, report that denial explicitly and stop."`
  - **Expected setup**: no `quine respond` command is issued for the session created by `run`; this intentionally leaves the headless session without an approval responder.
  - **Expected result from `run`**: JSON output shows a completed request whose final assistant text explicitly reports a permission denial or non-interactive approval failure; it must not claim the action ran successfully.
  - **Expected status/tool activity**: the run may show attempted tool activity up to the permission gate, but it must not show a successful execution of the gated tool; the surfaced status/error text must identify the permission problem rather than a socket, daemon, or serialization failure.
  - **Follow-up command**: `cargo run --bin quine -- ps --json --socket /tmp/quine-feature-050.sock`
  - **Expected follow-up result**: the session appears completed or failed, but not blocked waiting indefinitely for approval; if session metadata exposes pending approval state, it is absent/cleared.
  - **Cleanup command**: `cargo run --bin quine -- daemon stop --socket /tmp/quine-feature-050.sock`

- **Real daemon multi-round scenario — interactive chat still permits approval routing**
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-050.sock`
  - **Chat command**: `cargo run --bin quine -- chat --socket /tmp/quine-feature-050.sock`
  - **Round 1 user message**: `Attempt the same approval-requiring tool action, and if you need permission ask for it instead of failing immediately.`
  - **Expected round 1 result**: the session remains interactive and surfaces an approval interaction instead of immediately converting the request into a headless denial.
  - **Approval response command**: `cargo run --bin quine -- respond --socket /tmp/quine-feature-050.sock --session <SESSION_ID> "deny"`
  - **Expected round 2 result**: the final assistant response explicitly reports the permission denial after the operator response; it must remain distinguishable from the one-shot headless path because the system successfully routed an interaction rather than failing due to prompt unavailability.
  - **Expected status/tool activity**: tool execution remains blocked after the deny response, and no misleading success text appears.
  - **Cleanup command**: `cargo run --bin quine -- daemon stop --socket /tmp/quine-feature-050.sock`

- **CLI / operator messaging regression check**
  - **Commands**: run the daemon-backed one-shot scenario above once with plain-text output and once with `--json`.
  - **Expected plain-text result**: the terminal output explicitly says the action was denied or could not be approved in a non-interactive session.
  - **Expected JSON result**: the structured output includes an error/result field whose wording identifies permission denial or prompt unavailability, not a generic runtime failure.
  - **Failure signal**: wording that implies the tool succeeded, or wording that collapses permission denial into an unrelated daemon/transport error category.

## Required Evidence

- Output from `cargo test -p quine-core permission:: -- --nocapture` showing targeted startup-mode and fail-safe `ask` coverage.
- Output from `cargo test -p quine-harness scheduler -- --nocapture` showing scheduled/background deterministic denial and cleared pending-approval state.
- Transcript or captured stdout/stderr for the real daemon one-shot scenario, including:
  - daemon start command
  - one-shot `quine run` command
  - final denial / non-interactive-failure output
  - `quine ps --json` output proving the session is not stuck waiting on approval
- Transcript or captured stdout/stderr for the daemon-backed interactive chat control scenario, including:
  - the exact first user message
  - the approval response command with session ID
  - the final post-deny assistant response
- Plain-text and JSON examples demonstrating operator-visible permission-specific messaging.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Reviewed the implementation plan latest revision.
- Scope alignment is good: explicit prompt-behavior modeling, deterministic handling for would-be `ask` outcomes, interactive vs. headless differentiation, and no remote-responder work in this slice.
- This QA plan now supplies the concrete execution details previously requested: exact daemon commands, exact CLI/chat prompts, expected operator-visible results, and explicit evidence that background runs do not leave pending approvals behind.
- No additional design changes are requested from QA at this time.
