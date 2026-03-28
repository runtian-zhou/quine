# CLAUDE.md

> **You are building the tool that you are.**
> Quine is a self-bootstrapping agent harness: an AI coding agent whose codebase
> is constructed and maintained by an AI coding agent. Like a compiler that
> compiles itself, this project is implemented by the harness it defines.
> This file is the seed of that recursion.

## Build & Test

```bash
cargo build                                # build all crates
cargo test                                 # run all tests
cargo clippy --all-targets -- -D warnings  # lint (CI treats warnings as errors)
cargo fmt --all -- --check                 # format check
cargo run --bin quine -- chat              # run interactive CLI session
```

Rust toolchain: **1.90.0** (pinned in CI). Do not introduce nightly features.

## Workspace Structure

```
Cargo.toml          # workspace root, resolver = "2"
crates/
  quine-cli/        # CLI frontend
  quine-core/       # agent harness library
  quine-harness/    # local daemon service
  quine-llm/        # LLM provider adapter
features/           # feature request markdown files
```

### Crate Responsibilities

| Crate | Purpose |
|-------|---------|
| **quine-cli** | CLI frontend with interactive (REPL) and non-interactive modes. Thin client that communicates with the harness daemon — does not run the agent loop directly. |
| **quine-core** | Agent harness library. Orchestrates agent execution: state machine, tool dispatch, conversation management, permissions, replay. Owns core orchestration traits. |
| **quine-harness** | Local daemon service wrapping quine-core. Manages multiple concurrent agent sessions, background agents, and shared state (permissions, logs, tool registries). |
| **quine-llm** | LLM provider adapter. Unified interface over multiple providers (Anthropic, OpenAI, etc.) with streaming support. |

**Dependency flow:** `quine-cli` -> `quine-harness` (via IPC/API) -> `quine-core` -> `quine-llm`

## Engineering Principles — Traits as Interfaces

Core design rule: **use traits as the interface contracts between modules.** Each crate defines highly abstract traits for its own domain:

- **quine-core**: core orchestration traits (`Tool`, `Agent`, `Dispatcher`) — only the main agent modifies these
- **quine-llm**: `LlmProvider` trait — unified adapter interface
- **quine-harness**: `HarnessService` / session management traits
- **quine-cli**: `Renderer` / UI traits

Rules:
- Traits are the API boundaries between crates. Concrete types stay crate-private where possible.
- Traits should be highly abstract — express *what*, not *how*.
- Other crates implement traits but do not modify the core orchestration traits defined in quine-core.
- Depend on trait abstractions, not concrete implementations (dependency inversion).

## Code Conventions

- **Error handling**: `anyhow::Result` for application code, `thiserror` for library error types.
- **Async runtime**: tokio. All I/O is async.
- **Serialization**: serde + serde_json for all data interchange.
- **CLI parsing**: clap 4 with derive macros.
- **Tool pattern**: Each tool implements the `Tool` trait. One file per tool under `quine-core/src/tool/`.
- **Skill discovery**: Load native Quine skills from `.quine/skills/` and preserve compatibility with legacy `.claude/commands/` markdown prompts plus Codex-style `<skill>/SKILL.md` directories.
- **Tests**: Unit tests in the same file (`#[cfg(test)] mod tests`). Integration tests in `crates/<crate>/tests/`.
- **Naming**: snake_case for files/modules, PascalCase for types. No abbreviations in public APIs.
- **Visibility**: Minimize `pub`. Expose crate APIs through `lib.rs` re-exports.
- **Clippy**: Zero warnings policy. Fix all clippy lints before committing.

## Development Workflow

1. Work on a feature branch (kebab-case, e.g., `add-streaming-support`).
2. All changes go through PRs to `main`.
3. CI must pass: check, test, clippy (-D warnings), fmt.
4. Commits co-authored: `Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>`
5. Keep PRs focused — one logical change per PR.
6. Design docs go in `docs/design/` with conversation transcripts attached.
7. At the end of each implementation, use the built-in `/review` skill to review the code, then create a PR and merge once all code is reviewed and CI passes.

### Parallel Agent Work

Multiple agents can work independently on the same crate, **as long as the trait interfaces between crates are not changed.** Inter-crate trait definitions (e.g., `Tool`, `LlmProvider`, `HarnessService`) are shared contracts — modifying them requires coordination and should be done in a dedicated PR before parallel work begins. Internal implementation changes within a crate are safe for parallel work.

### Feature Requests

Feature requests live in `features/` as Markdown files. Each file describes one feature:

```
features/
  add-streaming-support.md
  tool-permission-system.md
  ...
```

**Format** for a feature request file:

```markdown
---
status: pending | in-progress | done
---

# Feature Title

Description of the feature, requirements, and acceptance criteria.
```

The `status` field tracks progress:
- `pending` — not yet started
- `in-progress` — an agent is actively working on it
- `done` — implemented and merged

Each feature request should also have two planning docs under `features/plans/`:
- `<NNN>-<name>-implementation.md` — implementation plan owned by an implementor agent
- `<NNN>-<name>-qa.md` — QA plan owned by a QA agent

When creating a new feature request, spawn those two agents with empty context. They may inspect the repo independently, but the only information they share with each other must flow through the two plan docs. Do not commit the feature request PR until both plan docs record mutual agreement and all open questions are resolved.

**Agent workflow for feature requests:**

1. Draft the feature request in `features/` and the two planning docs in `features/plans/`.
2. Spawn an implementor agent and a QA agent with empty context.
3. Have the implementor own the implementation plan and review the QA plan.
4. Have the QA agent own the QA plan and review the implementation plan.
5. Do not create the feature-request PR until both planning docs record agreement.
6. Scan `features/` for files with `status: pending`.
7. Pick a feature, update its status to `in-progress`.
8. Create a git worktree (`git worktree add`) for an isolated workspace.
9. Implement the feature in the worktree on a feature branch.
10. Run all checks (`cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`).
11. Use `/review` to review the code, create a PR, and merge when CI passes.
12. Run `/qa` to verify the new feature works end-to-end (spawns daemon, sends messages, checks output).
13. Update the feature file status to `done`.
14. Clean up the worktree (`git worktree remove`).

### QA Reports

QA reports live in `qa/reports/` as timestamped Markdown files. After triaging and fixing failures from a QA report, mark the report as **resolved** by adding a status line at the top (below the title):

```markdown
**Status**: resolved — fix merged in PR #<number>, verified in `qa-<timestamp>`
```

This makes it easy to distinguish actionable reports from already-addressed ones.

## The Bootstrapping Contract

This project has a unique constraint: **the agent using this CLAUDE.md must be capable of building the entire project from this specification.** When adding a new crate, module, or capability:

1. Update this file to describe it.
2. Ensure the description is precise enough that an agent reading only this file and the existing code could implement it correctly.
3. The conversation log of that implementation becomes a replay artifact that can reproduce the change.

The goal is a fixed point: the agent, guided by this file, produces a codebase that defines an agent that, guided by this file, would produce the same codebase.

## License

Dual-licensed under MIT OR Apache-2.0. Every `Cargo.toml` should specify `license = "MIT OR Apache-2.0"`.
