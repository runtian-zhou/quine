# 052 Persisted Permission Rules and Sources — Implementation Plan

Short summary: Add durable user- and project-configured permission rules, explicit rule-source precedence, and support for turning certain approval decisions into persisted rules once the runtime permission engine is stable.

## Open Questions

- None. This draft stays scoped to Feature 10 from `docs/design/003-permission-system-implementation-plan.md`.

## Agreement Status

agreed — Re-reviewed `features/plans/052-persisted-permission-rules-and-sources-qa.md` after updating both docs to concrete parsing, precedence, and daemon-backed runtime scenarios. Both docs now align on trusted persisted sources, rule-source attribution, conditional approval-to-persist coverage, and there are no unresolved open questions.

## Proposed Design

- Ground persisted-rule work in the current bootstrap and config flow that already exists:
  - `SessionConfig` in `crates/quine-harness/src/config.rs` is the current trusted session-start surface
  - `LocalHarness::create_session()` in `crates/quine-harness/src/local.rs` forwards config into `CoreInput::CreateSession`
  - `SessionContext` and `PersistedSessionConfig` already carry durable session creation state in `quine-core`
  - existing session/context inspection surfaces can expose winning rule provenance once the runtime state carries it
- Add persisted rule support only after the runtime domain model and evaluator are in place, so the source-precedence contract has a stable target.
- Keep rule-source ownership split cleanly:
  - parsing and locating trusted config sources belongs in `quine-harness`
  - source-partitioned runtime storage and evaluation precedence belong in `quine-core`
  - CLI support should remain additive and minimal, especially for converting an approval decision into a persisted rule only if that UX already exists naturally from Feature 4
  - diagnostics added in Feature 051 should be able to report the winning rule source without a second provenance model
- Define a typed persisted rule format instead of an open-ended policy language:
  - support allow, deny, and ask effects
  - support the same `PermissionScope` and `PermissionTarget` vocabulary introduced in earlier slices
  - preserve explicit `PermissionRuleSource` attribution for built-in, user, project, CLI, session, and approval-derived rules
- Use existing config/bootstrap seams conservatively:
  - extend `SessionConfig` only if session creation truly needs direct rule injection from CLI or daemon callers
  - otherwise, prefer harness-side loading of user/project config and thread the parsed rules into core during session creation
  - do not create organization/fleet-managed policy loaders in this slice
  - keep the trust boundary explicit: only harness-owned trusted config locations should produce persisted rules for core bootstrap
- Preserve distinction between persisted and session-only rules:
  - session rules remain runtime-only additions in `PermissionContext`
  - persisted rules are loaded from trusted config or approval-derived persisted storage
  - evaluation outcomes and diagnostics should continue to identify the winning source explicitly
  - the same source buckets should flow through Feature 051 inspection output so operators can see whether a winning rule came from built-in defaults, project config, user config, session state, or an approval-derived persisted rule
- Keep invalid-config behavior operationally safe:
  - malformed persisted rules should produce clear diagnostics
  - failure policy should be explicit per source (reject session creation vs ignore malformed entry with warning), but must not silently reinterpret broken rules as permissive policy
  - harness-side parsing errors should remain attributable to a trusted source path so QA and operators can diagnose which file needs attention
- Keep approval-to-persist strictly additive:
  - only wire it if the current approval flow already has a natural place to capture a durable choice
  - if it does land, it must write back through the same typed persisted-rule format and provenance model as file-based rules rather than a separate side channel

## File-by-File Changes

- `crates/quine-harness/src/config.rs`
  - Define or parse trusted persisted permission-rule config structures from supported user/project sources.
  - Keep the file format typed and conservative so `serde` parsing failures can be surfaced clearly without guessing at intent.
  - Encode source-path information alongside parsed rules so later diagnostics can report where winning rules came from.
- `crates/quine-harness/src/local.rs`
  - Load parsed persisted rules during session creation and thread them into `CoreInput::CreateSession` or an equivalent internal bootstrap path.
  - Keep this bootstrap one-way: harness supplies trusted parsed rules, while core remains responsible for precedence and matching semantics.
- `crates/quine-core/src/channel.rs`
  - Add additive create-session fields only if core needs parsed rule sets explicitly at bootstrap.
  - Keep the input surface narrow so callers cannot smuggle untrusted persisted rules around harness-owned trust checks.
- `crates/quine-core/src/permission/context.rs`
  - Merge parsed persisted rules into source-partitioned runtime state while preserving provenance.
  - Keep persisted and session-only rule buckets separately inspectable so later `/context` output can distinguish them directly.
- `crates/quine-core/src/permission/engine.rs`
  - Enforce source precedence deterministically across persisted and session rules.
  - Ensure the final outcome can report the winning source in the same structured form expected by Feature 051 diagnostics.
- `crates/quine-core/src/permission/types.rs`
  - Reuse or refine typed rule/target structures so persisted config maps cleanly onto runtime types.
  - Avoid introducing a second rule representation solely for config parsing.
- `crates/quine-core/src/persistence.rs`
  - Keep durable session state compatible with any new rule-source metadata that needs to survive restore or inspection.
- `crates/quine-cli/src/session.rs`
  - Add create-session params only if CLI callers need to inject CLI-arg rules directly in this slice.
- `crates/quine-cli/src/chat.rs` and/or approval UI modules
  - If approve-and-persist is implemented here, add the minimal response path to request persistent-rule creation.
  - Reuse the existing approval UX instead of inventing a separate policy-editing flow.
- Tests across harness config parsing, core rule merging, and integration paths
  - Add parsing, precedence, invalid-config, and session-vs-persisted distinction coverage.
  - Include at least one runtime-path assertion that inspected session context reports the same winning-source attribution used by the evaluator.

## Validation Plan

- Unit tests for persisted-rule parsing in `quine-harness`:
  - user/project config allow, deny, and ask rules parse into typed structures
  - malformed entries fail with clear diagnostics
- Unit tests in `quine-core` for precedence and provenance:
  - persisted user/project rules merge into source partitions correctly
  - session-only rules remain distinguishable from persisted rules
  - conflicting sources resolve according to the documented precedence contract
- Integration tests for bootstrap and runtime behavior:
  - creating a session with persisted rules results in those rules being visible in runtime context/inspection
  - equivalent session-only rules do not masquerade as persisted sources
  - invalid config handling remains deterministic and safe
- Approval-to-persisted-rule tests, if implemented in this slice:
  - an approval choice can create a persisted rule and affect later matching requests with the correct recorded source
  - this remains conditional; if the feature lands only trusted config loading plus precedence, the QA plan should not require approval-memory persistence as a mandatory gate
- Required workspace checks for the eventual implementation PR:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- Re-reviewed `features/plans/052-persisted-permission-rules-and-sources-qa.md` after its latest revision.
- The QA plan now matches this implementation plan’s concrete interaction points with existing code:
  - trusted config parsing in `quine-harness`
  - source-partitioned rule merging and precedence in `quine-core`
  - context/inspection checks proving winning-rule provenance is preserved at runtime
  - conditional approval-to-persist coverage only if that path actually lands in this slice
- Scope remains aligned: trusted user/project config bootstrap, deterministic precedence and provenance, additive CLI behavior only where it naturally reuses existing approval flows, and no separate organization-policy system.
- No further QA-side changes are required from this review.
