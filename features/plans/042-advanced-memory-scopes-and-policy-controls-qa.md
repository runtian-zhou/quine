# 042 Advanced Memory Scopes and Policy Controls — QA Plan

Short summary: Verify deterministic project/team/agent memory scope resolution, additive policy gating for read/write behavior, explicit precedence and conflict resolution, and trust/permission integration for advanced durable-memory scope handling.

## Open Questions

- None. The paired implementation and QA docs now align on additive advanced scope and policy behavior built on top of the earlier memory slices, with no unresolved scope, API, or validation questions.

## Agreement Status

agreed — this QA plan now matches the paired implementation plan’s scope boundaries, deterministic precedence rules, policy-enforcement expectations, and additive-only integration strategy. Both docs agree with no unresolved open questions.

## Test Strategy

- Verify the feature in three layers so failures are easy to localize:
  - `quine-core` unit tests for pure scope resolution, lookup order, policy authorization, and conflict-resolution helpers
  - `quine-core` or `quine-harness` integration tests for scoped runtime state, restore behavior, and on-disk write targeting
  - local-daemon QA scenarios for real prompt-time recall and live policy enforcement because this feature changes `quine-core` runtime memory behavior
- Keep QA scoped to the agreed Feature 6 slice only:
  - project, agent, and team durable-memory scopes
  - additive feature flags and read/write policy controls
  - deterministic precedence and conflict resolution
  - trust/permission enforcement and additive diagnostics evidence
- Exclude deferred work from pass criteria:
  - no broad interactive UI for conflict management
  - no distributed or remote shared-memory coordination
  - no automatic multi-scope mirroring or merge tooling
  - no shared inter-crate trait changes
- Prefer deterministic fixtures and providers:
  - fixed timestamps, stable scope keys, and seeded memory records for unit/integration tests
  - a deterministic local test provider for daemon scenarios so expected assistant text is exact and stable

## Scenarios

### Shared daemon-test setup

- Use an isolated temp workspace and state root so scope directories and durable-memory writes are easy to inspect:
  - `export QA_TMP="$(mktemp -d)"`
  - `export QA_SOCKET="$QA_TMP/harness.sock"`
  - `export XDG_STATE_HOME="$QA_TMP/xdg-state"`
  - `export QA_PROJECT_DIR="$QA_TMP/project"`
  - `mkdir -p "$QA_PROJECT_DIR" "$XDG_STATE_HOME"`
- Start the daemon in a second shell with deterministic provider settings. The implementation for this feature must keep or add a deterministic local provider/test harness so these expectations are exact and stable; if no deterministic provider exists when QA executes, block signoff until one exists.
  - `LLM_PROVIDER=openai LLM_BASE_URL=http://127.0.0.1:18080/v1 LLM_MODEL=test-memory-scopes cargo run --bin quine -- daemon start --socket "$QA_SOCKET"`
- For all one-shot checks, prefer JSON output so session IDs, final responses, and tool activity are machine-verifiable:
  - `cargo run --bin quine -- run --socket "$QA_SOCKET" --json "<message>"`
- For live-session context inspection, use the existing interactive slash command path or the harness RPC surface:
  - interactive: `cargo run --bin quine -- chat --socket "$QA_SOCKET"`
  - RPC/context inspection path: `get_session_context` via the existing harness protocol, or `/context` if the implementation exposes the needed memory fields there
- Stop the daemon after the scenario:
  - `cargo run --bin quine -- daemon stop --socket "$QA_SOCKET"`

- **Unit — Project-Only Resolution**
  - Configure persistent memory enabled with advanced scopes disabled.
  - Resolve scoped memory paths for a project working directory with no `agent_key` and no `team_key`.
  - Expect project-only behavior:
    - readable scopes: project only
    - writable scope: project only
    - lookup order: `ProjectOnly`
    - no team or agent directories selected

- **Unit — Agent Scope Resolution**
  - Configure agent memory enabled, `agent_key = "planner"`, no team key, and cross-scope recall disabled.
  - Resolve scoped memory paths.
  - Expect:
    - readable scopes contain project and agent only
    - team scope is absent
    - writable scope follows the configured default write policy
    - ordering matches configured `ScopedMemoryLookupOrder`

- **Unit — Team Scope Resolution**
  - Configure team memory enabled, `team_key = "infra"`, no agent key, and cross-scope recall disabled.
  - Resolve scoped memory paths.
  - Expect:
    - readable scopes contain project and team only
    - agent scope is absent
    - default write target remains project unless policy selects team

- **Unit — Cross-Scope Recall Gate**
  - Configure both `agent_key` and `team_key`.
  - Resolve once with cross-scope recall disabled and once with it enabled.
  - Expect:
    - disabled: only the authorized default readable subset is used
    - enabled: project, agent, and team scopes are all readable in explicit configured order
    - no implicit widening of readable scopes when the gate is off

- **Unit — Scope Write Authorization**
  - Build `MemoryPermissionContext` fixtures for trusted and untrusted workspace, explicit memory intent present and absent, and presence and absence of agent and team keys.
  - Validate project, agent, and team write authorization separately.
  - Expect:
    - project writes succeed when enabled
    - agent/team writes fail when their scope is disabled
    - agent/team writes fail when explicit intent is required but absent
    - writes fail when trusted-workspace policy is required but workspace trust is false

- **Unit — Conflict Resolution Rules**
  - Seed equivalent durable-memory records for the same fact in project, agent, and team scopes with deterministic timestamps.
  - Exercise every supported conflict rule:
    - `PreferNarrowerScope`
    - `PreferBroaderScope`
    - `PreferMostRecentlyUpdated`
    - `ErrorOnConflictingWrites`
  - Expect deterministic winning-scope or denial outcomes independent of filesystem iteration order.

- **Integration — Extraction Targets Exactly One Scope**
  - Configure project, agent, and team scopes as available but authorize writes to only one target scope.
  - Trigger a durable extraction decision.
  - Expect:
    - exactly one scope directory receives the created or updated durable-memory file
    - non-selected scopes do not receive mirrored copies automatically
    - persisted scoped-memory state records the selected writable scope snapshot if applicable

- **Integration — Denied Narrow-Scope Writes Do Not Fall Back**
  - Configure default write scope as agent or team, then deny that scope via policy while project writes remain generally enabled.
  - Trigger an extraction-worthy memory event without explicit fallback permission.
  - Expect:
    - the intended narrow-scope write is denied or skipped
    - no project-scope file is written as an implicit fallback
    - diagnostics or outcome state records the denial reason

- **Integration — Prompt Recall Precedence Across Scopes**
  - Seed overlapping durable facts into project and agent scopes, or project and team scopes, with deterministic metadata.
  - Run prompt-time relevant-memory selection under an explicit lookup order and conflict rule.
  - Expect:
    - all policy-authorized readable scopes are consulted in configured order
    - the selected reminder payload reflects the configured conflict-resolution outcome
    - lower-priority conflicting records are not rewritten or deleted as a side effect

- **Integration — Restore Rebuilds Scoped State**
  - Persist a session with scoped-memory state containing readable scopes, writable scope, and extraction boundary data.
  - Restore the session from checkpoint.
  - Expect:
    - derived runtime scope state rebuilds correctly from persisted and config inputs
    - optional new persisted fields default cleanly for older checkpoints
    - restore does not require full durable indexes or entry bodies in checkpoints

- **Daemon — Multi-Round Scoped Recall**
  - Purpose: prove that prompt-time scoped recall in `quine-core` is deterministic across more than one round, and that the runtime uses the configured readable scopes and conflict rule instead of filesystem order.
  - Required fixture setup before starting the daemon:
    - create the project root: `mkdir -p "$QA_PROJECT_DIR"`
    - seed project memory with a broad fact: `mkdir -p "$XDG_STATE_HOME/state/memory/projects/project-qa/memories"`
    - write the project fact file at `"$XDG_STATE_HOME/state/memory/projects/project-qa/entries/editor-style.md"` with content whose durable fact is exactly `Preferred editor: vim`.
    - write the agent fact file at `"$XDG_STATE_HOME/state/memory/agents/project-qa/planner/entries/editor-style.md"` with content whose durable fact is exactly `Preferred editor: helix`.
    - enable agent scope, disable team scope, enable relevant-memory recall, and set conflict resolution to `PreferNarrowerScope` in the feature’s config surface.
    - ensure the created session uses working directory `"$QA_PROJECT_DIR"` and `agent_key = "planner"`.
  - Exact commands:
    - start daemon: `LLM_PROVIDER=openai LLM_BASE_URL=http://127.0.0.1:18080/v1 LLM_MODEL=test-memory-scopes cargo run --bin quine -- daemon start --socket "$QA_SOCKET"`
    - round 1 one-shot send: `cargo run --bin quine -- run --socket "$QA_SOCKET" --json "What editor should I use for this task? Reply with exactly one sentence."`
    - save the returned `session_id` from JSON.
    - inspect context after round 1 using the saved session ID through `get_session_context` or `quine chat --socket "$QA_SOCKET"` followed by `/context`.
    - round 2 send on the same session: `cargo run --bin quine -- run --socket "$QA_SOCKET" --session "$SESSION_ID" --json "Repeat the preferred editor in exactly the same wording as before."`
    - inspect context again after round 2.
  - Exact round-by-round messages:
    - round 1 user message: `What editor should I use for this task? Reply with exactly one sentence.`
    - round 2 user message: `Repeat the preferred editor in exactly the same wording as before.`
  - Exact expected results:
    - round 1 final response text is exactly `Use helix.`
    - round 2 final response text is exactly `Use helix.`
    - round 1 and round 2 status both complete successfully with no session error notification and no interaction-needed pause.
    - round 1 and round 2 tool activity is exactly none unless the existing implementation already emits a read-only diagnostics tool for memory inspection; if such a tool exists, it must be the same in both rounds and must not mutate memory.
    - context inspection after each round shows readable scopes in deterministic order `[project, agent]`, writable scope unchanged from configured policy, and conflict winner `agent` with reason `PreferNarrowerScope`.
    - no team scope appears in the readable scope list.

- **Daemon — Live Policy Denial**
  - Purpose: prove that unauthorized narrow-scope writes are denied cleanly, do not fail the whole turn under best-effort memory semantics, and do not silently fall back to project scope.
  - Required fixture setup before starting the daemon:
    - create the project root: `mkdir -p "$QA_PROJECT_DIR"`
    - enable persistent memory, agent scope, and team scope.
    - configure default write scope as `agent`.
    - configure policy so agent writes require both explicit remember intent and trusted workspace.
    - create the session with `agent_key = "planner"`, `team_key = "infra"`, and a runtime permission context that is untrusted or otherwise fails the configured write requirement.
  - Exact commands:
    - start daemon: `LLM_PROVIDER=openai LLM_BASE_URL=http://127.0.0.1:18080/v1 LLM_MODEL=test-memory-scopes cargo run --bin quine -- daemon start --socket "$QA_SOCKET"`
    - send the write-triggering message: `cargo run --bin quine -- run --socket "$QA_SOCKET" --json "Remember that my deployment region is us-west-2."`
    - save the returned `session_id` from JSON.
    - inspect session context with the saved session ID through `get_session_context` or `/context`.
    - inspect the scope directories on disk:
      - `find "$XDG_STATE_HOME/state/memory" -maxdepth 8 -type f | sort`
  - Exact message:
    - user message: `Remember that my deployment region is us-west-2.`
  - Exact expected results:
    - final response text is exactly `I will keep that in mind.`
    - turn status is complete successfully with no session error notification and no interaction-needed pause.
    - tool activity is exactly none unless the existing memory pipeline already surfaces a reviewed diagnostics tool; any emitted tool activity must be read-only relative to memory files for this denied path.
    - there is no new file under `"$XDG_STATE_HOME/state/memory/agents/project-qa/planner/"` for the denied fact.
    - there is no new file under `"$XDG_STATE_HOME/state/memory/projects/project-qa/"` for the denied fact.
    - there is no implicit fallback write to team scope either unless the explicit policy for this scenario allows it; by default for this scenario it must not.
    - context inspection reports writable scope `agent`, effective write outcome `denied`, and denial reason indicating the missing authorization condition such as `explicit_intent_required` or `trusted_workspace_required`.

## Required Evidence

- Unit-test evidence for:
  - project-only, project-plus-agent, project-plus-team, and cross-scope-enabled resolution
  - read/write authorization by scope with trusted/untrusted and explicit-intent variations
  - deterministic conflict outcomes for every supported `MemoryConflictResolution` mode
- Integration-test evidence for:
  - scoped extraction writing to exactly one scope
  - denied writes not falling back silently
  - prompt-time recall selecting the configured winning scope across overlapping records
  - restore compatibility for new optional persisted scope fields
- Daemon evidence for at least two live-session flows:
  - one positive scoped-recall scenario
  - one negative policy-denial scenario
- Inspection evidence, preferably via existing `get_session_context` or `/context`, showing:
  - resolved readable scopes in order
  - resolved writable scope or no-write state
  - conflict winner or denial reason when applicable
- On-disk evidence for write-target correctness:
  - expected file present only in the authorized target scope directory
  - no mirrored copies in non-selected scopes

## Implementation Feedback

- Reviewed against `features/plans/042-advanced-memory-scopes-and-policy-controls-implementation.md`, the feature request, and the Feature 6 section of `docs/design/002-memory-systems-design.md`; the implementation plan is correctly scoped to additive advanced scope handling on top of earlier memory slices rather than a new memory subsystem.
- QA coverage stays explicitly bounded to the agreed scope:
  - advanced project/team/agent durable-memory scope resolution
  - additive read/write policy enforcement and trust gating
  - deterministic lookup order and conflict resolution
  - additive diagnostics/inspection evidence through existing surfaces if Feature 041 diagnostics exist
  - no broad new UI workflows, no new memory-management command surface, and no shared inter-crate trait changes
- The plan includes concrete positive and negative authorization scenarios, not just happy-path precedence checks.
- It requires at least one deterministic overlap/conflict scenario where the same durable fact exists in more than one scope and names both the configured policy and expected winning or denial outcome.
- Because this feature changes `quine-core` prompt-time memory behavior, the plan includes concrete daemon-backed multi-round scoped recall and live policy-denial scenarios.
- The plan requires machine-verifiable evidence where possible through session-context inspection and on-disk write-target checks.

