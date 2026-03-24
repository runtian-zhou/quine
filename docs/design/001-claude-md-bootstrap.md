# Design Doc 001: CLAUDE.md Bootstrap

## Conversation Transcript

The following is the conversation between the user and Claude Code that produced this design.

---

**User:**

Iterate on the CLAUDE.md for this engineering project quine.

Quine is designed to be an open source agent harness framework. The name quine comes from the idea that this project will mostly be implemented by the harness itself, just like a compiler bootstraps itself. This recursive self-reflectness would be the north star of this project, to be able to iterate on itself by using the development process of this project.

**Claude Code:** [Explored the codebase — found the project is a Rust workspace with prior architecture of 4 crates (quine-core, quine-llm, quine-cli, quine-eval), all code recently removed for a fresh start. CI/CD workflows remain.]

**Claude Code:** Since all code was removed for a fresh start, should the CLAUDE.md describe the previous architecture as the target to rebuild, or be more open-ended?

**User:** I'll describe the high level structure of the project:

1. quine-cli: the cli that renders the agent harness to user, it needs a both interactive and non-interactive mode.
2. quine-core: the agent harness library that orchestrate agent execution.
3. quine-harness: a local daemon service that wraps the harness in the quine-core. quine-cli will need to be interacting with this local service.
4. quine-llm: an llm adapter that can adapt multiple different llm provider sources into one unified interface

**Claude Code:** What's the motivation for quine-harness as a separate daemon?

**User:** Both multi-session/background agent support AND shared state coordination.

**Claude Code:** Should the CLAUDE.md include the previous eval crate (quine-eval)?

**User:** Drop for now. Focus on the 4 crates described.

**User:** Engineering Requirement: use traits as interfaces between function modules. Make sure traits are highly abstracted out and only the main agent will be able to modify trait.

**Claude Code:** When you say "only the main agent will be able to modify trait", do you mean trait definitions live exclusively in quine-core?

**User:** Each crate defines its own traits for its domain, but only quine-core can modify the core orchestration traits (Tool, LlmProvider, etc.).

**User:** Make sure commit the design doc separately in a local folder for future references. At the beginning of the design doc, also attach the conversation with claude code for the feature request from users.

**Claude Code:** Where should design docs live? What format for the conversation?

**User:** docs/design/ + full transcript. Separate commits (design doc first, then CLAUDE.md).

---

## Design: CLAUDE.md Bootstrap Specification

### Motivation

Quine's codebase was reset to a clean slate. The CLAUDE.md file is the seed of the project's self-bootstrapping loop — it must contain enough information for an AI agent to reconstruct and iterate on the entire project. This design doc captures the architectural decisions that inform that file.

### Architecture: 4-Crate Workspace

```
quine/
  Cargo.toml          # workspace root
  crates/
    quine-cli/        # CLI frontend
    quine-core/       # agent harness library
    quine-harness/    # local daemon service
    quine-llm/        # LLM provider adapter
```

**Dependency flow:** `quine-cli` -> `quine-harness` (via IPC/API) -> `quine-core` -> `quine-llm`

#### quine-cli
- CLI frontend with interactive (REPL) and non-interactive modes
- Communicates with the harness daemon — does not run the agent loop directly
- Responsible for terminal rendering, user input, markdown display

#### quine-core
- Agent harness library — the heart of the system
- Orchestrates agent execution: state machine, tool dispatch, conversation management, permissions, replay
- Owns the core orchestration traits (`Tool`, `Agent`, `Dispatcher`)

#### quine-harness
- Local daemon service wrapping quine-core
- Manages multiple concurrent agent sessions and background agents
- Provides shared state coordination: permissions, conversation logs, tool registries
- The CLI connects to this service

#### quine-llm
- LLM provider adapter
- Unified interface over multiple providers (Anthropic, OpenAI, etc.)
- Streaming support
- Defines and owns the `LlmProvider` trait

### Engineering Principle: Traits as Interfaces

Core design rule: **use traits as the interface contracts between modules**.

- Each crate defines highly abstract traits for its own domain
- `quine-core`: core orchestration traits (`Tool`, `Agent`, `Dispatcher`) — only the main agent modifies these
- `quine-llm`: `LlmProvider` trait — unified adapter interface
- `quine-harness`: `HarnessService` / session management traits
- `quine-cli`: `Renderer` / UI traits

Principles:
- Traits are the API boundaries between crates. Concrete types stay crate-private where possible.
- Traits should be highly abstract — express *what*, not *how*.
- Other crates implement traits but do not modify the core orchestration traits defined in quine-core.
- Depend on trait abstractions, not concrete implementations (dependency inversion).

### The Bootstrapping Contract

This project has a unique constraint: the agent using the CLAUDE.md must be capable of building the entire project from that specification. When adding a new crate, module, or capability:

1. Update CLAUDE.md to describe it
2. Ensure the description is precise enough for reconstruction
3. The conversation log of implementation becomes a replay artifact

The goal is a fixed point: agent + CLAUDE.md -> codebase -> agent that would produce the same codebase.

### Decisions Made

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Crate count | 4 (cli, core, harness, llm) | Eval dropped for now; harness separated for multi-session + shared state |
| Daemon architecture | quine-harness as local daemon | Multi-session support, background agents, shared state coordination |
| Trait ownership | Each crate owns its domain traits; core orchestration traits in quine-core | Abstraction at boundaries, dependency inversion, controlled modification |
| CLI-to-harness communication | IPC/API (not in-process) | CLI is a thin client; agent loop runs in daemon |
| Rust toolchain | 1.90.0 | Pinned in CI, no nightly features |
| License | MIT OR Apache-2.0 | Dual license for maximum compatibility |
