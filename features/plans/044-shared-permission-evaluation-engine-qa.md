# 044 Shared Permission Evaluation Engine — QA Plan

Short summary: Verify the deterministic shared permission-evaluation engine for Quine Feature 2, including precedence ordering, `defer` semantics, source attribution, and explicit headless prompt handling.

## Open Questions

- None. This plan is scoped to Feature 2 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Reviewed the latest implementation-plan revision; scope, scenarios, required evidence, and feedback are aligned, and no unresolved questions remain.

## Test Strategy

- Focus on deterministic `quine-core` coverage first:
  - table-driven precedence tests
  - explicit `defer` cases
  - structured-outcome assertions
- Add narrow integration coverage only where existing execution paths already thread permission decisions.
- Keep QA limited to the evaluator itself, not full per-tool UX or approval routing.

## Scenarios

- **Unit — Explicit Deny Beats Allow**
  - Provide a request matching both deny and allow candidates.
  - Expect final outcome `deny` with correct matched source attribution.
- **Unit — Tool `defer` Falls Through**
  - Provide tool-local `defer` and matching rule/mode policy.
  - Expect the engine to continue evaluation and produce the rule- or mode-based result.
- **Unit — Hard Tool Denial Wins**
  - Provide tool-local `deny` and conflicting broader allow rule.
  - Expect final `deny` attributed to tool-local analysis.
- **Unit — Mode Default Applies**
  - Provide a request with no matching rules.
  - Expect deterministic mode-default behavior.
- **Unit — Headless `ask` Fails Safe**
  - Configure non-interactive prompt behavior with a request that would otherwise become `ask`.
  - Expect deterministic deny or explicit non-interactive failure outcome, per implementation contract.
- **Integration — Outcome Serialization**
  - Serialize and inspect a representative `PermissionOutcome`.
  - Expect stable explanation fields usable by later diagnostics and approval routing.

## Required Evidence

- Passing precedence and regression tests for evaluator ordering.
- Passing `defer`, tool-deny, and headless `ask` tests.
- One concrete example of a structured `PermissionOutcome` showing result and source attribution.
- Workspace validation evidence:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## Implementation Feedback

- Reviewed `features/plans/044-shared-permission-evaluation-engine-implementation.md`; the implementation plan stays correctly scoped to the shared evaluator in `quine-core` and does not pull in later approval-routing UX.
- The QA scenarios listed here are sufficient for this feature slice and match the implementation plan’s validation targets:
  - precedence ordering
  - tool-local `deny` and `defer` handling
  - structured source attribution
  - deterministic headless `ask` fallback
- The narrow `PermissionOutcome` serialization scenario is appropriate if limited to fields intentionally introduced or preserved by this slice.
- No additional QA-plan changes are needed from the implementation side; this doc can move to `agreed` once its owner confirms review of the latest implementation-plan revision.
