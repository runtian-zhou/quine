---
status: in-progress
---

# Implement quine-harness and quine-cli Crates

## Overview

Build the harness daemon (quine-harness) and CLI frontend (quine-cli) crates, completing the full dependency chain: `quine-cli -> quine-harness -> quine-core -> quine-llm`.

## Requirements

### quine-harness Crate

1. **`service.rs`** — `HarnessService` async trait with methods: `create_session`, `send_message`, `submit_tool_result`, `cancel`, `shutdown`, `subscribe`
2. **`local.rs`** — `LocalHarness` implementation that spawns `run_core_loop`, holds `HarnessHandle`, fans out events via `tokio::sync::broadcast`
3. **`server.rs`** — IPC server on Unix domain socket with JSON-RPC 2.0 over newline-delimited JSON
4. **`protocol.rs`** — JSON-RPC request/response/notification types
5. **`config.rs`** — `SessionConfig`, `HarnessConfig`, socket path helpers
6. **`error.rs`** — `HarnessError` enum with thiserror
7. **`lib.rs`** — re-exports
8. **Binary target** — `quine-harness` with `start` command

### quine-cli Crate

1. **`main.rs`** — clap 4 with derive macros, subcommands: `chat`, `daemon start`, `daemon stop`, `version`
2. **`client.rs`** — IPC client connecting to Unix domain socket, JSON-RPC
3. **`chat.rs`** — interactive REPL: reads stdin, sends messages, prints streaming deltas, handles /quit and Ctrl-D
4. **`render.rs`** — `Renderer` trait + `TerminalRenderer` (writes to stdout)

### Integration

- Update workspace `Cargo.toml` members
- `quine-harness` depends on `quine-core` and `quine-llm`
- `quine-cli` depends on `quine-harness` (for shared protocol types)
- Binary name for CLI: `quine`

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- Unit tests for protocol serialization, config, and service trait
- Tool requests automatically replied with stub error
