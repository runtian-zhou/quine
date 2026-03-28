# Spawn and Subagent QA Smoke Coverage — Implementation Plan

Feature request: `features/032-spawn-subagent-qa-smoke.md`

Summary: Add focused validation for the existing `spawn` and `subagent` tools so the project has a clear, low-risk QA entry point for child-agent orchestration behavior.

## Open Questions

- Should the lightweight QA artifact live alongside existing QA docs under `qa/`, or should it be embedded directly in the feature file as executable-style scenarios?
- Is there already enough harness-level test support to cover `spawn`, or should implementation focus first on unit tests in `quine-core`?

## Agreement Status

Status: agreed

Implementation review confirms the QA plan is compatible with the proposed implementation scope. Both plans agree to keep this feature limited to `quine-core` tool coverage plus a lightweight QA artifact, without changing crate-boundary traits or orchestration design.

## Proposed Design

- Add `quine-core` unit tests for `SpawnTool` covering both successful dispatch through `core_input` and the missing-channel error path.
- Reuse existing subagent tests as the baseline for synchronous delegation behavior, extending them only if a narrow gap is discovered.
- Add a compact QA note or documented scenario describing how to verify `spawn` versus `subagent` from the user perspective without changing architecture.
- Keep all changes crate-local and avoid modifying orchestration trait contracts.

## File-by-File Changes

- `crates/quine-core/src/tool/spawn.rs`: add unit tests for success and failure behavior.
- `crates/quine-core/src/tool/subagent.rs`: extend tests only if current coverage misses an acceptance criterion.
- `qa/` or adjacent documented QA location: add a short smoke scenario for distinguishing `spawn` and `subagent` behavior.

## Validation Plan

- Run targeted tests for `quine-core` tool coverage first.
- Run full required workspace checks from `CLAUDE.md` once the focused changes are complete.

## QA Feedback

QA plan reviewed and accepted. The requested evidence matches the implementation scope: focused `SpawnTool` unit tests, confirmation that existing `subagent` coverage remains sufficient or receives only narrow additions, and a short QA scenario documenting the behavioral distinction between immediate inline delegation and child-session spawning.
