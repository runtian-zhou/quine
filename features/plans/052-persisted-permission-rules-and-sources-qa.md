# 052 Persisted Permission Rules and Sources — QA Plan

Short summary: Verify Quine Feature 10 persisted permission rules and trusted rule sources, including config parsing, explicit source precedence, invalid-config handling, and separation of session-only versus durable rules.

## Open Questions

- None. This plan stays scoped to Feature 10 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest `features/plans/052-persisted-permission-rules-and-sources-implementation.md` revision and aligned this QA plan to its trusted-config bootstrap and runtime precedence model. Both docs now describe the same concrete scenarios and have no unresolved open questions.

## Test Strategy

- Test rule parsing and precedence first in focused unit tests.
- Add integration coverage for runtime merging of persisted and session-only rules through existing bootstrap and inspection surfaces.
- Include at least one concrete daemon-backed runtime scenario because this feature changes `quine-core` permission evaluation behavior.
- Keep approval-to-persisted-rule QA explicitly conditional on that behavior landing in this slice.

## Scenarios

- **Unit — User and Project Config Parsing**
  - **Command**: `cargo test -p quine-harness config -- --nocapture`
  - **Expected added coverage**: parsing tests in `crates/quine-harness/src/config.rs` for representative trusted config documents containing allow, deny, and ask rules sourced from user and project config locations.
  - **Representative fixtures**:
    - a user config rule allowing a `Read` scope for a known tool or path prefix
    - a project config rule denying a `Write` or `Execute` scope target
    - a project config rule that yields `Ask`
  - **Expected result**: parsed rules become typed structures with preserved `PermissionRuleSource` attribution, and the test output passes without requiring the evaluator to reinterpret raw config text.

- **Unit — Source Precedence Is Deterministic**
  - **Command**: `cargo test -p quine-core permission:: -- --nocapture`
  - **Expected added coverage**: rule-merging/evaluator tests that load conflicting rules from multiple sources such as built-in, user config, project config, and session runtime additions.
  - **Representative setup**:
    - a project rule denies a target
    - a user rule asks for the same target
    - a session rule allows the same target if session rules are intended to override persisted rules in this slice
  - **Expected result**: the final decision matches the documented precedence contract exactly, and the resulting permission outcome records the winning rule source explicitly.

- **Unit — Invalid Config Handling Is Safe and Actionable**
  - **Command**: `cargo test -p quine-harness config -- --nocapture`
  - **Expected added coverage**: malformed persisted-rule fixtures in harness config parsing tests.
  - **Representative fixtures**:
    - unknown scope name
    - malformed target shape
    - partially valid config file containing both valid and invalid rules
  - **Expected result**: the parser emits clear diagnostics and follows the implementation’s chosen safe failure policy; invalid config never becomes an implicit allow and never silently changes rule meaning.

- **Integration — Session-only and Persisted Rules Stay Distinguishable**
  - **Command**: `cargo test -p quine-harness get_session_context -- --nocapture`
  - **Expected added coverage**: a harness integration test that boots a session with persisted rules loaded from trusted config and then adds a session-only rule through runtime mutation.
  - **Expected result**: inspected runtime state shows persisted and session-only rules as distinct sources; the session rule does not masquerade as a durable persisted rule, and persisted rule provenance remains visible through the same context snapshot.

- **Real daemon multi-round scenario — persisted project rule affects runtime behavior and inspection**
  - **Setup**:
    - create a temporary project config file in the repository or a temporary test workspace using the format chosen by implementation, containing a persisted rule that denies a known mutating action such as `bash` write behavior or `apply_patch` to a specific path prefix
    - ensure the daemon is started from that workspace so the harness loads the project config
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-052.sock`
  - **Chat command**: `cargo run --bin quine -- chat --socket /tmp/quine-feature-052.sock`
  - **Round 1 user message**: `/context`
  - **Expected round 1 result**: the rendered context shows persisted permission rules or rule-source summaries indicating at least one project-config rule is active.
  - **Round 2 user message**: `Attempt the action covered by the persisted project rule.`
  - **Expected round 2 result**: the action is denied or routed according to the persisted rule effect; the assistant must not report a successful tool execution when the persisted rule should block it.
  - **Round 3 user message**: `/context`
  - **Expected round 3 result**: the latest permission decision summary attributes the outcome to the persisted rule source, and the rule source is reported as project or user config rather than as a transient session rule.
  - **Cleanup command**: `cargo run --bin quine -- daemon stop --socket /tmp/quine-feature-052.sock`

- **Conditional integration — persist approval choice into a durable rule**
  - **Run only if implementation adds approval-derived persistence in this slice.**
  - **Daemon start command**: `cargo run --bin quine -- daemon start --socket /tmp/quine-feature-052.sock`
  - **Chat command**: `cargo run --bin quine -- chat --socket /tmp/quine-feature-052.sock`
  - **Round 1 user message**: `Attempt an action that requires approval and offer to remember the decision if supported.`
  - **Expected round 1 result**: the session presents an approval interaction with an option equivalent to remember-for-project or another persisted approval path, if and only if that UX was implemented.
  - **Approval response command**: `cargo run --bin quine -- respond --socket /tmp/quine-feature-052.sock --session <SESSION_ID> "approve_and_remember_project"`
  - **Round 2 user message**: `Attempt the same action again.`
  - **Expected round 2 result**: the second request follows the newly persisted rule without requiring a second prompt, and inspection attributes the winning source to the approval-derived persisted source configured by implementation.
  - **If not implemented**: record that this scenario is intentionally skipped because approval-derived persistence was explicitly left out of scope for this slice.

## Required Evidence

- Output from `cargo test -p quine-harness config -- --nocapture` showing trusted config parsing and invalid-config handling.
- Output from `cargo test -p quine-core permission:: -- --nocapture` showing deterministic source precedence and preserved winning-source attribution.
- Output from `cargo test -p quine-harness get_session_context -- --nocapture` showing persisted and session-only rules remain distinguishable in inspected runtime state.
- A daemon-backed transcript or captured output showing:
  - persisted rule presence in `/context`
  - the blocked or routed action governed by that rule
  - follow-up `/context` output proving the winning rule source is surfaced correctly
- If approval-derived persistence lands, a transcript or automated test showing the remembered decision affects a later matching request; otherwise explicit evidence that this branch was skipped as out of scope.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Re-reviewed the latest `features/plans/052-persisted-permission-rules-and-sources-implementation.md` before updating this QA plan.
- The implementation plan and this QA plan are aligned on trusted user/project config loading in `quine-harness`, source-partitioned runtime state and precedence in `quine-core`, and additive inspection support through existing context flows.
- This QA revision adds the concrete executable detail previously missing: exact test commands, exact daemon-backed runtime validation, explicit expected provenance outputs, and an explicitly conditional approval-to-persist branch.
- No further implementation-plan changes are requested from QA at this revision.
