# Permission Design Investigation

This document describes how the permission system is structured in this codebase, what state it owns, how decisions are made, and how that logic interacts with other subsystems such as tool execution, sandboxing, plan mode, remote control, swarms, and the TUI.

## Scope

The permission system here is not a single module. It is a distributed design with five major layers:

1. Bootstrapping and policy loading.
2. Session-scoped permission state.
3. Tool-specific permission checks.
4. Prompting, approval routing, and persistence.
5. Cross-cutting integrations such as sandboxing, remote sessions, and telemetry.

The core implementation is centered around:

- `Tool.ts`
- `types/permissions.ts`
- `utils/permissions/permissionSetup.ts`
- `utils/permissions/permissions.ts`
- `hooks/useCanUseTool.tsx`
- `hooks/toolPermission/PermissionContext.ts`
- `services/tools/toolExecution.ts`

## 1. Conceptual model

At a high level, the design separates three concerns:

- Policy representation: modes, rules, directories, and update operations.
- Decision evaluation: whether a specific tool invocation is allowed, denied, or should ask.
- Interaction handling: how an `ask` result becomes a user prompt, a remote callback, a swarm-leader mailbox request, or an auto-denial.

This separation is important because the system does not treat permissions as only a UI concern. A permission decision can be produced by:

- static rules from settings or CLI,
- tool-local logic,
- path safety checks,
- plan/auto mode semantics,
- hooks,
- classifiers,
- remote responders,
- swarm leaders,
- or the local interactive user.

The result type encodes that directly. `types/permissions.ts` defines:

- permission modes such as `default`, `acceptEdits`, `bypassPermissions`, `dontAsk`, `plan`, and optionally `auto`,
- rule behaviors `allow`, `deny`, `ask`,
- rule sources such as `userSettings`, `projectSettings`, `localSettings`, `policySettings`, `cliArg`, `command`, and `session`,
- decision/result variants `allow`, `deny`, `ask`, and the internal `passthrough`.

`passthrough` is a notable design choice. It means a tool can intentionally defer the final answer to the higher-level permission engine instead of committing locally.

## 2. Session permission state

The main state carrier is `ToolPermissionContext` in `Tool.ts`.

It holds:

- the current permission mode,
- additional working directories,
- allow/deny/ask rules partitioned by source,
- feature availability flags such as bypass-mode availability and auto-mode availability,
- stripped dangerous rules used when entering auto-mode semantics,
- prompt-avoidance flags for headless/background contexts,
- and `prePlanMode`, which preserves the previous mode when plan mode is entered.

This is a session-scoped runtime object, not just a projection of settings files. That matters because some permission state is intentionally ephemeral:

- session-only rules,
- CLI-provided rules,
- runtime-added directories,
- mode transitions,
- and temporary stripping/restoration of dangerous rules during auto mode.

`getEmptyToolPermissionContext()` makes that explicit: the runtime state starts minimal and is populated by setup code, not by static construction.

## 3. Bootstrapping and initial policy assembly

`utils/permissions/permissionSetup.ts` builds the initial context.

The setup path does several distinct jobs:

### 3.1 Choose the initial mode

`initialPermissionModeFromCLI()` resolves the starting mode from:

- `--dangerously-skip-permissions`,
- `--permission-mode`,
- settings default mode,
- and feature gates.

The resolution is not purely syntactic. It also applies policy:

- bypass mode can be disabled by org gate or settings,
- auto mode can be blocked by cached gate state,
- remote sessions restrict which default modes from settings are honored.

This means mode selection is already part of policy enforcement, not just configuration parsing.

### 3.2 Load rules from disk

`permissionsLoader.ts` reads permission rules from enabled setting sources.

Important behaviors:

- `policySettings` can force `allowManagedPermissionRulesOnly`, which suppresses non-managed rules.
- Editing helpers use a lenient JSON loader so invalid unrelated settings do not destroy permission edits.
- Rules are normalized through the rule parser/serializer pipeline, which helps collapse legacy aliases to canonical tool names.

### 3.3 Merge CLI and disk state

`initializeToolPermissionContext()` builds a base context from:

- selected mode,
- CLI allow/deny rules,
- additional working directories,
- bypass/auto availability flags.

Then it applies disk rules additively with `applyPermissionRulesToPermissionContext()`.

### 3.4 Validate additional workspace directories

Setup also validates directories from settings and `--add-dir`, then adds them to the context as additional working directories. That makes directory access part of the permission model rather than just a shell/path feature.

### 3.5 Detect and strip dangerous rules

One of the more important design choices lives here: auto mode is not just “default mode with a classifier.”

`permissionSetup.ts` explicitly detects rule patterns that would bypass the classifier:

- broad Bash rules,
- dangerous Bash prefixes like interpreters,
- dangerous PowerShell prefixes like `iex`, `start-process`, nested shells, etc.,
- Agent allow rules that would auto-approve sub-agent spawn.

When auto mode is activated, those rules are stripped from the in-memory context and stored in `strippedDangerousRules`. When auto mode is exited, they are restored.

This is a strong signal about the intended trust model:

- rules are allowed to authorize actions in ordinary modes,
- but rule-based preapproval must not undermine classifier-based safety in auto mode.

## 4. Mode transitions are first-class policy changes

`transitionPermissionMode()` in `permissionSetup.ts` centralizes mode switching side effects.

That function is doing more than toggling a string. It coordinates:

- plan mode enter/exit bookkeeping,
- auto mode activation/deactivation,
- dangerous-rule stripping/restoration,
- plan exit attachment state,
- and cleanup of `prePlanMode`.

Plan mode is especially interesting:

- `prepareContextForPlanMode()` records the previous mode in `prePlanMode`,
- if the user has opted into “use auto mode during plan,” plan mode can retain classifier semantics,
- if plan is entered from `auto`, dangerous permissions may be restored or preserved depending on plan-auto settings,
- `transitionPlanAutoMode()` can reconcile this live when settings change mid-plan.

This means plan mode is not an isolated alternate workflow. It is layered on top of the same permission engine.

## 5. The tool execution pipeline

`services/tools/toolExecution.ts` is where permissions become operational.

The execution path roughly looks like:

1. normalize/backfill tool input,
2. run validation and hooks,
3. compute permission decision,
4. if denied, emit a tool-result error message,
5. if allowed, continue into the tool call.

The critical bridge is `resolveHookPermissionDecision(...)`, which eventually relies on `canUseTool`, typically created by `hooks/useCanUseTool.tsx`.

Permission is therefore evaluated after the tool is known and its input is normalized, but before the tool call executes.

That ordering matters because:

- tools can expose richer permission semantics than just tool-name gating,
- path-based checks need canonicalized paths,
- hooks may transform input,
- and the final allowed input may differ from the model-provided input.

## 6. Two-stage decision model: tool-local check then orchestration

The design uses two layers of decision making.

### 6.1 Tool-local permission logic

Each tool can implement `checkPermissions`.

Examples:

- `FileEditTool` and `FileWriteTool` delegate to filesystem permission helpers.
- `BashTool` delegates to `bashToolHasPermission(...)`.
- `PowerShellTool` delegates to `powershellToolHasPermission(...)`.
- `WebFetchTool` checks preapproved hosts and domain-scoped rules.
- `AgentTool` usually auto-allows outside auto mode, but returns `passthrough` in auto mode so the outer engine can apply stronger handling.
- `ExitPlanModeV2Tool` intentionally always asks for non-teammates, even though it is an internal control-flow tool.

This makes tools the place where input semantics are understood.

### 6.2 Orchestration-layer permission handling

`hasPermissionsToUseTool()` in `utils/permissions/permissions.ts` is the cross-tool policy engine.

It takes the tool-local result and applies global semantics:

- transform `ask` into `deny` in `dontAsk` mode,
- run auto-mode classifier logic,
- short-circuit safe `acceptEdits` cases for some tools,
- invoke hooks for headless agents,
- maintain denial tracking,
- and preserve safety-check denials that must not be classifier-approved.

This split is one of the cleaner parts of the design. Tools decide what the action means; the engine decides how the session’s current policy should treat that action.

## 7. Filesystem permissions: the deepest specialization

Filesystem access is where the permission design is most layered.

Relevant modules:

- `utils/permissions/filesystem.ts`
- `utils/permissions/pathValidation.ts`
- `components/permissions/FilePermissionDialog/*`

The policy is not “read/write inside cwd, otherwise ask.” It has multiple ordered checks.

### 7.1 Path validation order

`isPathAllowed()` in `pathValidation.ts` applies checks in a deliberate order:

1. deny rules first,
2. internal editable paths for system-owned files,
3. path safety checks for dangerous files/directories and Windows edge cases,
4. working-directory allowance,
5. internal readable paths,
6. sandbox write allowlist for out-of-workdir writes,
7. explicit allow rules.

The comments make the design intent clear: ordering is security-sensitive. For example:

- internal editable paths must be checked before dangerous-directory blocking because some internal writable state lives under otherwise sensitive locations,
- sandbox write allowlist must not bypass `acceptEdits` semantics for cwd writes,
- path safety checks must happen before working-directory auto-allow.

### 7.2 Internal paths are permission-aware

The system has specific exceptions for Claude-managed files:

- current session plan files,
- session memory,
- project temp directories,
- scratchpad,
- bundled skills extraction,
- some agent memory and task output paths.

That avoids the system tripping over its own infrastructure while still treating user-controlled sensitive paths conservatively.

### 7.3 Sensitive path protections

`filesystem.ts` contains explicit protections for:

- `.git`,
- `.vscode`,
- `.idea`,
- `.claude`,
- shell rc files,
- config files,
- UNC paths and Windows path tricks,
- dangerous config targets such as settings files.

This is more than convenience prompting. It is a hardening layer against code execution and exfiltration through config or shell startup mutation.

### 7.4 Permission suggestions are generated from path context

When a file operation asks for permission, the UI can offer session-scoped rule updates. `generateSuggestions(...)` and the file permission dialog produce structured `PermissionUpdate` objects instead of mutating settings ad hoc.

That design keeps the prompt layer thin: the dialog selects among precomputed, typed updates; the permission engine applies and optionally persists them.

## 8. Shell permissions: rule matching plus classifier overlay

Shell tools have their own rich permission subsystems.

Relevant modules include:

- Bash: `tools/BashTool/*`, especially `bashPermissions.ts` and `pathValidation.ts`
- PowerShell: `tools/PowerShellTool/*`, especially `powershellPermissions.ts`
- shared rule matching: `utils/permissions/shellRuleMatching.ts`

Key properties of the shell design:

- rules can target whole tools or command prefixes,
- command parsing extracts subcommands and redirections,
- suggestions can recommend exact-command or prefix-scoped allow rules,
- compound commands can produce multi-part approval requirements,
- sandbox override is modeled as a permission reason, not just an execution flag.

The shell design is notably more advanced than the generic tool rule model because command content matters as much as tool identity.

In auto mode, the shell path is also where classifier interaction is deepest:

- Bash can run speculative and asynchronous classifier checks,
- auto mode can accept edits without paying classifier cost for some safe file actions,
- PowerShell is intentionally stricter and may require interactive approval unless feature-gated support is enabled.

## 9. The interactive permission flow

`hooks/useCanUseTool.tsx` is the main orchestrator for interactive sessions.

It:

- creates a `PermissionContext`,
- asks `hasPermissionsToUseTool()` for a decision,
- immediately resolves `allow` and `deny`,
- routes `ask` through one of several handlers.

The main handlers are:

- `handleCoordinatorPermission(...)`
- `handleSwarmWorkerPermission(...)`
- `handleInteractivePermission(...)`

### 9.1 Coordinator behavior

When `awaitAutomatedChecksBeforeDialog` is set, the system waits for:

- permission hooks,
- then classifier checks,

before showing a dialog. This is designed for coordinator-style flows where interrupting the user is more expensive than waiting for automation.

### 9.2 Interactive TUI behavior

`handleInteractivePermission(...)` pushes a `ToolUseConfirm` item into the UI queue.

That queue item carries callbacks for:

- allow,
- reject,
- abort,
- recheck,
- and user interaction notifications.

The user interaction tracking is significant. It prevents late classifier approvals from dismissing a dialog while the user is already engaging with it.

### 9.3 Specialized permission UIs

`components/permissions/PermissionRequest.tsx` dispatches to tool-specific UIs:

- filesystem dialogs,
- Bash/PowerShell dialogs,
- WebFetch dialogs,
- plan-mode enter/exit dialogs,
- ask-user-question dialogs,
- fallback dialogs for unknown or remote-only tools.

So the model is:

- shared decision type,
- specialized presentation.

## 10. Persistence and mutation

Permission changes are represented as typed `PermissionUpdate` values.

`utils/permissions/PermissionUpdate.ts` supports:

- `setMode`,
- add/replace/remove rules,
- add/remove directories.

Two separate operations exist:

- apply to runtime context,
- persist to editable settings if the destination supports it.

That distinction is important. Some updates are intentionally non-persistent:

- `session`,
- `cliArg`.

Persistence is limited to editable settings sources:

- `userSettings`,
- `projectSettings`,
- `localSettings`.

The UI uses this structure heavily. For example:

- file permission dialogs choose whether to allow once or allow for session,
- `/permissions` edits and deletes rules through the same update/persist pipeline,
- managed rules from `policySettings` can be displayed but not modified.

This is a strong part of the design because it avoids direct settings-file mutation logic being scattered across prompts.

## 11. Hooks are part of the permission system, not an add-on

Hooks are deeply integrated.

`PermissionContext.runHooks(...)` executes `PermissionRequest` hooks and can receive:

- allow decisions,
- deny decisions,
- updated input,
- updated permissions to persist/apply.

Important implications:

- hooks can grant permission before the user sees a prompt,
- hooks can deny permission and optionally interrupt execution,
- hooks can modify the input that eventually gets executed.

For headless agents, `runPermissionRequestHooksForHeadlessAgent(...)` is especially important. It gives hooks a chance to resolve a request before the system falls back to auto-deny because there is no UI available.

This is a flexible design, but it also means hooks sit on a sensitive boundary. They are effectively privileged policy participants.

## 12. Remote control and bridge interactions

The system has explicit permission plumbing for remote sessions and bridge mode.

Relevant files:

- `remote/RemoteSessionManager.ts`
- `remote/remotePermissionBridge.ts`
- `bridge/bridgePermissionCallbacks.ts`

The important design point is that remote permission prompting reuses the same local permission queue concepts instead of inventing a second approval model.

Remote requests arrive as control messages, are turned into:

- synthetic assistant messages,
- real `ToolUseConfirm` queue entries,
- and bridge/remote callbacks for responses.

This allows the local client to render permission prompts for tool calls that execute elsewhere.

The tool-stub path in `remotePermissionBridge.ts` is also telling: if the remote environment has a tool the local client does not know, the system still routes it through a fallback permission UI rather than silently accepting or dropping it.

## 13. Swarm and teammate interactions

For swarm workers, permissions are not decided locally by default.

`handleSwarmWorkerPermission(...)`:

- optionally tries classifier auto-approval first,
- forwards the permission request to the leader via mailbox,
- waits for leader approval/rejection callbacks,
- and exposes waiting state in app state.

Plan mode also has teammate-specific behavior:

- some plan enter/exit flows bypass local prompts for teammates,
- the leader or owning context becomes the approval authority.

This means permission authority is deliberately hierarchical in swarm mode.

## 14. Workspace trust is a separate boundary

`interactiveHelpers.tsx` makes an explicit distinction:

- workspace trust and startup approvals happen before ordinary tool permissions,
- bypass-permissions mode does not bypass workspace trust,
- CLAUDE.md external includes and MCP setup approval are handled at startup.

This is a good architectural separation. Tool permissions answer “may this action run now?” Workspace trust answers “may this repository and its ambient configuration influence the session at all?”

Those are different risks, and the code treats them separately.

## 15. `/permissions` is an inspector/editor for live policy

The `/permissions` command is not just a settings editor.

The TUI surfaces:

- current allow/ask/deny rules by source,
- recent denials,
- workspace directories,
- add/delete flows,
- managed-rule visibility,
- and warnings such as unreachable rules.

This is operationally useful because the actual runtime context is multi-source and mode-sensitive. A plain settings file view would not reflect session or CLI state properly.

## 16. Observability and telemetry

Permissions are heavily instrumented.

Examples include:

- per-tool permission decisions,
- code-edit accept/reject counters,
- slow permission decision logging,
- hook-originated decisions,
- auto-mode denial notifications,
- unary logging around prompt interactions,
- analytics around accept/reject with feedback.

The instrumentation is not incidental. It reflects how much behavior is driven by asynchronous policy components such as hooks, classifiers, bridge responders, and remote flows. Without telemetry, it would be difficult to understand why a tool was allowed or denied.

## 17. Design strengths

Several strong choices show up repeatedly:

### 17.1 Typed updates instead of ad hoc mutation

`PermissionUpdate` acts like a small command language for policy changes. That keeps UI, persistence, and runtime application aligned.

### 17.2 Tool-local semantics plus shared orchestration

Tools own meaning; the engine owns policy mode behavior. That is usually the right split.

### 17.3 Explicit handling of dangerous rule interactions

The stripping/restoration of dangerous allow rules in auto mode is careful and materially improves safety.

### 17.4 Runtime context instead of static settings projection

Because the context includes CLI/session/plan/auto state, decisions reflect the actual live environment.

### 17.5 Multiple approval transports on one model

Local TUI, bridge, remote, and swarm flows all converge on the same decision/prompt machinery.

## 18. Complexity and tradeoffs

The main weakness of the design is not that it is conceptually wrong. It is that policy is distributed across many layers, so understanding precedence is hard.

Some examples:

- tool-local `checkPermissions` can return `allow`, `ask`, `deny`, or `passthrough`,
- hooks may modify inputs or resolve decisions after tool-local checks,
- auto mode may reinterpret `ask`,
- plan mode can carry auto semantics,
- headless agents may turn promptable actions into denials,
- filesystem and shell tools each have their own sub-engines,
- sandbox write allowlists influence path authorization.

That complexity appears intentional, but it raises maintenance cost. The biggest ongoing risks are:

- precedence mistakes between path safety, working-directory rules, and sandbox exceptions,
- rule stripping/restoration drift when modes change in unusual sequences,
- tool-local permission code diverging from shared policy expectations,
- and hidden interactions between hooks and the final executed input.

## 19. Practical summary

The permission system is best understood as a policy engine around tool execution, not as a dialog manager.

The core invariants seem to be:

- rules are source-aware and mode-aware,
- runtime context is the source of truth,
- tools define the semantics of their own inputs,
- orchestration decides how those semantics are treated under the current mode,
- prompts are just one resolution path among many,
- and auto/plan/sandbox/remote flows are implemented by adapting the same shared permission model rather than replacing it.

That is a sophisticated design. Its biggest challenge is not lack of capability, but the amount of cross-module knowledge required to reason about precedence end to end.
