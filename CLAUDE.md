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

## The Bootstrapping Contract

This project has a unique constraint: **the agent using this CLAUDE.md must be capable of building the entire project from this specification.** When adding a new crate, module, or capability:

1. Update this file to describe it.
2. Ensure the description is precise enough that an agent reading only this file and the existing code could implement it correctly.
3. The conversation log of that implementation becomes a replay artifact that can reproduce the change.

The goal is a fixed point: the agent, guided by this file, produces a codebase that defines an agent that, guided by this file, would produce the same codebase.

## License

Dual-licensed under MIT OR Apache-2.0. Every `Cargo.toml` should specify `license = "MIT OR Apache-2.0"`.
