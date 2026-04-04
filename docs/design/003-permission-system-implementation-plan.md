# Permission System Implementation Plan

## Purpose

This document translates the findings in `permission-design-investigation.md` into a concrete implementation plan for Quine.

The investigation describes a mature permission system with distributed policy evaluation, session-scoped runtime state, tool-local checks, approval routing, sandbox integration, and mode-aware behavior such as plan and auto execution. Quine does not need to copy that implementation literally, but it should adopt the same core ideas in a Rust-native form that matches this workspace’s crate boundaries and trait-first architecture.

This plan splits the work into multiple self-contained features that can land incrementally without changing the core inter-crate trait contracts in destabilizing ways.

## Scope

This design doc covers:

- the target permission architecture for Quine
- the key runtime concepts Quine should implement
- a feature-by-feature rollout plan
- suggested crate and module boundaries
- sequencing and dependencies between features
- validation guidance for each feature
- explicit non-goals for the first permission rollout

This doc does not define the exact UI copy for every prompt, nor does it require Quine to reproduce every policy surface from the investigated system in the first implementation pass.

## Design Goals

- Treat permissions as a core orchestration system, not only a CLI prompt.
- Keep the source of truth in `quine-core` runtime state.
- Make decisions deterministic and testable.
- Support both interactive and headless execution.
- Integrate sandbox constraints into the same permission model.
- Keep tool-specific semantics local to the tool while centralizing policy evaluation.
- Preserve room for future remote-control, background-agent, and multi-session approval flows.
- Avoid cross-crate trait churn unless a later slice proves it is necessary.

## Non-goals

- Reproducing every advanced policy source from the investigation on day one.
- Implementing organization-managed policy distribution in the first pass.
- Building a full permission-management TUI before core semantics exist.
- Adding network policy classifiers or external reputation systems.
- Solving all future swarm or remote-operator approval workflows in the initial rollout.

## Core Design Principles

### 1. Runtime permission context is authoritative

Quine should keep a session-scoped `PermissionContext` in `quine-core` as the authoritative runtime object for the current session. Settings, CLI flags, and session commands populate this context, but evaluation reads from the runtime context rather than re-reading configuration ad hoc.

### 2. Shared engine, tool-local semantics

Each tool should remain responsible for understanding its own inputs and risk boundaries. For example, `bash` knows command strings, `apply_patch` knows file modifications, and `find`/`read_file` know read-only filesystem access. However, those tool-local checks should feed a shared permission engine rather than each tool inventing its own full policy system.

### 3. Decisions are richer than allow/deny

The permission engine should model at least:

- `allow`
- `deny`
- `ask`
- `defer`

`defer` is the Quine equivalent of the investigation doc’s `passthrough`: a tool-specific checker can decline to make the final decision and allow the shared engine to continue evaluation.

### 4. Mode transitions are policy transitions

Permission modes should be treated as policy state transitions with side effects, not as a plain enum switch. Entering plan mode, exiting plan mode, and entering any future auto-execution mode should be implemented through explicit transition helpers that preserve invariants.

### 5. Headless flows must fail safe

If a session cannot prompt a user, `ask` cannot silently behave like `allow`. Headless, scheduled, background, and remote-controlled sessions should either route approvals to an approved responder or convert unresolved prompts into explicit denials.

## Target Architecture

## Core concepts

Quine should add a dedicated permission subsystem in `quine-core` built around the following internal concepts:

- `PermissionMode`
- `PermissionDecision`
- `PermissionRule`
- `PermissionRuleEffect`
- `PermissionRuleSource`
- `PermissionContext`
- `PermissionRequest`
- `PermissionOutcome`
- `PermissionPromptBehavior`
- `ModeTransitionResult`

These names are illustrative, not mandatory, but the design should preserve the separation between persisted/static policy, runtime session state, tool-local input analysis, and prompt resolution.

## Core data structures and APIs

The following Rust-facing model is the recommended starting contract for the permission subsystem. These definitions are intentionally internal-first: they should live primarily inside `quine-core`, stay crate-private where possible, and only be re-exposed through narrower harness or CLI-facing types when needed.

### Core enums and identifiers

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
    Defer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionRuleEffect {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionRuleSource {
    BuiltIn,
    UserConfig,
    ProjectConfig,
    CliArg,
    Session,
    ApprovalMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PermissionScope {
    Read,
    Write,
    Execute,
    ProcessControl,
    AgentControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalRequestId(pub String);
```

### Rule matching and rule storage

Rules should be explicit about both what they target and how they match. The first release should avoid a fully generic policy language and instead support typed matchers that fit Quine’s actual tools.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionTarget {
    ToolName(String),
    ToolAndAction {
        tool_name: String,
        action: Option<String>,
    },
    PathPrefix(PathBuf),
    CommandPrefix(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub source: PermissionRuleSource,
    pub effect: PermissionRuleEffect,
    pub scope: PermissionScope,
    pub target: PermissionTarget,
    pub reason: Option<String>,
    pub persistent: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PermissionRuleSet {
    pub built_in: Vec<PermissionRule>,
    pub user_config: Vec<PermissionRule>,
    pub project_config: Vec<PermissionRule>,
    pub cli_args: Vec<PermissionRule>,
    pub session: Vec<PermissionRule>,
    pub approval_memory: Vec<PermissionRule>,
}
```

`PermissionRuleSet` keeps rule source partitioning explicit so precedence remains testable and diagnostics can explain where a decision came from.

### Session runtime context

`PermissionContext` is the runtime source of truth attached to a live session.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PromptBehavior {
    Interactive,
    DenyIfUnattended,
    RequireExternalApprover,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SandboxPolicySnapshot {
    pub enabled: bool,
    pub writable_roots: Vec<PathBuf>,
    pub readable_roots: Vec<PathBuf>,
    pub allows_network: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionContext {
    pub mode: PermissionMode,
    pub pre_plan_mode: Option<PermissionMode>,
    pub prompt_behavior: PromptBehavior,
    pub workspace_root: PathBuf,
    pub additional_roots: Vec<PathBuf>,
    pub rules: PermissionRuleSet,
    pub sandbox: SandboxPolicySnapshot,
    pub pending_approval: Option<ApprovalRequestId>,
}
```

This structure should be small enough to checkpoint and restore without embedding transient tool inputs or full prompt payloads.

### Tool-facing request model

Tools should not directly decide final permission outcomes except for hard local denials. Instead, they build a structured request and optionally contribute an initial local decision.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionResource {
    None,
    Path(PathBuf),
    Paths(Vec<PathBuf>),
    Command { program: String, argv: Vec<String> },
    Process { target: String },
    Agent { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: Option<String>,
    pub scope: PermissionScope,
    pub resource: PermissionResource,
    pub local_decision: PermissionDecision,
    pub rationale: Option<String>,
}
```

Expected usage:

- read-only tools often emit `Defer` plus a read-scoped resource
- tools with obvious hard safety boundaries may emit `Deny`
- tools with strong local confidence may emit `Allow` or `Ask`
- the shared engine still computes the final outcome

### Evaluation outputs

The shared engine should return both a user-visible result and enough internal detail for diagnostics, replay, and testing.

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionOutcomeKind {
    Allowed,
    Denied,
    RequiresApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermissionOutcomeSource {
    ToolLocal,
    Rule(PermissionRuleSource),
    ModeDefault(PermissionMode),
    HeadlessPolicy,
    Sandbox,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionOutcome {
    pub kind: PermissionOutcomeKind,
    pub source: PermissionOutcomeSource,
    pub reason: String,
    pub matched_rule: Option<PermissionRule>,
    pub approval_request: Option<ApprovalRequest>,
}
```

### Approval lifecycle model

Approval requests should be explicit, resumable units that the harness can surface through local CLI or future remote responders.

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ApprovalAction {
    ApproveOnce,
    DenyOnce,
    ApproveAndRememberSession,
    ApproveAndRememberProject,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: ApprovalRequestId,
    pub session_id: String,
    pub request: PermissionRequest,
    pub message: String,
    pub suggested_actions: Vec<ApprovalAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub request_id: ApprovalRequestId,
    pub action: ApprovalAction,
}
```

The first implementation may choose to support only `ApproveOnce`, `DenyOnce`, and `ApproveAndRememberSession`; the type should still reserve space for a project-persisted path.

### Mode transition model

Mode transitions should be represented by explicit helpers that can explain state changes.

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModeTransitionResult {
    pub previous_mode: PermissionMode,
    pub new_mode: PermissionMode,
    pub restored_mode: Option<PermissionMode>,
    pub notes: Vec<String>,
}
```

## Internal APIs

The permission system should expose small, focused internal interfaces instead of a single god-object API.

### `quine-core` evaluation API

```rust
pub trait PermissionEvaluator {
    fn evaluate(
        &self,
        context: &PermissionContext,
        request: &PermissionRequest,
    ) -> PermissionOutcome;
}

pub trait PermissionContextMutator {
    fn add_rule(&mut self, rule: PermissionRule);
    fn add_additional_root(&mut self, path: PathBuf);
    fn transition_mode(
        &mut self,
        next_mode: PermissionMode,
    ) -> anyhow::Result<ModeTransitionResult>;
    fn attach_pending_approval(&mut self, request_id: ApprovalRequestId);
    fn clear_pending_approval(&mut self, request_id: &ApprovalRequestId);
}
```

Implementation note:

- these traits can remain crate-private abstractions implemented by concrete structs in `quine-core`
- tools and orchestration code should depend on these abstractions rather than open-coding mutations or evaluation precedence

### Tool-local permission API

Rather than changing the core `Tool` trait immediately, Quine can add an internal helper trait implemented alongside specific tools.

```rust
pub trait ToolPermissionAdapter {
    fn build_permission_request(
        &self,
        invocation: &ToolInvocation,
    ) -> anyhow::Result<PermissionRequest>;
}
```

If a later feature proves that the `Tool` trait itself needs a permission hook, that should be introduced in a dedicated PR after the adapter pattern has been validated.

### Filesystem and command helpers

These helpers should be pure or near-pure functions so they are easy to test.

```rust
pub fn authorize_path(
    context: &PermissionContext,
    path: &Path,
    scope: PermissionScope,
) -> PermissionDecision;

pub fn classify_command(argv: &[String]) -> CommandRisk;
```

Suggested shell-risk model:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommandRisk {
    ReadOnly,
    WritesWorkspace,
    SpawnsSubprocess,
    HighRisk,
    Unknown,
}
```

### `quine-harness` approval routing API

The harness should mediate between paused core execution and operator responses.

```rust
pub trait ApprovalBroker {
    async fn publish_request(&self, request: ApprovalRequest) -> anyhow::Result<()>;
    async fn await_response(
        &self,
        request_id: &ApprovalRequestId,
    ) -> anyhow::Result<ApprovalResponse>;
}
```

The harness can implement this via local session channels first, then later extend it to remote operators without changing `quine-core` evaluation semantics.

### `quine-cli` rendering API

The CLI should consume approval requests as presentation data rather than re-deriving policy.

```rust
pub trait PermissionRenderer {
    fn render_request(&mut self, request: &ApprovalRequest) -> anyhow::Result<()>;
    fn render_outcome(&mut self, outcome: &PermissionOutcome) -> anyhow::Result<()>;
}
```

## API boundary rules

- `quine-core` owns permission semantics, rule precedence, mode transitions, and evaluation.
- `quine-harness` owns trusted config loading, persistence, and async approval transport.
- `quine-cli` owns prompt rendering and operator interaction only.
- `quine-sdk` should not gain permission-specific APIs until local daemon semantics are proven stable.
- The `Tool` trait should remain unchanged in the first implementation if the helper-adapter pattern is sufficient.

## Resulting contract

With the structures above, the end-to-end flow becomes:

1. `quine-harness` bootstraps a `PermissionContext` for a session.
2. A tool invocation builds a `PermissionRequest` through a tool-local adapter.
3. `quine-core` evaluates the request against runtime mode, rules, sandbox policy, and headless behavior.
4. If the outcome is `Allowed` or `Denied`, execution proceeds or fails immediately.
5. If the outcome is `RequiresApproval`, the harness emits an `ApprovalRequest` and pauses execution.
6. The CLI renders the request and sends back an `ApprovalResponse`.
7. The harness resumes execution and, if requested, mutates `PermissionContext` with a new session or persisted rule.

This contract keeps policy centralized, tool semantics explicit, and approval transport replaceable.

## Crate ownership

### `quine-core`

Owns:

- permission domain types
- runtime permission context
- permission evaluation engine
- mode transition logic
- tool-facing permission helpers
- approval request lifecycle and checkpoint-safe state
- sandbox-aware path and command authorization helpers

### `quine-harness`

Owns:

- trusted loading of permission-related config and CLI/session inputs
- persistence of session permission state where needed
- request/response plumbing for approval prompts
- operator-facing daemon APIs for approving or denying pending requests
- session bootstrap wiring that injects permission settings into `quine-core`

### `quine-cli`

Owns:

- rendering permission prompts and results
- interactive approve/deny/always-allow choices supported by the first release
- optional inspection surfaces for current mode and pending approvals

### `quine-sdk`

Should remain unchanged if possible in the initial slices. If approval APIs must be exposed for remote operators later, add them in a dedicated feature after the local model is stable.

## Proposed module layout

A likely first-pass module structure in `quine-core`:

- `crates/quine-core/src/permissions/mod.rs`
- `crates/quine-core/src/permissions/types.rs`
- `crates/quine-core/src/permissions/context.rs`
- `crates/quine-core/src/permissions/evaluate.rs`
- `crates/quine-core/src/permissions/mode.rs`
- `crates/quine-core/src/permissions/filesystem.rs`
- `crates/quine-core/src/permissions/command.rs`
- `crates/quine-core/src/permissions/prompt.rs`

Likely harness modules:

- `crates/quine-harness/src/permissions/config.rs`
- `crates/quine-harness/src/permissions/service.rs`
- `crates/quine-harness/src/session/` additions for bootstrap and pending approvals

Likely CLI modules:

- `crates/quine-cli/src/permissions/render.rs`
- `crates/quine-cli/src/permissions/interactive.rs`

The exact file layout can change, but the subsystem should be centralized rather than diffused across unrelated modules.

## Feature Breakdown

## Feature 1: Permission domain model and runtime context foundation

### Objective

Create the shared permission vocabulary and session-scoped runtime state in `quine-core`.

### Scope

- Add core enums and structs for modes, decisions, rules, and rule sources.
- Add `PermissionContext` with support for:
  - current mode
  - session-only rules
  - CLI-provided rules
  - additional allowed working directories
  - prompt suppression or headless behavior flags
  - optional storage for previous mode when entering plan mode
- Add serialization support only where session checkpointing or daemon persistence requires it.
- Keep the public API surface conservative and crate-private where possible.

### Why this slice is self-contained

This feature introduces no end-user prompting by itself. It gives the rest of the system a typed internal contract and a single source of truth for permission state.

### Main crates

- `quine-core`
- narrow bootstrap wiring in `quine-harness`

### Validation guidance

- Unit tests for default context initialization.
- Unit tests for additive rule insertion by source.
- Unit tests for serialization round-trips if the context is persisted.
- Clippy and format checks on the affected crates.

## Feature 2: Shared permission evaluation engine with tool-level defer support

### Objective

Implement the shared decision engine that combines mode, rules, and tool-local analysis into a final permission result.

### Scope

- Define a `PermissionRequest` shape that tools can construct.
- Allow tools to contribute an initial decision or `defer`.
- Define deterministic precedence across:
  - hard tool-local denials
  - explicit deny rules
  - explicit allow rules
  - mode defaults
  - headless prompt behavior
- Return structured outcomes that explain both result and source.
- Keep the engine generic enough for filesystem, shell, process-control, and future tools.

### Why this slice is self-contained

The engine can be implemented and tested with synthetic requests before wiring it into every tool or any CLI prompt surface.

### Main crates

- `quine-core`

### Validation guidance

- Table-driven tests for precedence ordering.
- Unit tests for `defer` behavior.
- Unit tests for source attribution in outcomes.
- Regression tests proving explicit deny beats broader allow.

## Feature 3: Tool integration for current Quine tools

### Objective

Adopt the shared permission engine across Quine’s existing tools without changing the `Tool` trait contract unless truly necessary.

### Scope

- Review each current tool and classify its permission behavior:
  - always safe read-only
  - workspace-limited write
  - process control
  - interactive prompt tool
  - agent-management tool
- Add tool-local permission request builders for:
  - `bash`
  - `apply_patch`
  - `find`
  - `read_file`
  - `spawn`
  - `subagent`
  - `signal`
  - any other stateful tool that executes external actions
- Preserve conservative defaults:
  - read-only tools can be explicitly marked low risk
  - write/process/subagent tools should require explicit policy review
- Ensure tool metadata and runtime checks do not drift apart.

### Why this slice is self-contained

This feature brings immediate value by enforcing consistent policy on existing execution paths without yet requiring advanced sandbox or UI work.

### Main crates

- `quine-core`

### Validation guidance

- Unit tests colocated with each tool for request construction and classification.
- Integration tests for representative allow, deny, and ask flows.
- At least one end-to-end daemon test exercising `bash` and `apply_patch` permission checks.

## Feature 4: Interactive approval routing and pending-request lifecycle

### Objective

Turn `ask` decisions into a concrete approval workflow spanning core, harness, and CLI.

### Scope

- Add a pending approval model in `quine-core` or harness-owned session state.
- Ensure a tool call that reaches `ask` can pause cleanly and resume after a decision.
- Add daemon plumbing for:
  - emitting approval requests
  - receiving approve/deny responses
  - correlating responses to paused tool executions
- Add CLI rendering for local interactive approval prompts.
- Support at least these first-release responses:
  - approve once
  - deny once
  - optionally approve and persist a session rule
- Ensure unresolved prompts in non-interactive sessions fail safe.

### Why this slice is self-contained

This feature closes the loop on the `ask` path without yet expanding into advanced remote operators or complex policy editing.

### Main crates

- `quine-core`
- `quine-harness`
- `quine-cli`

### Validation guidance

- Integration tests for pause/resume behavior.
- Daemon tests covering interactive approve and deny flows.
- Tests proving timed-out or unreachable responders yield deterministic denial or cancellation.
- Verification that checkpoint or replay state is coherent if a session pauses mid-approval.

## Feature 5: Filesystem and workspace boundary policy

### Objective

Centralize path-based authorization so write access, read access, and additional directories are evaluated consistently.

### Scope

- Implement workspace root and additional-directory modeling in the permission context.
- Add canonicalization and safety checks for accessed paths.
- Distinguish read versus write policy where needed.
- Integrate sandbox-derived allowlists into the same evaluation path.
- Ensure path authorization is evaluated on the final resolved target, not only on user-supplied strings.
- Define deterministic behavior for:
  - paths inside workspace
  - paths inside approved additional directories
  - symlink or traversal edge cases
  - paths outside all approved roots

### Why this slice is self-contained

Path policy is the highest-risk area for filesystem tools and can be implemented as a shared utility reused by `read_file`, `find`, `apply_patch`, and future file tools.

### Main crates

- `quine-core`
- `quine-harness` for sandbox bootstrap data if needed

### Validation guidance

- Unit tests for canonicalization and containment checks.
- Tests for symlink and traversal edge cases where supported by the test environment.
- Integration tests for `read_file`, `find`, and `apply_patch` using workspace and additional-directory paths.
- Explicit negative tests for outside-root writes.

## Feature 6: Shell command risk policy for `bash`

### Objective

Add command-aware policy helpers for shell execution so broad command access is not treated as a single undifferentiated permission.

### Scope

- Introduce a shell command analyzer in `quine-core` that can classify:
  - clearly read-only commands
  - write-capable commands
  - dangerous command prefixes or interpreter launches
- Use the analyzer to produce richer `PermissionRequest` metadata for `bash`.
- Keep the first release conservative and explicit rather than trying to be clever.
- Do not depend on an LLM classifier.
- Ensure the system can later support mode-specific stripping of overly broad allow rules for any future auto-execution mode.

### Why this slice is self-contained

This feature improves the safety and explainability of the most powerful tool without depending on broader remote-control or auto-mode infrastructure.

### Main crates

- `quine-core`

### Validation guidance

- Table-driven tests for command prefix classification.
- Regression tests for dangerous nested shell or interpreter patterns.
- Integration tests showing the same explicit rule can allow safe commands while still prompting or denying high-risk patterns.

## Feature 7: Plan mode permission semantics

### Objective

Make plan mode a real permission transition rather than only a UI or planner concern.

### Scope

- Add `PermissionMode::Plan` if not already modeled.
- Implement transition helpers that preserve `pre_plan_mode` and restore it correctly.
- Define plan-mode behavior for tool requests, likely favoring ask/deny for mutating tools while allowing normal read behavior.
- Ensure exiting plan mode restores prior runtime state deterministically.
- Keep any future auto-plan hybrid behavior explicitly deferred unless already needed by current product requirements.

### Why this slice is self-contained

Plan mode already exists conceptually in Quine. This feature formalizes its permission behavior without requiring auto mode, remote approval, or org policy.

### Main crates

- `quine-core`
- small `quine-cli` integration if the mode is user-visible there

### Validation guidance

- Unit tests for mode transitions in and out of plan mode.
- Integration tests proving write-capable tools behave differently in plan mode than in normal mode.
- Tests ensuring nested or repeated transitions do not corrupt stored prior state.

## Feature 8: Headless, scheduled, and background-session approval semantics

### Objective

Define deterministic behavior for sessions that cannot block on a local interactive prompt.

### Scope

- Add explicit prompt behavior settings for:
  - interactive local session
  - headless batch session
  - scheduled/background run
- Ensure unresolved `ask` becomes deny or explicit failure according to policy.
- Propagate clear error messages to the operator.
- Preserve room for future remote responders without requiring them now.

### Why this slice is self-contained

This feature primarily changes orchestration behavior and avoids unsafe assumptions in non-interactive environments.

### Main crates

- `quine-core`
- `quine-harness`
- `quine-cli` for surfaced status messaging

### Validation guidance

- Integration tests for non-interactive session startup.
- Daemon tests proving pending approvals are not silently dropped.
- Tests for scheduled runs returning deterministic permission-denied outcomes.

## Feature 9: Permission inspection, diagnostics, and operator visibility

### Objective

Make the permission engine observable enough to debug precedence and session state safely.

### Scope

- Add structured debug information to permission outcomes.
- Add operator-visible inspection surfaces for:
  - current mode
  - loaded rules by source
  - additional working directories
  - pending approvals
  - last decision reason for a denied or prompted request
- Keep the first release textual and operationally focused; a full TUI management panel is not required.

### Why this slice is self-contained

Diagnostics improve maintainability and QA leverage without changing the permission semantics themselves.

### Main crates

- `quine-core`
- `quine-harness`
- `quine-cli`

### Validation guidance

- Unit tests for decision explanation formatting or structured serialization.
- Integration tests verifying inspection output reflects actual runtime state.
- Manual QA checks confirming denied operations provide actionable reasons.

## Feature 10: Persisted rule editing and trusted policy sources

### Objective

Add durable user-configured permission rules after the runtime engine is stable.

### Scope

- Define trusted config formats for allow, deny, and ask rules.
- Load and merge rules from supported sources with explicit precedence.
- Support session-only rules separately from persisted rules.
- Add CLI or daemon support for turning an approval decision into a persisted rule where appropriate.
- Keep organization-managed or fleet-managed policy as a later additive feature.

### Why this slice is self-contained

This feature expands how policy enters the system but does not require changing the evaluation core built in earlier features.

### Main crates

- `quine-harness`
- `quine-core`
- optional `quine-cli` affordances

### Validation guidance

- Unit tests for parsing and precedence by rule source.
- Tests for invalid config tolerance and clear diagnostics.
- Integration tests proving session rules and persisted rules remain distinguishable.

## Sequencing and Dependencies

Recommended implementation order:

1. Feature 1 — domain model and runtime context
2. Feature 2 — shared evaluation engine
3. Feature 3 — current tool integration
4. Feature 4 — interactive approval lifecycle
5. Feature 5 — filesystem and workspace boundary policy
6. Feature 6 — shell command risk policy
7. Feature 7 — plan mode semantics
8. Feature 8 — headless and background semantics
9. Feature 9 — diagnostics and inspection
10. Feature 10 — persisted rule editing and trusted sources

Dependency notes:

- Feature 2 depends on Feature 1.
- Feature 3 depends on Features 1 and 2.
- Feature 4 depends on Feature 2 and practically on Feature 3 for high-value coverage.
- Feature 5 can begin after Feature 2, but provides the most value once Feature 3 is wired into file tools.
- Feature 6 depends on Feature 3’s `bash` integration and the shared request model from Feature 2.
- Feature 7 depends on Feature 1 and Feature 2.
- Feature 8 depends on Feature 4.
- Feature 9 can begin after Feature 2, but is most useful after Features 4 through 8.
- Feature 10 should land last so persisted policy does not harden around unstable semantics.

## Suggested Feature Request Breakdown

If these are tracked as separate feature requests under `features/`, the following split keeps scope focused:

- `permission-foundation-and-runtime-context`
- `shared-permission-evaluation-engine`
- `tool-permission-integration`
- `interactive-approval-routing`
- `filesystem-permission-boundaries`
- `bash-command-risk-policy`
- `plan-mode-permission-semantics`
- `headless-and-background-permission-behavior`
- `permission-diagnostics-and-inspection`
- `persisted-permission-rules-and-sources`

If the team wants fewer PRs, Features 5 and 6 can merge into a single “tool risk policy hardening” slice, and Features 8 and 9 can merge into a single “operational semantics and diagnostics” slice.

## Validation Strategy

Every feature should validate at three layers where applicable:

### Unit tests

Use unit tests in the owning module for:

- mode transitions
- precedence rules
- path and command classification
- config parsing
- source attribution and diagnostics

### Integration tests

Use crate-level integration tests for:

- paused approval workflows
- tool execution under different modes
- workspace and additional-directory access
- headless deny behavior
- persisted-rule merging

### Daemon-level tests

Use local daemon tests for:

- interactive approval prompts
- non-interactive failure behavior
- multi-round sessions involving permission decisions
- visibility of pending approvals and final outcomes

The eventual implementation PRs for these features should continue to satisfy:

- `cargo build`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --all -- --check`

## Key Risks and Mitigations

### Risk: policy logic becomes scattered across crates

Mitigation:

- keep evaluation logic in `quine-core`
- let `quine-harness` and `quine-cli` route prompts rather than invent policy
- add diagnostics early enough to catch drift

### Risk: tool-local checks diverge from shared policy semantics

Mitigation:

- force tools to emit structured permission requests
- centralize final evaluation in one engine
- add per-tool regression tests around classification

### Risk: path safety bugs around symlinks and additional directories

Mitigation:

- canonicalize before evaluation where possible
- test edge cases explicitly
- integrate sandbox allowlists into the same boundary checks

### Risk: headless sessions accidentally bypass prompts

Mitigation:

- make prompt behavior explicit in session state
- fail closed when no responder exists
- cover this behavior with daemon-level tests

### Risk: persisted rules lock in poor semantics too early

Mitigation:

- land durable config editing after the evaluation model stabilizes
- keep source attribution in outcomes so future migrations stay tractable

## Recommended First Milestone

The best first milestone is Features 1 through 4 together:

- shared permission domain model
- shared evaluation engine
- integration with current tools
- interactive approval routing

That milestone would give Quine a real, end-to-end permission system with allow, deny, and ask flows while leaving the more specialized hardening work for later focused slices.

## Practical Summary

Quine should adopt permissions as a first-class execution policy layer centered in `quine-core`, with `quine-harness` handling trusted bootstrap and approval plumbing, and `quine-cli` handling operator interaction.

The safest rollout is incremental:

- first establish the runtime model
- then establish deterministic evaluation
- then wire tools into it
- then make approval routing work end-to-end
- then harden filesystem, shell, plan-mode, and headless behavior
- then add diagnostics and persisted rule management

This sequence keeps each feature independently reviewable while steadily converging on the richer permission architecture described in `permission-design-investigation.md`.
