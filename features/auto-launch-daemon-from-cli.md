---
status: done
---

# Auto-Launch Daemon from CLI Chat Mode

## Overview

When a user runs `quine chat` (or `quine run`) and no daemon is currently running, the CLI should automatically spawn the daemon in the background rather than failing with a connection error. This removes the need to manually run `quine-harness start` before using the CLI.

## Requirements

### Detection

- Before connecting to the Unix domain socket, check if the socket file exists and is accepting connections.
- If the socket doesn't exist or connection is refused, the daemon is not running.

### Auto-Launch

- Spawn `quine-harness start` as a background process (detached from the CLI's process group).
- Wait for the socket to become available (poll with short backoff, timeout after ~5 seconds).
- If the daemon fails to start within the timeout, print an error and exit.
- Print a brief message to stderr: `Starting daemon...` so the user knows what's happening.

### Lifecycle

- The auto-launched daemon continues running after the CLI exits (it is a background daemon, not a child process).
- Users can still stop it explicitly with `quine daemon stop`.
- If the daemon is already running, the CLI connects directly with no extra output.

## Acceptance Criteria

- `quine chat` works without a pre-started daemon — it launches one automatically.
- `quine run "message"` also auto-launches if needed.
- If the daemon is already running, no extra process is spawned.
- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
