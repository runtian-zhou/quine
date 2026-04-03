# Memory Systems Design

## Purpose

This document explains the two distinct memory systems in this codebase:

1. Persistent memory (`memdir` / auto-memory)
2. Session memory (`session-memory/summary.md`)

They solve different problems:

- Persistent memory stores durable knowledge that should survive across conversations.
- Session memory stores a rolling summary of the current session so the conversation can survive compaction and resume cleanly.

The two systems are related, but they are not interchangeable.

## Scope

This design doc covers:

- goals and non-goals
- storage layout
- runtime lifecycle
- how memory is loaded into chat context
- how memory is updated
- how memory interacts with compaction
- failure and staleness behavior
- important feature gates and implementation boundaries

Primary implementation files:

- `memdir/paths.ts`
- `memdir/memdir.ts`
- `memdir/memoryScan.ts`
- `memdir/findRelevantMemories.ts`
- `services/extractMemories/extractMemories.ts`
- `utils/claudemd.ts`
- `context.ts`
- `utils/attachments.ts`
- `utils/messages.ts`
- `services/SessionMemory/sessionMemory.ts`
- `services/SessionMemory/sessionMemoryUtils.ts`
- `services/compact/sessionMemoryCompact.ts`
- `utils/permissions/filesystem.ts`

## Design Summary

### Persistent memory

Persistent memory is a file-backed knowledge base scoped primarily by project, with optional team and agent-specific variants.

It exists to answer questions like:

- What does the user prefer across sessions?
- What durable project context is not derivable from code or git?
- What external references should the assistant remember to check later?

Persistent memory is injected into model context in two different ways:

1. Broad context:
   - `MEMORY.md` can be loaded as part of `userContext.claudeMd`.
2. Targeted recall:
   - specific memory files can be selected per-turn and injected as `relevant_memories` attachments.

### Session memory

Session memory is a per-session markdown summary file.

It exists to answer different questions:

- What has happened in this session so far?
- What is the current task state?
- Which files, commands, decisions, and corrections matter for continuity?

Session memory is not part of the normal per-turn prompt prefix. Instead:

- it is generated from the current conversation in the background
- it is later used as the basis for compaction summaries

In short:

- persistent memory improves future conversations
- session memory preserves the current conversation

## Goals

### Persistent memory goals

- Preserve durable knowledge across sessions.
- Keep memory human-readable and inspectable on disk.
- Allow selective recall instead of blindly loading everything.
- Avoid storing information that is derivable from source code or git history.
- Allow direct user intent such as "remember this" or "forget this".

### Session memory goals

- Preserve continuity when message history is compacted.
- Maintain a structured summary of the current session.
- Update in the background without interrupting the main conversation.
- Make resume and post-compact continuation coherent.

## Non-goals

### Persistent memory non-goals

- It is not a task tracker for current-turn work.
- It is not a replacement for plans or task tools.
- It is not intended to store raw transcripts.
- It is not a source of truth for codebase state that can be read from the repo.

### Session memory non-goals

- It is not intended to survive as durable cross-session knowledge.
- It is not intended to replace durable memory extraction.
- It is not automatically inserted into every model turn.
- It is not a general-purpose note system outside the current session.

## Storage Layout

## Persistent memory layout

Auto-memory is enabled by default unless explicitly disabled by env or settings.

Path resolution is implemented in `memdir/paths.ts`.

Resolution order:

1. `CLAUDE_COWORK_MEMORY_PATH_OVERRIDE`
2. trusted `autoMemoryDirectory` setting
3. default project-scoped path under `~/.claude/projects/<sanitized-git-root>/memory/`

Key properties:

- path is project-scoped
- worktrees share the same canonical git-root-backed memory directory
- the directory is normalized and validated for safety

The directory contains:

- `MEMORY.md`
- topic files such as `user_role.md`, `feedback_testing.md`, etc.
- optional subdirectories

`MEMORY.md` is an index, not the memory payload itself.

Expected shape:

- one durable memory per file
- frontmatter describing the memory
- concise index entries in `MEMORY.md` pointing to those files

### Team memory

When the feature is enabled, team memory exists as a sibling concept under the same auto-memory base, but in a separate team-scoped directory and with its own `MEMORY.md`.

### Agent memory

When the user works through a custom agent with its own memory scope, relevant recall can be redirected into that agent's memory directory rather than the default auto-memory directory.

## Session memory layout

Session memory path is defined in `utils/permissions/filesystem.ts`:

- directory: `{projectDir}/{sessionId}/session-memory/`
- file: `{projectDir}/{sessionId}/session-memory/summary.md`

This means session memory is:

- per project session
- not global
- not shared across sessions by default

The file is initialized from a template in `services/SessionMemory/prompts.ts`.

Sections include:

- Session Title
- Current State
- Task specification
- Files and Functions
- Workflow
- Errors & Corrections
- Codebase and System Documentation
- Learnings
- Key results
- Worklog

## Runtime Architecture

There are three major phases where memory matters:

1. prompt/context construction
2. post-turn extraction/update
3. compaction and resume

### 1. Prompt/context construction

This is how memory influences the model before it answers.

Sources:

- system prompt
- `systemContext`
- `userContext`
- attachments/meta reminders
- transcript messages

Persistent memory participates here.
Session memory usually does not.

### 2. Post-turn extraction/update

After a completed turn:

- durable memory may be extracted into the persistent memory directory
- session memory may be updated to reflect the current conversation state

These are background maintenance paths.

### 3. Compaction and resume

When the message history is too large:

- session memory can become the compact summary
- old messages are collapsed
- recent unsummarized messages are preserved

This is where session memory becomes directly relevant to future model context.

## Persistent Memory Design

## Core model

Persistent memory uses a file-backed taxonomy with constrained memory types.

The instruction builder in `memdir/memdir.ts` and the taxonomy in `memdir/memoryTypes.ts` define the intended semantics:

- user memory
- feedback memory
- project memory
- reference memory

The memory prompt explicitly tells the model:

- memory is durable and file-based
- `MEMORY.md` is an index
- individual files contain the actual memory payload
- memory should be updated or removed when stale
- current-conversation-only state should not be stored as durable memory

## Why `MEMORY.md` exists

`MEMORY.md` is a compact always-loadable entrypoint.

It serves two purposes:

1. lightweight index into the memory directory
2. stable bootstrap context at session start

It is intentionally capped:

- max 200 lines
- max 25 KB

If the file exceeds limits, it is truncated and a warning is appended.

This prevents the memory index from silently consuming too much prompt budget.

## Loading persistent memory into context

Persistent memory enters context through `getUserContext()` in `context.ts`.

That function builds `claudeMd` by calling:

- `getMemoryFiles()`
- `filterInjectedMemoryFiles(...)`
- `getClaudeMds(...)`

`utils/claudemd.ts` loads:

- managed instructions
- user instructions
- project instructions
- local instructions
- auto-memory `MEMORY.md`
- optional team-memory `MEMORY.md`

Then `context.ts` inserts the rendered result into:

- `userContext.claudeMd`

This makes persistent memory part of the conversation prefix, not a regular chat message.

### Important nuance: index injection can be disabled

When the relevant-memory recall path is enabled, `filterInjectedMemoryFiles()` removes `AutoMem` and `TeamMem` from the files used to build `claudeMd`.

That means:

- the memory index is no longer broadly injected into every turn
- instead, only query-relevant memory files are surfaced as attachments

This is a key design choice:

- broad injection is simple but noisy
- targeted recall is higher precision and cheaper in prompt budget

## Relevant memory recall

Relevant recall is implemented in:

- `memdir/memoryScan.ts`
- `memdir/findRelevantMemories.ts`
- `utils/attachments.ts`
- `utils/messages.ts`

### Step 1: discover candidate memory files

`scanMemoryFiles()` recursively scans the memory directory:

- excludes `MEMORY.md`
- reads only the frontmatter/header region
- extracts filename, mtime, description, and parsed type
- sorts newest-first
- caps to 200 files

This gives a low-cost candidate set for relevance selection.

### Step 2: choose relevant memories

`findRelevantMemories()` uses a side-query model to select which files are worth surfacing for the current user query.

Inputs:

- current user query
- candidate file manifest
- recently used tools
- already surfaced memory paths

Output:

- up to 5 selected memory file paths with mtimes

Important properties:

- conservative selection
- explicit de-dup against previously surfaced memories
- avoids surfacing tool reference docs when current conversation already demonstrates that tool

### Step 3: read actual memory content

`readMemoriesForSurfacing()` reads the selected files with hard caps:

- max lines
- max bytes

If truncated:

- partial content is still surfaced
- the model is told to use `FileRead` for the full file if needed

### Step 4: inject into chat context

`getRelevantMemoryAttachments()` returns a `relevant_memories` attachment.

`utils/messages.ts` renders that attachment into meta user messages wrapped as system reminders.

At this point, relevant memory is no longer just static config; it is now a turn-local context artifact in the chat transcript.

This means relevant persistent memory can influence the current turn without being part of the always-on prompt prefix.

## When persistent memory is updated

Persistent memory can be updated in two ways.

### Path A: direct main-agent writes

The main model prompt includes instructions describing how memory should be saved:

- write memory file
- update `MEMORY.md`

If the model decides to write memory directly, those writes go into the auto-memory directory.

### Path B: background durable-memory extraction

`services/extractMemories/extractMemories.ts` runs as a forked subagent after a completed query loop.

It exists to catch durable information the main agent did not explicitly save.

Behavior:

- only runs after completed turns
- skips work if the main agent already wrote to memory paths in that range
- gets tightly restricted tool permissions
- can read, grep, glob, use read-only shell commands, and edit/write only inside the memory directory

This keeps extraction safe and purpose-built.

### Why this split exists

Direct writes are best when the user explicitly says:

- remember this
- forget this

Background extraction is best when the conversation produced durable knowledge implicitly.

The result is a hybrid model:

- explicit save path
- fallback extraction path

## Persistent memory lifecycle

### Creation

1. auto-memory path is resolved
2. memory directory is ensured to exist
3. `MEMORY.md` and topic files may be created over time

### Read path

1. session starts or context is rebuilt
2. `getUserContext()` loads memory-related files
3. optional relevant-memory prefetch runs for the active prompt
4. selected memories are injected as system reminders

### Update path

1. main model or extractor decides durable knowledge should be persisted
2. topic files are written or edited
3. `MEMORY.md` index is updated
4. future sessions and future turns can recall the new knowledge

### Deletion / correction

1. user requests forgetting, or
2. memory is detected as stale or wrong
3. topic file and/or index entry is removed or updated

The design assumes memory drift is normal and correction is part of normal maintenance.

## Session Memory Design

## Core model

Session memory is a structured session summary maintained in a single file.

It is generated from the conversation itself and optimized for continuity, not long-term recall.

Its implementation is in:

- `services/SessionMemory/sessionMemory.ts`
- `services/SessionMemory/sessionMemoryUtils.ts`
- `services/SessionMemory/prompts.ts`

## Why a separate session-memory file exists

Large chat transcripts eventually need compaction.

A generic conversation summary is often too weak for coding sessions because it can miss:

- current state
- important files
- commands
- errors already encountered
- exact outputs or deliverables

Session memory solves this by maintaining a structured summary continuously during the session instead of only summarizing at compaction time.

## Session memory update trigger

Session memory extraction is a post-sampling hook registered by `initSessionMemory()`.

It only runs when:

- not in remote mode
- auto-compact is enabled
- the feature gate is on
- the query is the main REPL thread

Additional thresholds:

- minimum total context tokens before initialization
- minimum context growth since last extraction
- enough tool calls since the previous extraction, or a natural turn boundary with no tool calls in the last assistant turn

This prevents excessive extraction churn.

## Session memory update flow

### Step 1: decide whether to extract

`shouldExtractMemory()` uses:

- total estimated tokens in current messages
- tokens since last extraction
- tool calls since last extraction
- whether the last assistant turn still has tool calls

The design intentionally requires the token-growth threshold before extraction.

### Step 2: prepare the session memory file

`setupSessionMemoryFile()`:

- creates the session-memory directory if needed
- creates `summary.md` if needed
- initializes from template if the file is new
- reads the current file contents

The updater always starts from the current note state.

### Step 3: run a forked updater agent

The updater uses `runForkedAgent()` with:

- a prompt telling it to update the notes file
- the existing transcript context
- a tool permission function that only allows `FileEdit` on the exact summary file

This is deliberately narrow:

- the updater cannot roam the repo
- it cannot rewrite arbitrary files
- it cannot call unrelated tools

### Step 4: record extraction progress

After success, the system updates:

- last extraction token count
- last summarized message id, if safe

That last summarized message id becomes critical later for compaction.

## Session memory and normal chat context

Session memory is not normally inserted into every prompt.

That is an intentional design choice.

Reasons:

- the file is optimized for compaction continuity, not for every-turn reasoning
- always injecting it would duplicate transcript state
- it would consume prompt budget continuously
- it risks making the model overfit to stale session notes instead of the live transcript

Instead, session memory is:

- maintained in the background
- consulted when compaction happens
- optionally used manually through summary-oriented commands

## Session memory compaction path

Session memory becomes part of model-visible context during compaction.

Implementation lives in `services/compact/sessionMemoryCompact.ts`.

### High-level behavior

When `/compact` or auto-compact runs, the system first tries session-memory-based compaction.

If session memory is usable:

1. wait for any in-flight session-memory extraction to finish
2. load `summary.md`
3. reject it if missing or still equal to the template
4. find the boundary between summarized and unsummarized messages
5. keep the unsummarized tail
6. create a compact summary message from session memory
7. rebuild post-compact messages

If not usable:

- fall back to the legacy compaction path

### Why `lastSummarizedMessageId` matters

The system needs to know which part of the transcript is already represented in session memory.

`lastSummarizedMessageId` provides that boundary.

Normal case:

- keep only messages after that message

Resume case:

- if there is session memory but no boundary id, treat it as resumed-session compaction
- session memory becomes the summary
- current messages may all be considered preserved tail depending on boundary calculation

### Result of compaction

The compacted transcript contains:

- a compact boundary system message
- a compact summary user message generated from session memory
- preserved recent messages
- restored hook/plan context as needed

At that point, the session memory summary has effectively replaced older transcript history in model context.

This is the primary place where session memory interacts with chat context.

## End-to-End Lifecycle

## Persistent memory lifecycle

### Session start

1. auto-memory path is resolved
2. `getMemoryFiles()` loads `MEMORY.md` and related files
3. `getUserContext()` renders them into `claudeMd`
4. that content becomes part of the prompt prefix

### During a turn

1. if relevant-memory prefetch is enabled, the last user prompt is analyzed
2. candidate memory files are scanned
3. a side-query selects useful memories
4. selected files are attached as system reminders

### After a turn

1. if the main model wrote memory directly, extraction is skipped
2. otherwise, background durable-memory extraction may update memory files

### Later session

1. the saved memory can be loaded again through `claudeMd`
2. or selectively recalled as relevant memory

## Session memory lifecycle

### Session start

1. no session memory may exist yet
2. once thresholds are met, the session-memory file is created from template

### During the session

1. post-sampling hook evaluates extraction thresholds
2. background updater edits `summary.md`
3. latest boundary id and token checkpoints are recorded

### During compaction

1. compaction first tries to use session memory
2. if available, older transcript is replaced by a session-memory-based summary
3. recent unsummarized tail is preserved

### After compaction

1. the live transcript is smaller
2. continuity comes from the compact summary
3. session memory remains on disk and can continue to be updated

## Interaction Between the Two Systems

These systems are complementary but intentionally separate.

### Persistent memory feeds future sessions

Persistent memory is durable and cross-session:

- user preferences
- durable project context
- external references
- long-lived collaboration rules

### Session memory feeds current-session continuity

Session memory is ephemeral and session-scoped:

- current task state
- current worklog
- recent errors and fixes
- current output artifacts

### Shared behaviors

Both systems:

- are file-backed
- use background subagents in some paths
- use restrictive tool permissions
- prefer structured markdown over opaque storage

### Different context insertion points

Persistent memory:

- enters prompt prefix directly via `userContext`
- may also enter transcript as relevant-memory system reminders

Session memory:

- does not normally enter prompt prefix
- enters context mainly through compact summary generation

### Why not unify them

A single system would create design tension:

- durable memory wants low churn and high precision
- session continuity wants frequent updates and session-local detail

If unified:

- durable memory would get polluted with transient session state
- session continuity would be burdened by durability rules and stale memory concerns

The split is correct.

## Caching and Performance

## Persistent memory performance choices

- `getMemoryFiles()` is memoized
- `getUserContext()` is memoized
- `MEMORY.md` is concise and capped
- relevant-memory recall scans only frontmatter first
- relevant-memory selection uses a side-query with a small JSON output
- already-surfaced memories are de-duped across the live transcript

## Session memory performance choices

- extraction is threshold-gated
- extraction runs in the background
- updater is restricted to one file
- session memory is only pulled into model-visible context when compaction needs it

## Safety and Correctness

## Persistent memory safety

- auto-memory path overrides are validated
- extraction agent can only write inside the memory directory
- stale memory is explicitly acknowledged as a risk
- the prompt instructs the model to verify memory-derived claims against current repo state

## Session memory safety

- updater can only edit one exact file
- extraction waits for a stable boundary
- compaction protects tool-use/tool-result pair integrity when preserving the tail
- compaction falls back if session memory is unavailable or unusable

## Failure Modes and Expected Behavior

## Persistent memory failure modes

### Memory directory unavailable

Behavior:

- prompt building continues
- writes fail when attempted
- debug logging records the issue

### `MEMORY.md` too large

Behavior:

- index is truncated with an explicit warning
- system still gets partial memory bootstrap context

### Relevant-memory selector fails

Behavior:

- no relevant memories are surfaced for that turn
- conversation continues normally

### Memory drift

Behavior:

- prompts instruct the model to verify current state before relying on old memory
- stale files should be updated or removed

## Session memory failure modes

### Session memory gate disabled

Behavior:

- no session memory extraction runs
- compaction falls back to legacy behavior

### Summary file missing

Behavior:

- session-memory compaction returns `null`
- legacy compaction path takes over

### Summary file still equal to template

Behavior:

- treated as empty
- legacy compaction path takes over

### Last summarized message id missing or invalid

Behavior:

- special resumed-session path may apply, or
- compaction falls back to legacy behavior if a safe boundary cannot be determined

## Feature Gates and Modes

Important controls:

- `CLAUDE_CODE_DISABLE_AUTO_MEMORY`
- `CLAUDE_CODE_SIMPLE`
- `autoMemoryEnabled` setting
- relevant-memory feature gate controlling selector-based recall
- session memory feature gate
- session-memory compact feature gate
- team memory feature gate

Important consequence:

The exact way persistent memory enters context can change by feature flag:

- always-on `MEMORY.md` injection
- or targeted relevant-memory attachment injection

This is the biggest implementation-mode difference to keep in mind when debugging memory behavior.

## Operational Mental Model

Use this model when reasoning about the system:

- `claudeMd` is baseline instruction/context material
- `relevant_memories` are selective just-in-time reminders
- `summary.md` is the compactable state of the current session

In other words:

- persistent memory is memory for the assistant
- session memory is memory for the transcript

## Sequence Diagrams

### Persistent memory: normal turn with targeted recall

```text
User prompt
  -> context builder loads systemContext + userContext
  -> userContext may include CLAUDE.md and possibly MEMORY.md index
  -> relevant-memory prefetch scans memory headers
  -> selector chooses up to 5 memory files
  -> selected memory files are injected as system reminders
  -> main model answers with those reminders in context
  -> post-turn extractor may update durable memory files
```

### Session memory: ongoing maintenance

```text
Conversation grows
  -> post-sampling hook checks thresholds
  -> if thresholds met, create/read summary.md
  -> forked updater agent edits summary.md
  -> extraction metadata is updated
```

### Session memory: compaction

```text
Compaction requested
  -> wait for in-flight session-memory extraction
  -> load summary.md
  -> find summarized boundary
  -> keep recent unsummarized tail
  -> create compact summary from session memory
  -> rebuild transcript around compact boundary
  -> future turns continue from compacted context
```

## Tradeoffs

### Advantages of the current design

- separates durable knowledge from transient continuity
- keeps both systems inspectable on disk
- supports both broad and selective persistent recall
- de-risks compaction by maintaining structured session state continuously
- confines background updaters with narrow tool permissions

### Costs of the current design

- multiple pathways can make debugging memory behavior non-obvious
- feature gates can materially change how memory enters context
- persistent memory can drift stale
- session memory adds maintenance overhead and state bookkeeping

## Concrete Implementation Plan for Quine

This section translates the design into a staged Quine roadmap.

Implementation should be split into small feature PRs that preserve existing crate boundaries and avoid modifying shared inter-crate traits unless a dedicated coordination PR is required.

Guiding constraints for Quine:

- keep orchestration ownership in `crates/quine-core`
- keep daemon-facing persistence and filesystem integration in `crates/quine-harness`
- keep CLI changes additive and diagnostic-first in `crates/quine-cli`
- prefer additive session-local state over broad trait churn
- make every stage independently testable and shippable

### Phase 0: Architectural groundwork

Before implementing memory behavior, establish the minimal internal seams Quine needs.

Scope:

- define internal core modules for memory concerns without exposing broad new public APIs
- identify where prompt construction, post-turn hooks, compaction, and checkpoint persistence can call memory helpers
- define stable on-disk locations under harness-managed storage

Primary crates/files:

- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/compaction.rs`
- `crates/quine-core/src/persistence.rs`
- `crates/quine-harness/src/storage.rs`
- `crates/quine-core/src/lib.rs`

Concrete work:

- add a new internal `memory` module tree under `crates/quine-core/src/`
- split it into `session.rs`, `template.rs`, `summary.rs`, `persistent.rs`, and `diagnostics.rs` or equivalent focused modules
- add per-session in-memory bookkeeping to `SessionContext` for:
  - session memory status
  - last summarized message boundary
  - persistent memory configuration
  - last memory refresh/extraction timestamps
- define additive persisted fields in `PersistedSession` for memory metadata only when they represent durable restore state
- keep raw durable memory files out of checkpoints; checkpoints should store references and metadata, not duplicate memory content

Planned data structure changes:

- add internal `quine-core` memory types such as:
  - `SessionMemoryState`
  - `SessionMemoryPaths`
  - `PersistentMemoryConfig`
  - `MemoryDiagnostics`
- extend `engine.rs` `SessionContext` with additive fields for:
  - `session_memory: SessionMemoryState`
  - `persistent_memory: Option<PersistentMemoryConfig>`
  - `memory_diagnostics: MemoryDiagnostics`
- extend persisted session state with a small serializable memory metadata struct, for example:
  - `PersistedMemoryState`
  - `PersistedSessionMemoryState`
- keep checkpoints limited to metadata such as:
  - summary path reference
  - last summarized boundary
  - in-flight-disabled/defaultable status fields

Planned API changes:

- no shared inter-crate trait changes in this phase
- add crate-private helpers in `quine-core::memory` for:
  - resolving memory paths
  - deciding whether a refresh/extraction should run
  - loading/saving memory metadata
- optionally add additive internal `SessionContext` methods in `engine.rs`, such as:
  - `schedule_session_memory_refresh(...)`
  - `restore_memory_state(...)`

Acceptance criteria:

- memory modules compile without changing the existing `Tool`, `Agent`, `Dispatcher`, `HarnessService`, or `LlmProvider` trait contracts
- the core can represent memory state for a session even before behavior is enabled

### Feature 1: Session memory foundation and summary file lifecycle

This feature introduces the per-session `summary.md` concept and keeps it independent from compaction initially.

Goal:

- create and maintain a structured session-memory file for each session
- update it asynchronously after completed turns
- tolerate missing files and disabled state cleanly

Primary crates/files:

- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/persistence.rs`
- `crates/quine-harness/src/storage.rs`
- `crates/quine-harness/src/service.rs`
- optionally new files such as:
  - `crates/quine-core/src/memory/session_memory.rs`
  - `crates/quine-core/tests/session_memory.rs`

Concrete work:

- define a session-memory directory under harness storage rooted by session id
- initialize `session-memory/summary.md` from a Quine-owned markdown template
- add a post-turn background maintenance path in the engine that:
  - snapshots the current transcript
  - computes whether session memory should refresh
  - writes or rewrites `summary.md` asynchronously
- start with deterministic summarization logic owned by Quine rather than a second autonomous agent process; the first version can summarize from transcript structure directly or from a single internal LLM call routed by the engine
- record the last summarized message id or equivalent boundary marker in a machine-readable footer or sidecar metadata file
- ensure writes are serialized per session to avoid races with concurrent turns
- expose only lightweight metadata in checkpoints so restore can resume session-memory maintenance cleanly

Planned data structure changes:

- add an internal summary metadata type, for example:
  - `SessionSummaryMetadata { last_summarized_message_index, updated_at, template_version }`
- add a deterministic summary model or intermediate accumulator for rendering `summary.md`, for example:
  - `SessionSummaryDocument`
  - `SessionSummaryUpdate`
- extend persisted session data with only restore-relevant fields, for example:
  - `PersistedSessionMemoryState { enabled, last_summarized_message_index, template_version }`
- use on-disk files under the harness state root:
  - `<state_dir>/sessions/<session_id>/session-memory/summary.md`
  - `<state_dir>/sessions/<session_id>/session-memory/summary.meta.json`

Planned API changes:

- add crate-private session-memory functions in `quine-core`, such as:
  - `session_memory_paths(state_root, session_id) -> SessionMemoryPaths`
  - `initialize_summary_if_missing(...)`
  - `should_refresh_summary(...) -> bool`
  - `refresh_summary_from_history(...)`
  - `load_summary_metadata(...) -> Result<SessionSummaryMetadata>`
  - `store_summary_metadata(...)`
- add additive engine integration points after turn completion so background summary refresh can be scheduled without changing public tool APIs
- keep harness protocol unchanged for this feature unless a small diagnostic exposure is needed; if exposed, make it additive and optional

Validation:

- unit tests for summary initialization, refresh decisions, and boundary metadata parsing
- integration test that runs a multi-turn session and asserts `summary.md` is created and updated
- restore test confirming a resumed harness session preserves session-memory metadata

Why this feature stands alone:

- it adds continuity state without changing prompt construction or compaction behavior
- users get inspectable session summaries immediately

### Feature 2: Session-memory-driven compaction

This feature makes compaction consume session memory when available and fall back safely when not.

Goal:

- use `summary.md` plus boundary metadata to compact history more coherently than generic transcript summarization
- preserve the existing legacy compaction path as a fallback

Primary crates/files:

- `crates/quine-core/src/compaction.rs`
- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/memory/session.rs`
- `crates/quine-core/tests/` integration coverage for compaction flows

Concrete work:

- extend compaction logic to query session-memory state before invoking the generic summarizer path
- if a valid summary and boundary exist:
  - keep the system message
  - replace archived history with a compact assistant summary derived from `summary.md`
  - preserve the unsummarized tail after the recorded boundary
- if summary state is missing, stale, or invalid, retain the current `summarizer_messages()` flow as the fallback
- block or coordinate compaction while a session-memory update is in flight so compaction consumes a consistent summary snapshot
- archive the pre-compact transcript exactly as today so observability and rollback remain intact

Planned data structure changes:

- add a compact-consumable summary input type, for example:
  - `SessionMemoryCompactionInput { summary_text, metadata, boundary }`
- add a boundary-resolution model so compaction can distinguish usable, resumed, and invalid session-memory states, for example:
  - `SessionMemoryBoundary`
  - `SessionMemoryBoundaryResolution`
- add an internal compaction decision model so `quine-core` can record which path was taken without changing external APIs, for example:
  - `CompactionSource::SessionMemory`
  - `CompactionSource::LegacySummarizer`
  - `CompactionPlan`
- extend restore-oriented session-memory metadata only where compaction needs durable resume information, for example:
  - `PersistedSessionMemoryState { last_compaction_source, last_compacted_message_index }`

Planned API changes:

- extend internal compaction helpers with additive parameters or wrapper types so compaction can prefer session memory when available
- add crate-private functions such as:
  - `load_session_memory_for_compaction(...)`
  - `resolve_session_memory_boundary(...)`
  - `build_compaction_plan(...)`
  - `build_compacted_history_from_session_memory(...)`
  - `apply_compaction_plan(...)`
- add additive engine-to-compaction wiring so compaction can coordinate with in-flight summary refreshes through existing session state instead of introducing a new public interface
- do not change external CLI or SDK APIs for this feature; behavior changes remain internal to `quine-core`

Validation:

- unit tests for boundary selection and fallback behavior
- integration test covering both success and fallback paths
- regression test ensuring tool-result archiving and live-tail preservation still work after session-memory compaction is introduced

Why this feature stands alone:

- it improves compaction quality using data already produced by Feature 1
- no persistent cross-session memory is required

### Feature 3: Persistent memory store and durable extraction pipeline

This feature introduces durable memory files that survive across sessions, but does not yet inject them into every prompt.

Goal:

- create a project-scoped durable memory store on disk
- support explicit and automatic durable-memory extraction after turns
- maintain an inspectable `MEMORY.md` index plus one-memory-per-file payloads

Primary crates/files:

- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/memory/persistent.rs`
- `crates/quine-harness/src/storage.rs`
- `crates/quine-harness/src/config.rs`
- `crates/quine-core/tests/` for extraction and storage behavior

Concrete work:

- define Quine’s durable memory root; align it with harness-managed state rather than copying the source design’s TypeScript path layout literally
- resolve memory scope from:
  - session working directory / project root
  - trusted harness config overrides
  - optional environment overrides for local development and testing
- create durable memory file schema:
  - markdown body
  - frontmatter for title, scope, timestamps, and relevance hints
- generate and maintain `MEMORY.md` as an index of durable memory entries
- add an extraction pipeline after completed turns that can:
  - detect explicit user intents like remember/forget
  - detect candidate durable facts under conservative heuristics
  - create, update, or tombstone durable memory files
- keep the first release conservative: prefer under-extraction over aggressive, noisy memory creation
- record extraction diagnostics so later UI/debug tooling can explain why a memory changed

Planned data structure changes:

- add durable-memory internal types, for example:
  - `PersistentMemoryScope`
  - `PersistentMemoryPaths`
  - `PersistentMemoryEntry`
  - `PersistentMemoryFrontmatter`
  - `PersistentMemoryIndex`
  - `PersistentMemoryIndexEntry`
  - `PersistentMemoryRecord`
  - `PersistentMemoryTombstone`
  - `PersistentMemoryExtractionState`
  - `MemoryExtractionDecision`
  - `MemoryExtractionCandidate`
  - `MemoryExtractionReason`
  - `MemoryExtractionTrigger`
  - `MemoryExtractionOutcome`
- extend session runtime state with additive extraction bookkeeping such as:
  - last extraction timestamp
  - last extraction outcome
  - configured memory scope
  - extraction in-flight flag or task handle reference
  - last extracted transcript boundary index
  - last explicit remember/forget request detected
- extend persisted session metadata only for restore-relevant extraction state, for example:
  - `PersistedPersistentMemoryState { enabled, scope, last_extracted_message_index, last_extraction_at }`
- store durable memory on disk under harness-managed state using:
  - one markdown file per live durable memory entry
  - a generated `MEMORY.md` index
  - an optional small `index.json` or equivalent machine-readable sidecar for efficient internal rebuilds if Quine needs it
  - tombstone records only if needed to preserve explicit forget semantics across index rebuilds without keeping deleted content live

Planned API changes:

- add crate-private persistent-memory helpers such as:
  - `resolve_persistent_memory_paths(...)`
  - `ensure_persistent_memory_store(...)`
  - `load_memory_index(...)`
  - `rebuild_memory_index(...)`
  - `write_memory_entry(...)`
  - `delete_memory_entry(...)`
  - `tombstone_memory_entry(...)`
  - `load_persistent_memory_state(...)`
  - `store_persistent_memory_state(...)`
  - `extract_persistent_memories_from_turn(...)`
  - `apply_memory_extraction_decisions(...)`
  - `detect_explicit_memory_requests(...)`
- add additive config accessors in `quine-harness` for trusted overrides and feature gating
- add additive internal engine hooks after successful turn completion so extraction can be scheduled without blocking the main reply path
- keep durable memory file management internal; no new shared trait contracts in this feature

Validation:

- unit tests for path resolution, frontmatter parsing, index generation, and explicit remember/forget parsing
- integration test confirming durable memories persist across harness restarts and across new sessions in the same project

Why this feature stands alone:

- it builds the durable store and maintenance pipeline before prompt injection complexity is added
- users can inspect and manually curate memories from disk immediately

### Feature 4: Prompt-time persistent memory injection

This feature surfaces durable memory during prompt construction.

Goal:

- inject stable baseline memory into context safely
- support targeted recall so only the most relevant durable memories are surfaced per turn

Primary crates/files:

- `crates/quine-core/src/engine.rs`
- `crates/quine-core/src/memory/persistent.rs`
- `crates/quine-core/src/memory/diagnostics.rs`
- `crates/quine-cli/src/context_debug.rs`

Concrete work:

- add prompt-construction hooks in the engine that load durable memory before model invocation
- implement two modes, both additive and feature-gated:
  1. baseline index injection via `MEMORY.md`
  2. targeted recall selecting a bounded set of memory files per turn
- implement a first-pass relevant-memory selector using deterministic heuristics before introducing an LLM-based selector:
  - keyword overlap with the latest user message
  - recency and explicit pinning from frontmatter
  - project or agent scope match
- inject selected memories as system-side reminders or structured synthetic messages, keeping ordering stable and explainable
- cap injected memory volume and truncate with explicit diagnostic notes when limits are exceeded
- ensure prompts remind the model that durable memory may be stale and must be verified against the repo or current user instructions

Planned data structure changes:

- add recall-selection internal types, for example:
  - `RelevantMemoryCandidate`
  - `RelevantMemoryMatch`
  - `RelevantMemoryBudget`
  - `RelevantMemorySelection`
  - `PromptMemoryInjection`
  - `PromptMemoryEnvelope`
- extend additive per-turn runtime state with:
  - latest prompt-time memory injection snapshot
  - selected memory ids/paths for the active turn
  - already-injected memory ids for de-dup within the current prompt build
  - truncation reasons and byte/entry counts
  - injection mode used
- extend prompt-construction internals with an explicit pre-invocation memory stage so baseline prefix material and turn-local recalled memories are assembled separately and in a stable order

Planned API changes:

- add crate-private prompt-building helpers such as:
  - `build_memory_injection(...)`
  - `select_relevant_memories(...)`
  - `inject_memory_into_prompt(...)`
  - `build_memory_envelope(...)`
  - `render_memory_reminder_message(...)`
- keep prompt-construction changes internal to `quine-core::engine`
- if surfaced externally, expose only additive diagnostics fields rather than a new prompt-building trait

Validation:

- unit tests for ranking, truncation, and injection ordering
- integration test proving memories influence prompt construction only when enabled
- regression tests ensuring disabled memory mode leaves prompt construction unchanged

Why this feature stands alone:

- it layers recall on top of the durable storage introduced by Feature 3
- targeted recall can ship after a simpler index-only injection mode if needed

### Feature 5: Memory diagnostics and operator visibility

This feature makes the system debuggable before advanced scopes are added.

Goal:

- explain which memory sources were used, updated, skipped, or rejected
- surface enough metadata for CLI users and QA agents to validate behavior

Primary crates/files:

- `crates/quine-core/src/memory/diagnostics.rs`
- `crates/quine-harness/src/protocol.rs`
- `crates/quine-harness/src/storage.rs`
- `crates/quine-cli/src/context_debug.rs`
- `crates/quine-cli/src/render.rs`

Concrete work:

- record per-turn diagnostics including:
  - whether session memory updated
  - current summary path
  - last summarized boundary
  - whether persistent memory index injection ran
  - which durable memory files were selected for targeted recall
  - why files were skipped, truncated, or considered stale
- expose diagnostics through an additive harness/session inspection API or existing context-debug surfaces
- add CLI rendering for a concise memory diagnostics view
- include enough structured data that QA can assert behavior without scraping human-oriented logs

Planned data structure changes:

- add structured diagnostics payloads, for example:
  - `MemoryTurnDiagnostics`
  - `SessionMemoryDiagnostics`
  - `PersistentMemoryDiagnostics`
- extend harness-side session snapshots with additive memory fields instead of replacing existing snapshot models

Planned API changes:

- add additive harness protocol/session inspection fields for memory diagnostics
- add CLI rendering helpers for memory diagnostics views
- keep diagnostics read-only and observational; no mutation API is required in this feature

Validation:

- protocol serialization tests for diagnostics payloads
- integration test covering a turn that updates memory and then inspects diagnostics

Why this feature stands alone:

- it improves operability without changing memory semantics
- it should land before broader rollout so debugging is practical

### Feature 6: Advanced scopes and policy controls

This final feature adds optional scope variants only after the core behaviors are stable.

Goal:

- support team-shared and agent-specific durable memory scopes
- add explicit policy controls and stricter permission boundaries

Primary crates/files:

- `crates/quine-core/src/memory/persistent.rs`
- `crates/quine-harness/src/config.rs`
- `crates/quine-core/src/permission/`
- `crates/quine-cli/src/chat.rs`

Concrete work:

- extend durable-memory resolution to support:
  - project scope as default
  - optional agent scope for custom-agent sessions
  - optional team scope with separate directory roots and index files
- add config and feature gates controlling:
  - auto-memory enablement
  - targeted recall enablement
  - session-memory enablement
  - advanced scope enablement
- ensure memory writes respect workspace trust and configured filesystem policy
- document conflict resolution when the same fact could exist in project, team, and agent scopes
- keep scope precedence simple and explicit in the first release

Planned data structure changes:

- extend `PersistentMemoryScope` to represent:
  - project scope
  - agent scope
  - team scope
- add explicit scope identifiers and resolution models so scope precedence stays deterministic, for example:
  - `ProjectMemoryScope`
  - `AgentMemoryScope`
  - `TeamMemoryScope`
  - `MemoryScopeRef`
  - `ScopedMemoryPaths`
  - `ScopedMemoryResolution`
  - `ScopedMemorySelection`
  - `ScopedMemoryLookupOrder`
- add policy/config types so feature flags, write rules, and trust requirements stay separate from path resolution, for example:
  - `MemoryFeatureFlags`
  - `MemoryPolicyConfig`
  - `MemoryAccessPolicy`
  - `MemoryScopePolicy`
  - `MemoryPermissionContext`
  - `MemoryWritePolicy`
  - `MemoryReadPolicy`
  - `MemoryConflictResolution`
- extend runtime persistent-memory state with resolved scope metadata and active policy snapshots, for example:
  - `PersistentMemoryScopeState`
  - `ResolvedMemoryPolicies`
  - `ScopedPersistentMemoryState`

Planned API changes:

- add additive harness config fields and parsing for memory scope/policy controls
- add internal scope-resolution helpers that compute readable and writable scope sets for a session before prompt injection or extraction starts
- add internal permission/policy helpers for validating memory reads and writes under each scope
- add internal conflict-resolution helpers so extraction and targeted recall can resolve overlapping facts deterministically
- keep scope resolution behind internal helpers; avoid exposing a broad public memory-management API unless later UX work requires it

Validation:

- unit tests for scope resolution and policy gating
- integration test covering project-plus-agent or project-plus-team lookup precedence

Why this feature stands alone:

- advanced scopes are valuable, but they are not required for the first usable memory system in Quine
- deferring them keeps the initial implementation tractable

## Planned Data Structure and API Changes

This section captures the intended Rust-facing shape of the implementation before code exists.

These are planned changes, not locked contracts. They are detailed enough to guide implementation, code review, and QA, while still allowing small naming adjustments during implementation.

### Design rules for data structures

- prefer crate-private structs and enums in `quine-core` unless cross-crate exposure is required
- keep checkpoint data minimal and restore-oriented
- keep large memory payloads on disk instead of embedding them in persisted checkpoints
- use strongly typed enums for scope, status, and source selection instead of stringly typed fields
- add fields conservatively and default them for backward-compatible restore

### Design rules for API changes

- prefer additive helper functions and internal methods over broad public API changes
- keep prompt-construction, summary-refresh, and compaction integration behind internal `quine-core` helpers
- use additive harness protocol fields only for diagnostics or inspection when needed
- avoid changes to shared inter-crate traits unless a dedicated coordination feature is approved

### Phase 0 planned data structures

Planned internal types in `crates/quine-core/src/memory/`:

```rust
struct SessionMemoryPaths {
    directory: PathBuf,
    summary_path: PathBuf,
    metadata_path: PathBuf,
}

struct SessionMemoryState {
    enabled: bool,
    paths: SessionMemoryPaths,
    refresh_in_flight: bool,
    last_summarized_message_index: Option<usize>,
    last_refresh_at: Option<DateTime<Utc>>,
    template_version: u32,
}

struct PersistentMemoryConfig {
    enabled: bool,
    scope: PersistentMemoryScope,
    root_dir: PathBuf,
}

struct MemoryDiagnostics {
    last_session_refresh: Option<SessionMemoryRefreshDiagnostic>,
    last_persistent_extraction: Option<PersistentMemoryExtractionDiagnostic>,
}
```

Planned `SessionContext` additions in `crates/quine-core/src/engine.rs`:

```rust
struct SessionContext {
    // existing fields...
    session_memory: SessionMemoryState,
    persistent_memory: Option<PersistentMemoryConfig>,
    memory_diagnostics: MemoryDiagnostics,
}
```

Planned persisted checkpoint additions in `crates/quine-core/src/persistence.rs`:

```rust
struct PersistedMemoryState {
    session_memory: PersistedSessionMemoryState,
}

struct PersistedSessionMemoryState {
    enabled: bool,
    last_summarized_message_index: Option<usize>,
    template_version: u32,
}
```

Planned API/internal-method changes:

```rust
fn session_memory_paths(state_root: &Path, session_id: SessionId) -> SessionMemoryPaths;
fn restore_memory_state(persisted: Option<&PersistedMemoryState>) -> SessionMemoryState;
fn snapshot_memory_state(state: &SessionMemoryState) -> PersistedMemoryState;
```

Notes:

- the roadmap above and this section use consistent planned module names: `memory/session.rs`, `memory/template.rs`, `memory/summary.rs`, `memory/persistent.rs`, and `memory/diagnostics.rs`
- exact identifiers may still change during implementation, but the separation of responsibilities should remain stable
- `PersistedMemoryState` should be added as an optional/additively defaulted field on `PersistedSession`
- no memory file contents should be embedded in `CoreCheckpoint`
- no shared trait changes are planned in this phase

### Feature 1 planned data structures

Planned summary-file and metadata model:

```rust
struct SessionSummaryMetadata {
    last_summarized_message_index: usize,
    updated_at: DateTime<Utc>,
    template_version: u32,
}

struct SessionSummaryDocument {
    current_state: String,
    task_specification: String,
    files_and_functions: Vec<String>,
    workflow: Vec<String>,
    errors_and_corrections: Vec<String>,
    codebase_and_system_documentation: Vec<String>,
    learnings: Vec<String>,
    key_results: Vec<String>,
    worklog: Vec<String>,
}

struct SessionSummaryUpdate {
    from_message_index: usize,
    to_message_index: usize,
    document: SessionSummaryDocument,
    metadata: SessionSummaryMetadata,
}
```

Planned storage layout under harness-managed state:

```text
<state_dir>/sessions/<session_id>/session-memory/
  summary.md
  summary.meta.json
```

Planned persisted data addition:

```rust
struct PersistedSession {
    // existing fields...
    memory_state: Option<PersistedMemoryState>,
}
```

Planned internal helper APIs:

```rust
fn initialize_summary_if_missing(paths: &SessionMemoryPaths) -> anyhow::Result<()>;
fn load_summary_metadata(path: &Path) -> anyhow::Result<SessionSummaryMetadata>;
fn store_summary_metadata(path: &Path, metadata: &SessionSummaryMetadata) -> anyhow::Result<()>;
fn should_refresh_summary(
    state: &SessionMemoryState,
    history: &[Message],
) -> bool;
fn build_summary_update(
    history: &[Message],
    current: Option<&SessionSummaryDocument>,
    metadata: Option<&SessionSummaryMetadata>,
) -> SessionSummaryUpdate;
async fn refresh_summary_from_history(
    state: &mut SessionMemoryState,
    history: Vec<Message>,
) -> anyhow::Result<()>;
```

Planned engine integration points:

```rust
impl SessionContext {
    fn maybe_schedule_session_memory_refresh(&mut self) -> bool;
    fn mark_session_memory_refresh_started(&mut self);
    fn mark_session_memory_refresh_finished(
        &mut self,
        result: &anyhow::Result<SessionSummaryMetadata>,
    );
}
```

Behavioral API expectations:

- summary refresh is best-effort and asynchronous after a completed turn
- turn completion must not wait on summary-file writes
- missing `summary.md` or `summary.meta.json` is recoverable state
- no new user-facing CLI command or tool API is required in Feature 1

### Feature 2 planned data structures and APIs

Planned compaction integration types:

```rust
enum CompactionSource {
    SessionMemory,
    LegacySummarizer,
}

enum SessionMemoryBoundary {
    Indexed {
        last_summarized_message_index: usize,
    },
    ResumedSession,
}

enum SessionMemoryBoundaryResolution {
    Ready {
        boundary: SessionMemoryBoundary,
        first_unsummarized_message_index: usize,
    },
    MissingSummary,
    TemplateOnly,
    MissingMetadata,
    InvalidBoundary,
    RefreshInFlight,
}

struct SessionMemoryCompactionInput {
    summary_text: String,
    metadata: SessionSummaryMetadata,
    boundary: SessionMemoryBoundary,
}

struct CompactionPlan {
    source: CompactionSource,
    archived_message_count: usize,
    preserved_tail_count: usize,
    summary_message: Message,
    resulting_history: Vec<Message>,
}
```

Planned additive `SessionMemoryState` fields used by compaction in `crates/quine-core/src/memory/session_memory.rs` and `crates/quine-core/src/engine.rs`:

```rust
struct SessionMemoryState {
    enabled: bool,
    paths: SessionMemoryPaths,
    refresh_in_flight: bool,
    last_summarized_message_index: Option<usize>,
    last_compaction_source: Option<CompactionSource>,
    last_compacted_message_index: Option<usize>,
    last_refresh_at: Option<DateTime<Utc>>,
    template_version: u32,
}
```

Planned additive persisted checkpoint fields when resume behavior needs to survive restart:

```rust
struct PersistedSessionMemoryState {
    enabled: bool,
    last_summarized_message_index: Option<usize>,
    last_compaction_source: Option<CompactionSource>,
    last_compacted_message_index: Option<usize>,
    template_version: u32,
}
```

Field intent:

- `last_compaction_source` records whether the previous compaction used session memory or the legacy summarizer so restore/debug paths can explain behavior without rereading archived transcripts
- `last_compacted_message_index` records the highest message index compacted into the live summary message so resumed sessions can avoid re-compacting the same region ambiguously
- `refresh_in_flight` remains transient runtime state and must not block restore if absent in persisted state

Planned internal helper APIs:

```rust
fn load_session_memory_for_compaction(
    state: &SessionMemoryState,
) -> anyhow::Result<Option<SessionMemoryCompactionInput>>;

fn resolve_session_memory_boundary(
    history: &[Message],
    metadata: &SessionSummaryMetadata,
    state: &SessionMemoryState,
) -> SessionMemoryBoundaryResolution;

fn build_session_memory_summary_message(
    summary_text: &str,
    archive_ref: &str,
) -> Message;

fn build_compacted_history_from_session_memory(
    history: &[Message],
    input: &SessionMemoryCompactionInput,
    archive_ref: &str,
) -> anyhow::Result<Vec<Message>>;

fn build_compaction_plan(
    history: &[Message],
    session_memory: Option<&SessionMemoryCompactionInput>,
    archive_ref: &str,
) -> anyhow::Result<CompactionPlan>;

fn apply_compaction_plan(
    history: &mut Vec<Message>,
    state: &mut SessionMemoryState,
    plan: CompactionPlan,
);
```

Planned `quine-core` internal behavior changes:

- `compaction.rs` should start by consulting `SessionContext.session_memory` before invoking the generic summarizer path
- if `refresh_in_flight` is true, compaction should either await the current refresh handle owned by the engine or fail over immediately to `LegacySummarizer`; the first implementation should prefer a bounded wait only if the engine already has a join handle available without changing trait boundaries
- boundary calculation should be index-based against the canonical in-memory transcript ordering already owned by `engine.rs`; Feature 2 should not introduce message-id-based cross-crate contracts
- the session-memory path should only activate when all of the following are true:
  - `summary.md` exists and is not template-only
  - `summary.meta.json` parses successfully
  - the boundary resolves to a safe cut that preserves the unsummarized tail
- if any of those checks fail, `compaction.rs` should fall through to the existing `summarizer_messages()` logic without mutating session-memory state
- when session-memory compaction succeeds, `apply_compaction_plan(...)` should update in-memory session state with:
  - `last_compaction_source = Some(CompactionSource::SessionMemory)`
  - `last_compacted_message_index = Some(...)`
- when fallback compaction succeeds, `apply_compaction_plan(...)` should update:
  - `last_compaction_source = Some(CompactionSource::LegacySummarizer)`
  - `last_compacted_message_index = Some(...)` only if the legacy path already computes a deterministic compacted boundary
- archive creation and transcript replacement should remain owned by the existing compaction flow in `quine-core`; Feature 2 only changes how the replacement summary payload is chosen
- no planned external protocol or CLI API change for this feature

### Feature 3 planned data structures and APIs

Planned persistent-memory model:

```rust
enum PersistentMemoryScope {
    Project {
        project_key: String,
    },
}

struct PersistentMemoryPaths {
    root_dir: PathBuf,
    index_path: PathBuf,
    entries_dir: PathBuf,
    state_path: PathBuf,
    tombstones_dir: PathBuf,
}

struct PersistentMemoryFrontmatter {
    title: String,
    summary: String,
    scope: PersistentMemoryScope,
    memory_kind: PersistentMemoryKind,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    source: MemoryExtractionSource,
    tags: Vec<String>,
    keywords: Vec<String>,
    pinned: bool,
    stale: bool,
}

enum PersistentMemoryKind {
    UserPreference,
    ProjectContext,
    WorkflowRule,
    ExternalReference,
}

enum MemoryExtractionSource {
    ExplicitUserRequest,
    AutomaticExtraction,
    ManualOperatorEdit,
}

struct PersistentMemoryEntry {
    entry_id: String,
    slug: String,
    path: PathBuf,
    frontmatter: PersistentMemoryFrontmatter,
    body_markdown: String,
}

struct PersistentMemoryRecord {
    entry_id: String,
    slug: String,
    relative_path: PathBuf,
    title: String,
    summary: String,
    memory_kind: PersistentMemoryKind,
    updated_at: DateTime<Utc>,
    pinned: bool,
    stale: bool,
}

struct PersistentMemoryIndexEntry {
    entry_id: String,
    title: String,
    relative_path: PathBuf,
    summary: String,
    memory_kind: PersistentMemoryKind,
    updated_at: DateTime<Utc>,
}

struct PersistentMemoryIndex {
    generated_at: DateTime<Utc>,
    scope: PersistentMemoryScope,
    entries: Vec<PersistentMemoryIndexEntry>,
}

struct PersistentMemoryTombstone {
    entry_id: String,
    slug: String,
    deleted_at: DateTime<Utc>,
    reason: MemoryExtractionReason,
}

struct PersistentMemoryExtractionState {
    enabled: bool,
    scope: PersistentMemoryScope,
    paths: PersistentMemoryPaths,
    extraction_in_flight: bool,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
    last_outcome: Option<MemoryExtractionOutcome>,
}

struct MemoryExtractionCandidate {
    title: String,
    summary: String,
    details_markdown: String,
    memory_kind: PersistentMemoryKind,
    reason: MemoryExtractionReason,
    trigger: MemoryExtractionTrigger,
    source_message_indices: Vec<usize>,
    existing_entry_id: Option<String>,
}

enum MemoryExtractionTrigger {
    ExplicitRemember,
    ExplicitForget,
    PostTurnHeuristic,
}

enum MemoryExtractionReason {
    UserRequestedRemember,
    UserRequestedForget,
    DurablePreferenceDetected,
    DurableProjectFactDetected,
    DurableWorkflowRuleDetected,
    ExternalReferenceDetected,
    SupersededByNewerMemory,
    MarkedStale,
}

enum MemoryExtractionDecision {
    Create {
        candidate: MemoryExtractionCandidate,
    },
    Update {
        entry_id: String,
        candidate: MemoryExtractionCandidate,
    },
    Tombstone {
        entry_id: String,
        reason: MemoryExtractionReason,
    },
    Ignore {
        reason: String,
    },
}

enum MemoryExtractionOutcome {
    Skipped {
        reason: String,
    },
    Applied {
        created_entry_ids: Vec<String>,
        updated_entry_ids: Vec<String>,
        tombstoned_entry_ids: Vec<String>,
    },
    Failed {
        error: String,
    },
}

struct PersistedPersistentMemoryState {
    enabled: bool,
    scope: PersistentMemoryScope,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
}
```

Planned additive `SessionContext` and persisted-state changes in `crates/quine-core/src/engine.rs` and `crates/quine-core/src/persistence.rs`:

```rust
struct SessionContext {
    // existing fields...
    persistent_memory: Option<PersistentMemoryExtractionState>,
}

struct PersistedMemoryState {
    session_memory: PersistedSessionMemoryState,
    persistent_memory: Option<PersistedPersistentMemoryState>,
}
```

Planned storage layout under harness-managed state for the Feature 3 project-scoped store:

```text
<state_dir>/memory/projects/<project_key>/
  MEMORY.md
  index.json
  entries/
    <slug>.md
  tombstones/
    <entry_id>.json
```

Field intent:

- `PersistentMemoryScope` remains project-only in Feature 3 even though later features may extend it; this keeps the first durable store scoped narrowly and avoids overlapping with advanced scope work
- `project_key` should be a stable harness-generated identifier derived from the resolved project root rather than a raw user-provided string
- `summary` in frontmatter and index entries exists so `MEMORY.md` can be regenerated deterministically without reparsing full markdown bodies for every update
- `keywords` gives the later recall path a bounded, explicit relevance hint without requiring Feature 4 selection logic yet
- `stale` and tombstones model correction without physically rewriting historical metadata into checkpoints
- `last_extracted_message_index` is the restore boundary for automatic extraction; it prevents duplicate best-effort extraction after restart
- `extraction_in_flight` remains transient runtime state and must not be persisted

Planned internal helper APIs:

```rust
fn resolve_persistent_memory_paths(
    state_root: &Path,
    working_directory: &Path,
    config: &HarnessConfig,
) -> anyhow::Result<PersistentMemoryPaths>;

fn ensure_persistent_memory_store(
    paths: &PersistentMemoryPaths,
    scope: &PersistentMemoryScope,
) -> anyhow::Result<()>;

fn load_memory_index(
    paths: &PersistentMemoryPaths,
) -> anyhow::Result<PersistentMemoryIndex>;

fn load_memory_records(
    paths: &PersistentMemoryPaths,
) -> anyhow::Result<Vec<PersistentMemoryRecord>>;

fn parse_memory_entry(
    path: &Path,
) -> anyhow::Result<PersistentMemoryEntry>;

fn render_memory_entry(
    entry: &PersistentMemoryEntry,
) -> anyhow::Result<String>;

fn write_memory_entry(
    paths: &PersistentMemoryPaths,
    entry: &PersistentMemoryEntry,
) -> anyhow::Result<()>;

fn delete_memory_entry(
    paths: &PersistentMemoryPaths,
    entry_id: &str,
) -> anyhow::Result<()>;

fn tombstone_memory_entry(
    paths: &PersistentMemoryPaths,
    tombstone: &PersistentMemoryTombstone,
) -> anyhow::Result<()>;

fn rebuild_memory_index(
    paths: &PersistentMemoryPaths,
    records: &[PersistentMemoryRecord],
) -> anyhow::Result<PersistentMemoryIndex>;

fn write_memory_index(
    paths: &PersistentMemoryPaths,
    index: &PersistentMemoryIndex,
) -> anyhow::Result<()>;

fn detect_explicit_memory_requests(
    turn_messages: &[Message],
) -> Vec<MemoryExtractionCandidate>;

fn detect_heuristic_memory_candidates(
    turn_messages: &[Message],
    existing_records: &[PersistentMemoryRecord],
) -> Vec<MemoryExtractionCandidate>;

fn decide_memory_extraction_actions(
    candidates: Vec<MemoryExtractionCandidate>,
    existing_records: &[PersistentMemoryRecord],
) -> Vec<MemoryExtractionDecision>;

async fn extract_persistent_memories_from_turn(
    state: &mut PersistentMemoryExtractionState,
    turn_messages: Vec<Message>,
) -> anyhow::Result<MemoryExtractionOutcome>;

async fn apply_memory_extraction_decisions(
    paths: &PersistentMemoryPaths,
    decisions: Vec<MemoryExtractionDecision>,
) -> anyhow::Result<MemoryExtractionOutcome>;

fn load_persistent_memory_state(
    persisted: Option<&PersistedPersistentMemoryState>,
    paths: PersistentMemoryPaths,
    scope: PersistentMemoryScope,
) -> PersistentMemoryExtractionState;

fn snapshot_persistent_memory_state(
    state: &PersistentMemoryExtractionState,
) -> PersistedPersistentMemoryState;
```

Planned `quine-core` internal behavior changes:

- `engine.rs` should resolve project-scoped persistent-memory state during session initialization and store it on `SessionContext`
- after a successful completed turn, `engine.rs` should schedule durable-memory extraction on a best-effort background path similar in shape to session-memory refresh, but independent from prompt construction
- extraction should operate only on the newly completed message range after `last_extracted_message_index` so restart and retry behavior remain conservative
- explicit remember/forget requests should take precedence over heuristic extraction and should suppress duplicate heuristic decisions for the same turn when they overlap
- if the turn already included an internal memory-management write path owned by Quine, automatic extraction should skip the already-handled range to avoid duplicate entries
- index regeneration should be deterministic and owned by `quine-core::memory::persistent_memory`, while actual filesystem root resolution and directory creation remain aligned with `quine-harness` storage helpers
- extraction failures should not fail the user-visible turn; they should update runtime outcome state and return control to the normal session loop
- Feature 3 should not yet alter prompt construction, attachment injection, or relevant-memory selection behavior

Planned `quine-harness` config additions in `crates/quine-harness/src/config.rs`:

```rust
struct MemoryConfig {
    auto_memory_enabled: bool,
    memory_root_override: Option<PathBuf>,
    memory_project_key_override: Option<String>,
    memory_allow_env_override: bool,
    persistent_memory_extraction_enabled: bool,
    persistent_memory_max_decisions_per_turn: usize,
    session_memory_enabled: bool,
}
```

Planned `quine-harness` internal changes in `crates/quine-harness/src/storage.rs` and `crates/quine-harness/src/config.rs`:

- add storage helpers that resolve a project-scoped memory root beneath harness-managed state, for example `storage.memory_project_root(project_key)`
- keep config ownership for trusted root overrides in `quine-harness`; `quine-core` should consume a resolved or validated config view rather than reading environment variables directly
- allow an environment override only through harness config loading for local development and tests; the override should be explicit and default-off in production-like configurations
- provide a stable `project_key` derivation helper based on resolved project root normalization so sessions in the same project share durable memory across restarts
- keep the harness protocol unchanged for Feature 3 unless an existing session snapshot already has an additive place for last extraction outcome; no new mutation API is required

### Feature 4 planned data structures and APIs

Planned recall-selection model:

```rust
enum PromptMemoryInjectionMode {
    Disabled,
    IndexOnly,
    TargetedRecall,
}

struct RelevantMemoryMatch {
    matched_terms: Vec<String>,
    keyword_overlap_count: usize,
    scope_matches_session: bool,
    pinned_boost_applied: bool,
    recency_boost_applied: bool,
}

struct RelevantMemoryCandidate {
    entry_id: String,
    path: PathBuf,
    title: String,
    summary: String,
    memory_kind: PersistentMemoryKind,
    updated_at: DateTime<Utc>,
    pinned: bool,
    stale: bool,
    match_detail: RelevantMemoryMatch,
    score: i64,
    score_reasons: Vec<String>,
}

struct RelevantMemoryBudget {
    max_candidates_scanned: usize,
    max_selected_entries: usize,
    max_total_bytes: usize,
    max_bytes_per_entry: usize,
    max_rendered_lines_per_entry: usize,
}

struct RelevantMemorySelection {
    mode: PromptMemoryInjectionMode,
    candidates_considered: usize,
    selected: Vec<RelevantMemoryCandidate>,
    skipped_stale_entry_ids: Vec<String>,
    skipped_duplicate_entry_ids: Vec<String>,
    truncated_entry_ids: Vec<String>,
    truncated_bytes: usize,
}

struct PromptMemoryEnvelope {
    baseline_prefix_markdown: Option<String>,
    reminder_messages: Vec<Message>,
    selected_entry_ids: Vec<String>,
    selected_paths: Vec<PathBuf>,
}

struct PromptMemoryInjection {
    mode: PromptMemoryInjectionMode,
    baseline_prefix_markdown: Option<String>,
    injected_messages: Vec<Message>,
    selected_entry_ids: Vec<String>,
    selected_paths: Vec<PathBuf>,
    truncation_notes: Vec<String>,
}
```

Planned additive per-turn runtime state in `crates/quine-core/src/engine.rs`:

```rust
struct SessionContext {
    // existing fields...
    last_prompt_memory_injection: Option<PromptMemoryInjection>,
}
```

Planned additive runtime state in `crates/quine-core/src/memory/persistent_memory.rs`:

```rust
struct PersistentMemoryExtractionState {
    enabled: bool,
    scope: PersistentMemoryScope,
    paths: PersistentMemoryPaths,
    extraction_in_flight: bool,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
    last_outcome: Option<MemoryExtractionOutcome>,
    prompt_injection_mode: PromptMemoryInjectionMode,
    prompt_budget: RelevantMemoryBudget,
    last_selected_entry_ids: Vec<String>,
}
```

Field intent:

- `PromptMemoryInjectionMode::Disabled` lets prompt construction explicitly represent the no-injection path without overloading `Option` in every internal helper
- `RelevantMemoryCandidate` uses stable entry metadata rather than loading full markdown bodies for all records during ranking; body reads should happen only after selection
- `RelevantMemoryMatch` keeps heuristic ranking explainable and testable without introducing an LLM selector in Feature 4
- `RelevantMemoryBudget` centralizes prompt-budget caps so index injection and targeted recall use the same bounded accounting model
- `PromptMemoryEnvelope` separates baseline prefix material from turn-local reminder messages because those attach at different points in prompt construction
- `last_prompt_memory_injection` is per-turn runtime state only and should not be persisted in checkpoints
- `last_selected_entry_ids` is transient de-dup bookkeeping for repeated prompt builds within the same live session

Planned internal helper APIs:

```rust
fn load_memory_index_markdown(
    paths: &PersistentMemoryPaths,
) -> anyhow::Result<Option<String>>;

fn load_memory_records_for_recall(
    paths: &PersistentMemoryPaths,
) -> anyhow::Result<Vec<PersistentMemoryRecord>>;

fn extract_latest_user_text(
    history: &[Message],
) -> Option<&str>;

fn select_relevant_memories(
    latest_user_text: &str,
    records: &[PersistentMemoryRecord],
    scope: &PersistentMemoryScope,
    budget: &RelevantMemoryBudget,
    already_selected_entry_ids: &[String],
) -> RelevantMemorySelection;

fn score_memory_candidate(
    latest_user_text: &str,
    record: &PersistentMemoryRecord,
    scope: &PersistentMemoryScope,
) -> Option<RelevantMemoryCandidate>;

fn read_selected_memory_entries(
    paths: &PersistentMemoryPaths,
    selection: &RelevantMemorySelection,
    budget: &RelevantMemoryBudget,
) -> anyhow::Result<Vec<PersistentMemoryEntry>>;

fn build_memory_envelope(
    mode: PromptMemoryInjectionMode,
    index_markdown: Option<String>,
    selected_entries: &[PersistentMemoryEntry],
    selection: &RelevantMemorySelection,
) -> PromptMemoryEnvelope;

fn render_memory_reminder_message(
    entry: &PersistentMemoryEntry,
    truncated: bool,
) -> Message;

fn build_memory_injection(
    envelope: PromptMemoryEnvelope,
    selection: &RelevantMemorySelection,
) -> PromptMemoryInjection;

fn inject_memory_into_prompt(
    history: &[Message],
    injection: &PromptMemoryInjection,
) -> Vec<Message>;
```

Planned engine changes:

- add an additive prompt-construction step before LLM invocation
- keep prompt mutation internal to `engine.rs`
- do not add a new prompt-builder trait in this feature
- split prompt construction into three ordered internal stages:
  1. build the normal baseline prompt prefix
  2. derive `PromptMemoryInjection` from persistent-memory state and the latest user turn
  3. splice memory output into the final request with baseline index material remaining in prefix position and targeted recall material inserted as synthetic system-side reminder messages before the newest user message
- when `PromptMemoryInjectionMode::IndexOnly` is active, load `MEMORY.md` into the same baseline prompt material path already used for other instruction-like context rather than emitting extra transcript messages
- when `PromptMemoryInjectionMode::TargetedRecall` is active, omit broad `MEMORY.md` injection by default and instead attach selected durable memories as bounded reminder messages so turn-local recall does not permanently alter the session transcript
- if no latest user text is available or no memory record scores above the minimum threshold, targeted recall should yield an empty `PromptMemoryInjection` rather than falling back implicitly to index injection
- reminder message ordering should be deterministic: highest score first, ties broken by `pinned`, then most recent `updated_at`, then `entry_id`
- prompt construction should add a short stale-memory reminder alongside injected memory content telling the model to verify durable facts against the current repository and current user instructions before relying on them
- prompt construction should overwrite `SessionContext.last_prompt_memory_injection` on every model invocation so downstream diagnostics can describe the exact memory payload used for that turn
- Feature 4 should not yet expose a user-facing prompt-editing command or mutate shared harness/SDK prompt-building contracts

### Feature 5 planned data structures and APIs

Planned diagnostics model:

```rust
enum MemoryDiagnosticStatus {
    Disabled,
    Idle,
    Succeeded,
    Skipped,
    Failed,
}

enum MemoryDiagnosticSource {
    SessionMemoryRefresh,
    SessionMemoryCompaction,
    PersistentMemoryIndexInjection,
    PersistentMemoryTargetedRecall,
    PersistentMemoryExtraction,
}

enum MemorySkipReason {
    FeatureDisabled,
    NoEligibleMessages,
    NoRelevantEntries,
    NoStateAvailable,
    BudgetExceeded,
    AlreadyUpToDate,
    PermissionDenied,
}

struct MemoryDiagnosticFailure {
    source: MemoryDiagnosticSource,
    message: String,
}

struct SessionMemoryDiagnostics {
    status: MemoryDiagnosticStatus,
    summary_path: PathBuf,
    metadata_path: Option<PathBuf>,
    refreshed_this_turn: bool,
    compacted_this_turn: bool,
    last_summarized_message_index: Option<usize>,
    last_compacted_message_index: Option<usize>,
    last_updated_at: Option<DateTime<Utc>>,
    skip_reason: Option<MemorySkipReason>,
    failure: Option<MemoryDiagnosticFailure>,
}

struct PersistentMemoryInjectedEntryDiagnostics {
    entry_id: String,
    path: PathBuf,
    title: String,
    injection_source: MemoryDiagnosticSource,
    truncated: bool,
    score: Option<i64>,
    score_reasons: Vec<String>,
}

struct PersistentMemorySkippedEntryDiagnostics {
    entry_id: String,
    path: Option<PathBuf>,
    reason: String,
}

struct PersistentMemoryExtractionDiagnostics {
    status: MemoryDiagnosticStatus,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
    created_entry_ids: Vec<String>,
    updated_entry_ids: Vec<String>,
    tombstoned_entry_ids: Vec<String>,
    skip_reason: Option<MemorySkipReason>,
    failure: Option<MemoryDiagnosticFailure>,
}

struct PersistentMemoryDiagnostics {
    injection_status: MemoryDiagnosticStatus,
    injection_mode: PromptMemoryInjectionMode,
    index_loaded: bool,
    selected_entries: Vec<PersistentMemoryInjectedEntryDiagnostics>,
    skipped_entries: Vec<PersistentMemorySkippedEntryDiagnostics>,
    total_selected_bytes: usize,
    truncation_notes: Vec<String>,
    extraction: Option<PersistentMemoryExtractionDiagnostics>,
}

struct MemoryTurnDiagnostics {
    turn_id: String,
    session_memory: SessionMemoryDiagnostics,
    persistent_memory: PersistentMemoryDiagnostics,
}
```

Planned additive runtime state in `crates/quine-core/src/engine.rs`:

```rust
struct SessionContext {
    // existing fields...
    last_memory_diagnostics: Option<MemoryTurnDiagnostics>,
}
```

Planned additive persisted/session-snapshot model changes in `crates/quine-core/src/persistence.rs` and the harness protocol types:

```rust
struct PersistedSessionMemoryState {
    // existing fields...
    last_updated_at: Option<DateTime<Utc>>,
    last_compacted_message_index: Option<usize>,
}

struct SessionContextSnapshot {
    // existing fields...
    memory_diagnostics: Option<MemoryTurnDiagnostics>,
}
```

Field intent:

- `MemoryDiagnosticStatus` keeps operator-facing state explicit without overloading booleans such as `refreshed` or `index_loaded` to also mean skipped or failed
- `MemoryDiagnosticSource` gives a stable cross-crate vocabulary for diagnostics rendering, snapshot serialization, and future filtering without changing memory behavior
- `MemorySkipReason` constrains common skip cases to a testable enum while still allowing finer per-entry `reason` strings for targeted recall details
- `SessionMemoryDiagnostics` focuses on the latest observed session-memory maintenance state for the current turn rather than attempting to expose full historical logs
- `PersistentMemoryInjectedEntryDiagnostics` carries enough metadata for operators to understand why a memory was injected without reparsing markdown files from the CLI or SDK
- `PersistentMemoryExtractionDiagnostics` mirrors the additive outcome structure already planned for extraction state so diagnostics and internal bookkeeping stay aligned
- `last_memory_diagnostics` is transient per-session runtime state and should be overwritten on each completed turn or prompt build; it should not become an append-only history buffer
- `last_updated_at` and `last_compacted_message_index` are additive persisted references so restored sessions can describe current session-memory boundaries even before another refresh occurs

Planned internal helper APIs:

```rust
fn build_session_memory_diagnostics(
    state: &SessionMemoryState,
    refreshed_this_turn: bool,
    compacted_this_turn: bool,
    failure: Option<MemoryDiagnosticFailure>,
) -> SessionMemoryDiagnostics;

fn build_persistent_memory_extraction_diagnostics(
    state: &PersistentMemoryExtractionState,
) -> Option<PersistentMemoryExtractionDiagnostics>;

fn build_persistent_memory_diagnostics(
    state: &PersistentMemoryExtractionState,
    injection: Option<&PromptMemoryInjection>,
    selection: Option<&RelevantMemorySelection>,
) -> PersistentMemoryDiagnostics;

fn build_memory_turn_diagnostics(
    turn_id: String,
    session_memory: SessionMemoryDiagnostics,
    persistent_memory: PersistentMemoryDiagnostics,
) -> MemoryTurnDiagnostics;

fn snapshot_memory_diagnostics(
    diagnostics: Option<&MemoryTurnDiagnostics>,
) -> Option<MemoryTurnDiagnostics>;
```

Planned additive harness protocol exposure in `crates/quine-harness/src/protocol.rs` and `crates/quine-sdk`:

```rust
struct GetSessionRequest {
    session_id: SessionId,
    include_memory_diagnostics: bool,
}

struct ListSessionsRequest {
    include_memory_diagnostics: bool,
}

struct SessionSummary {
    // existing fields...
    memory_diagnostics: Option<MemoryTurnDiagnostics>,
}
```

Planned read-only diagnostics API surface:

- add additive `memory_diagnostics` fields to existing session inspection responses when `include_memory_diagnostics` is `true`
- keep diagnostics read-only and derived from current runtime state plus persisted memory boundaries; Feature 5 does not add any write, retry, clear, or override operation
- expose the same additive diagnostics payload through `quine-sdk` session inspection helpers rather than introducing a memory-specific client abstraction in this phase
- allow harness implementations to omit diagnostics when the session is not found or the caller does not request them, preserving existing inspection defaults and payload sizes
- add CLI rendering helpers that print a compact operator view for session memory path, last summarized and compacted boundaries, prompt injection mode, selected persistent memory entries, and the latest extraction outcome
- keep all diagnostics observational: they must never trigger a memory refresh, extraction pass, compaction, or filesystem read beyond normal snapshot assembly

### Feature 6 planned data structures and APIs

Planned scope/policy model:

```rust
enum PersistentMemoryScope {
    Project(ProjectMemoryScope),
    Agent(AgentMemoryScope),
    Team(TeamMemoryScope),
}

struct ProjectMemoryScope {
    project_key: String,
}

struct AgentMemoryScope {
    project_key: String,
    agent_key: String,
}

struct TeamMemoryScope {
    team_key: String,
}

enum MemoryScopeRef<'a> {
    Project(&'a ProjectMemoryScope),
    Agent(&'a AgentMemoryScope),
    Team(&'a TeamMemoryScope),
}

struct MemoryFeatureFlags {
    session_memory_enabled: bool,
    session_memory_compaction_enabled: bool,
    auto_memory_enabled: bool,
    relevant_memory_enabled: bool,
    team_memory_enabled: bool,
    agent_memory_enabled: bool,
    memory_policy_enforcement_enabled: bool,
}

struct MemoryReadPolicy {
    allow_project_scope: bool,
    allow_agent_scope: bool,
    allow_team_scope: bool,
    allow_cross_scope_recall: bool,
}

struct MemoryWritePolicy {
    allow_project_writes: bool,
    allow_agent_writes: bool,
    allow_team_writes: bool,
    require_trusted_workspace_for_writes: bool,
    require_explicit_user_intent_for_team_writes: bool,
    require_explicit_user_intent_for_agent_writes: bool,
}

struct MemoryScopePolicy {
    read_policy: MemoryReadPolicy,
    write_policy: MemoryWritePolicy,
    default_write_scope: PersistentMemoryScope,
    lookup_order: ScopedMemoryLookupOrder,
    conflict_resolution: MemoryConflictResolution,
}

struct MemoryAccessPolicy {
    persistent_memory: MemoryScopePolicy,
}

struct MemoryPolicyConfig {
    flags: MemoryFeatureFlags,
    root_override: Option<PathBuf>,
    team_root_override: Option<PathBuf>,
    agent_root_override: Option<PathBuf>,
    policy: MemoryAccessPolicy,
}

struct ScopedMemoryPaths {
    scope: PersistentMemoryScope,
    paths: PersistentMemoryPaths,
}

struct ScopedMemoryResolution {
    writable_scope: Option<ScopedMemoryPaths>,
    readable_scopes: Vec<ScopedMemoryPaths>,
    lookup_order: ScopedMemoryLookupOrder,
}

struct ScopedMemorySelection {
    selected_scope: PersistentMemoryScope,
    fallback_scopes: Vec<PersistentMemoryScope>,
    readable_scopes: Vec<PersistentMemoryScope>,
}

enum ScopedMemoryLookupOrder {
    ProjectOnly,
    ProjectThenAgent,
    ProjectThenTeam,
    ProjectThenAgentThenTeam,
    ProjectThenTeamThenAgent,
    AgentThenProject,
    TeamThenProject,
}

enum MemoryConflictResolution {
    PreferNarrowerScope,
    PreferBroaderScope,
    PreferMostRecentlyUpdated,
    ErrorOnConflictingWrites,
}

struct MemoryPermissionContext {
    workspace_is_trusted: bool,
    explicit_user_memory_intent: bool,
    active_agent_key: Option<String>,
    active_team_key: Option<String>,
}

struct PersistentMemoryScopeState {
    default_scope: PersistentMemoryScope,
    resolved_scopes: ScopedMemoryResolution,
    policy: MemoryScopePolicy,
}

struct ResolvedMemoryPolicies {
    read_policy: MemoryReadPolicy,
    write_policy: MemoryWritePolicy,
}

struct ScopedPersistentMemoryState {
    active_scope: PersistentMemoryScope,
    readable_scopes: Vec<PersistentMemoryScope>,
    writable_scope: Option<PersistentMemoryScope>,
    resolved_policies: ResolvedMemoryPolicies,
}
```

Planned additive persistent-memory model changes in `crates/quine-core/src/memory/persistent_memory.rs`:

```rust
struct PersistentMemoryRecord {
    entry_id: String,
    slug: String,
    relative_path: PathBuf,
    title: String,
    summary: String,
    scope: PersistentMemoryScope,
    memory_kind: PersistentMemoryKind,
    updated_at: DateTime<Utc>,
    pinned: bool,
    stale: bool,
}

struct PersistentMemoryIndex {
    generated_at: DateTime<Utc>,
    scope: PersistentMemoryScope,
    entries: Vec<PersistentMemoryIndexEntry>,
}

struct PersistentMemoryExtractionState {
    enabled: bool,
    scope: PersistentMemoryScope,
    paths: PersistentMemoryPaths,
    scoped_state: Option<ScopedPersistentMemoryState>,
    extraction_in_flight: bool,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
    last_outcome: Option<MemoryExtractionOutcome>,
    prompt_injection_mode: PromptMemoryInjectionMode,
    prompt_budget: RelevantMemoryBudget,
    last_selected_entry_ids: Vec<String>,
}

struct PersistedPersistentMemoryState {
    enabled: bool,
    scope: PersistentMemoryScope,
    readable_scopes: Vec<PersistentMemoryScope>,
    writable_scope: Option<PersistentMemoryScope>,
    last_extracted_message_index: Option<usize>,
    last_extraction_at: Option<DateTime<Utc>>,
}
```

Planned additive harness config changes in `crates/quine-harness/src/config.rs`:

```rust
struct MemoryConfig {
    auto_memory_enabled: bool,
    memory_root_override: Option<PathBuf>,
    memory_project_key_override: Option<String>,
    memory_allow_env_override: bool,
    persistent_memory_extraction_enabled: bool,
    persistent_memory_max_decisions_per_turn: usize,
    session_memory_enabled: bool,
    relevant_memory_enabled: bool,
    team_memory_enabled: bool,
    team_memory_root_override: Option<PathBuf>,
    team_memory_default_team_key: Option<String>,
    agent_memory_enabled: bool,
    agent_memory_root_override: Option<PathBuf>,
    memory_policy_enforcement_enabled: bool,
    require_trusted_workspace_for_memory_writes: bool,
    require_explicit_user_intent_for_team_memory_writes: bool,
    require_explicit_user_intent_for_agent_memory_writes: bool,
    cross_scope_recall_enabled: bool,
    cross_scope_lookup_order: ScopedMemoryLookupOrder,
    conflict_resolution: MemoryConflictResolution,
}
```

Planned storage layout under harness-managed state for advanced scopes:

```text
<state_dir>/memory/
  projects/<project_key>/
    MEMORY.md
    index.json
    entries/
  teams/<team_key>/
    MEMORY.md
    index.json
    entries/
  agents/<project_key>/<agent_key>/
    MEMORY.md
    index.json
    entries/
```

Field intent:

- `PersistentMemoryScope` becomes the stable scope discriminator for extraction, prompt-time recall, diagnostics, and policy enforcement, replacing the temporary project-only assumption from Feature 3
- `ProjectMemoryScope`, `AgentMemoryScope`, and `TeamMemoryScope` keep scope keys explicit so path resolution and diagnostics do not rely on ad hoc string parsing
- `MemoryPolicyConfig` owns trusted configuration and policy defaults in `quine-harness`; `quine-core` should consume a validated snapshot rather than reading raw environment variables or CLI flags directly
- `MemoryReadPolicy` and `MemoryWritePolicy` separate prompt-time visibility rules from mutation rules so cross-scope recall can be enabled without also enabling cross-scope writes
- `ScopedMemoryLookupOrder` makes first-release precedence explicit and testable instead of relying on filesystem iteration order or implicit `Option` priority
- `MemoryConflictResolution` gives extraction and recall a deterministic tie-break rule when equivalent facts exist across project, team, and agent scopes
- `MemoryPermissionContext` captures the minimum runtime facts needed for write authorization without introducing a new shared trait contract
- `ScopedPersistentMemoryState` is runtime-only resolved state and should be rebuilt on session restore rather than treated as the source of truth over config
- `PersistedPersistentMemoryState` stores only restore-relevant scope references so sessions can resume with the same active scope defaults even when no new extraction has run yet

Planned helper APIs:

```rust
fn resolve_scoped_memory_paths(
    state_root: &Path,
    config: &MemoryPolicyConfig,
    working_directory: &Path,
    project_key: &str,
    agent_key: Option<&str>,
    team_key: Option<&str>,
) -> anyhow::Result<ScopedMemoryResolution>;

fn resolve_default_memory_scope(
    resolution: &ScopedMemoryResolution,
    policy: &MemoryScopePolicy,
) -> anyhow::Result<PersistentMemoryScope>;

fn resolve_memory_scope_selection(
    resolution: &ScopedMemoryResolution,
    policy: &MemoryScopePolicy,
) -> ScopedMemorySelection;

fn resolve_memory_read_paths(
    selection: &ScopedMemorySelection,
    resolution: &ScopedMemoryResolution,
) -> Vec<PersistentMemoryPaths>;

fn resolve_memory_write_path(
    selection: &ScopedMemorySelection,
    resolution: &ScopedMemoryResolution,
) -> Option<PersistentMemoryPaths>;

fn build_memory_permission_context(
    workspace_is_trusted: bool,
    explicit_user_memory_intent: bool,
    agent_key: Option<&str>,
    team_key: Option<&str>,
) -> MemoryPermissionContext;

fn validate_memory_read_scope(
    policy: &MemoryScopePolicy,
    scope: &PersistentMemoryScope,
) -> anyhow::Result<()>;

fn validate_memory_write_scope(
    policy: &MemoryScopePolicy,
    scope: &PersistentMemoryScope,
    context: &MemoryPermissionContext,
) -> anyhow::Result<()>;

fn authorize_memory_read(
    policy: &MemoryScopePolicy,
    scope: &PersistentMemoryScope,
) -> bool;

fn authorize_memory_write(
    policy: &MemoryScopePolicy,
    scope: &PersistentMemoryScope,
    context: &MemoryPermissionContext,
) -> bool;

fn resolve_memory_conflict(
    policy: &MemoryScopePolicy,
    candidates: &[PersistentMemoryRecord],
) -> Option<PersistentMemoryRecord>;

fn load_scoped_memory_records(
    resolution: &ScopedMemoryResolution,
    policy: &MemoryScopePolicy,
) -> anyhow::Result<Vec<PersistentMemoryRecord>>;

fn extract_persistent_memories_for_scope(
    state: &mut PersistentMemoryExtractionState,
    scope: &PersistentMemoryScope,
    turn_messages: Vec<Message>,
    permission_context: &MemoryPermissionContext,
) -> anyhow::Result<MemoryExtractionOutcome>;
```

Planned `quine-core` internal behavior changes:

- resolve scoped memory roots during session initialization after project resolution and before prompt-building state is finalized
- derive a default writable scope plus an ordered readable scope list once per session, then reuse that resolved state for extraction, index loading, and targeted recall
- keep first-release lookup precedence explicit and stable:
  - project-only when advanced scopes are disabled
  - project plus agent when agent memory is enabled for a custom-agent session
  - project plus team when team memory is enabled and a team key is configured
  - project plus agent plus team only when cross-scope recall is explicitly enabled
- default write behavior should remain conservative:
  - project scope remains the default write target unless policy selects a narrower scope
  - agent and team writes must pass policy checks and may require explicit remember/forget intent
  - denied writes should fail safe, emit diagnostics, and never silently fall through to a broader scope without an explicit policy rule
- prompt-time recall should read from all policy-authorized readable scopes in configured lookup order, then de-duplicate or resolve conflicts before ranking
- extraction should write into exactly one authorized target scope per decision; Feature 6 should not create mirrored copies of the same fact across multiple scopes automatically
- when the same durable fact exists in multiple scopes, conflict handling should be deterministic and diagnostic-friendly:
  - prefer the configured `MemoryConflictResolution` rule
  - surface the winning scope in diagnostics
  - avoid deleting or rewriting lower-priority entries unless an explicit forget or update operation targets that scope
- scope resolution and policy checks should stay internal to `quine-core::memory` and `quine-core::permission`; they should not require changes to shared `Tool`, `Agent`, `Dispatcher`, `HarnessService`, or `LlmProvider` traits
- policy enforcement should align with existing workspace-trust and filesystem-permission models already owned by Quine rather than introducing a parallel authorization framework

Planned external/API impact:

- additive harness config fields and parsing for scope roots, scope flags, lookup order, and write-policy requirements
- additive CLI/session startup wiring so custom-agent sessions can pass a resolved `agent_key` into internal memory scope resolution without creating a new user-facing memory command surface
- additive diagnostics exposure may include resolved readable scopes, writable scope, and denied-scope reasons when Feature 5 diagnostics are enabled
- still no broad public memory-management API planned

### Recommended implementation order

Recommended PR order:

1. Phase 0 groundwork
2. Feature 1: session memory foundation
3. Feature 2: session-memory compaction
4. Feature 3: persistent memory store and extraction
5. Feature 4: prompt-time persistent recall
6. Feature 5: diagnostics and visibility
7. Feature 6: advanced scopes and policy controls

This order intentionally delivers compaction continuity before durable recall. That matches Quine’s current architecture, where transcript history and compaction are already first-class concepts in `quine-core`.

### Cross-cutting design decisions

These decisions should remain consistent across all phases:

- keep memory formats human-readable on disk using markdown plus small machine-readable metadata
- prefer additive internal structs over changes to shared inter-crate traits
- persist references and boundaries, not redundant transcript snapshots inside checkpoints
- serialize all memory writes per session or per scope to avoid races
- fail open for reads and fail safe for writes:
  - missing memory should not break chat
  - invalid or denied writes should emit diagnostics and continue
- preserve current behavior behind feature gates until each stage is validated

### Concrete testing strategy for the roadmap

Each feature should include:

- unit tests in the owning module for parsing, ranking, truncation, and fallback logic
- crate-level integration tests in `crates/quine-core/tests/` or `crates/quine-harness/tests/` for end-to-end session behavior
- at least one local-daemon QA scenario once prompt injection or compaction behavior changes
- checkpoint/restore coverage for any persisted memory metadata

CI gates for any implementation PR should include:

- `cargo build`
- `cargo test`
- `cargo clippy --all-targets -- -D warnings`
- `cargo fmt --all -- --check`

## Recommendations for Future Work

Potential improvements:

- stronger observability for "why this memory was injected"
- explicit UI surfacing of whether current turn used index injection or targeted recall
- better reconciliation tooling for stale durable memories
- clearer user-facing distinction between durable memory and session continuity memory
- unified diagnostics view showing:
  - loaded `claudeMd` sources
  - surfaced relevant memories
  - current session-memory path
  - last summarized message id

## Final Takeaway

The architecture is intentionally split:

- persistent memory captures what should be true beyond this conversation
- session memory captures what must remain coherent within this conversation

That split is what makes the overall chat system workable under long-running, tool-heavy coding sessions:

- durable memory gives continuity across sessions
- session memory gives continuity across compaction

Both are required. Neither should absorb the role of the other.
