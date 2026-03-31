# 036 Add `quine-sdk` Harness Connection Interface — QA Plan

Short summary: Verify that the new `crates/quine-sdk` crate provides a reliable Rust-first client interface for connecting to `quine-harness` over the existing Unix domain socket JSON-RPC transport, while keeping the feature limited to connection and interface behavior.

## Open Questions

- None. The user clarified that this feature adds a new `crates/quine-sdk` crate, targets a Rust API first, uses the existing Unix domain socket transport, and is intentionally scoped to the interface/connection layer only.

## Agreement Status

agreed — reviewed the latest implementation-plan revision; the crate scope, Unix socket transport, optional raw-request seam, and validation expectations are aligned, and there are no unresolved planning questions.

## Test Strategy

- Validate the SDK at three layers so failures are easy to localize:
  - focused `quine-sdk` unit tests for config defaults, public error mapping, and any internal protocol/framing helpers
  - crate integration tests against a temporary local Unix socket server that deliberately exercises success and failure paths
  - at least one daemon-backed acceptance path using real `quine-harness` transport behavior
- Keep QA aligned with the intentionally narrow scope:
  - prove the SDK can establish a connection, report connected state if exposed, and disconnect/close cleanly
  - prove it reports transport, early-close, and malformed-payload failures predictably without hanging
  - if a raw JSON-RPC send/receive primitive is included, prove exactly one harmless end-to-end request succeeds
- Treat API-boundary review as part of QA, not just runtime testing:
  - confirm the public surface remains Rust-first, small, and connection-oriented
  - confirm transport internals such as `tokio::net::UnixStream`, split halves, or background task handles are not exposed publicly
  - confirm existing inter-crate traits in `quine-core`, `quine-harness`, and `quine-llm` are not modified for this feature
- Do not require a broad typed RPC surface, C ABI work, or a new transport in this feature.
- Prefer real local daemon coverage over pure mocks for the main acceptance path, while allowing a local protocol-compatible socket server for deterministic error injection.

## Scenarios

- **Scenario 1: SDK crate unit tests**
  - Start/use: run the new crate’s focused tests.
  - Command: `cargo test -p quine-sdk`
  - Exact validation: unit tests pass for configuration defaults, socket-path handling, public error mapping for I/O and JSON failures, and any helper introduced for framing or protocol adaptation.
  - Expected result: the crate’s targeted tests pass without relying on unrelated workspace behavior.

- **Scenario 2: Happy-path local socket connect and clean close**
  - Start/use: run a targeted `quine-sdk` integration test against a temporary local Unix socket server that accepts one client and performs the minimum valid handshake or idle connection behavior required by the SDK.
  - Command: `cargo test -p quine-sdk connect_and_close_cleanly -- --nocapture`
  - Exact validation: the SDK connects successfully, any exposed connection-state API reflects a live connection, and `close`/disconnect returns promptly without panic, task leak symptoms, or double-close failure surprises.
  - Expected result: the primary connection lifecycle works cleanly on a local socket.

- **Scenario 3: Missing socket path connection failure**
  - Start/use: run a targeted `quine-sdk` test against a nonexistent Unix socket path.
  - Command: `cargo test -p quine-sdk connect_fails_for_missing_socket`
  - Exact validation: attempting to connect to a path with no listening daemon returns an SDK connection error variant rather than panicking or surfacing only an opaque transport error.
  - Expected result: the test passes and the error is stable enough for SDK consumers to handle.

- **Scenario 4: Early server disconnect handling**
  - Start/use: run a targeted integration test where a temporary Unix socket server accepts the connection and then closes immediately before any valid response is produced.
  - Command: `cargo test -p quine-sdk early_disconnect -- --nocapture`
  - Exact validation: the SDK surfaces a deterministic disconnect/broken-connection error and does not hang waiting for bytes that will never arrive.
  - Expected result: premature daemon shutdown or peer reset is handled predictably.

- **Scenario 5: Malformed server response handling**
  - Start/use: run a targeted `quine-sdk` integration test that starts a temporary Unix socket server which accepts a client and returns malformed JSON or truncates a frame.
  - Command: `cargo test -p quine-sdk malformed_response -- --nocapture`
  - Exact validation: after the SDK connects and reads the bad payload, it returns a protocol/framing error variant instead of hanging indefinitely or misclassifying the failure as a successful disconnect.
  - Expected result: malformed daemon responses are surfaced cleanly.

- **Scenario 6: Real daemon connection smoke test**
  - Start/use: start the local daemon on a temporary socket, preferably from test setup if practical, otherwise via a documented manual step.
  - Command to start daemon: `cargo run --bin quine-harness -- --socket /tmp/quine-sdk.sock`
  - Command to exercise the SDK: `cargo test -p quine-sdk real_daemon_connect -- --nocapture`
  - Exact validation: the integration test connects to `/tmp/quine-sdk.sock`, confirms the SDK reports a live connection if such state is public, and then closes the connection cleanly.
  - Expected result: the SDK can establish and tear down a real connection to `quine-harness` over the existing Unix domain socket transport.

- **Scenario 7: Real daemon raw request smoke test**
  - Start/use: only if the implementation includes a raw JSON-RPC request/response primitive.
  - Command to start daemon: `cargo run --bin quine-harness -- --socket /tmp/quine-sdk.sock`
  - Command to exercise the SDK: `cargo test -p quine-sdk real_daemon_create_session -- --nocapture`
  - Exact validation: the test sends a single harmless JSON-RPC request through the SDK, preferably `create_session`, with exact request semantics equivalent to:
    - request method: `create_session`
    - request params: `{}`
  - Expected response: a JSON-RPC success payload containing a `session_id` string and no `error` object.
  - Expected result: the SDK’s optional raw request seam proves the connection abstraction can successfully exchange one real harness request without broadening the feature into a full typed RPC client.

- **Scenario 8: Public API scope review**
  - Start/use: inspect `crates/quine-sdk/src/lib.rs` exports and any public items reachable from the crate root.
  - Command: `rg "^pub " crates/quine-sdk/src && cargo doc -p quine-sdk --no-deps`
  - Exact validation: the public API remains limited to client/config/error/connection-facing types; no public typed RPC method set appears; transport internals and `tokio` stream types remain crate-private.
  - Expected result: the crate boundary matches the feature scope and future-extension intent described in the implementation plan.

## Required Evidence

- Passing `quine-sdk` tests covering:
  - connect and clean close success
  - missing-socket failure
  - early peer disconnect
  - malformed response or truncated frame handling
- Evidence of at least one daemon-backed Unix socket connection test using the real `quine-harness` binary or a test setup that exercises the same daemon transport path.
- If a raw request primitive exists, one passing end-to-end request/response test against the real daemon and evidence that no additional typed RPC helpers were added in this feature.
- Passing workspace quality gates after the crate is added:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`
- Repository inspection evidence that:
  - the public SDK surface is small and connection-oriented rather than a sprawling typed API
  - existing inter-crate traits were not modified to accommodate the SDK
  - `CLAUDE.md` and workspace manifests document the new crate clearly enough for future agents to discover it

## Implementation Feedback

- The implementation plan is aligned with QA on scope: Rust-first API, new `crates/quine-sdk` crate, existing Unix domain socket transport, and connection/interface-only behavior.
- QA will explicitly require both deterministic local-socket failure injection tests and at least one daemon-backed acceptance path so the feature is validated without depending only on mocks.
- QA will treat a raw JSON-RPC request/response primitive as optional but, if present, will require exactly one harmless end-to-end exchange and will reject expansion into multiple typed harness methods.
- QA will inspect the final crate root exports to ensure transport internals stay private and the public API remains centered on client/config/error/lifecycle concerns.
- QA will verify documentation updates in `CLAUDE.md` and workspace manifests as part of feature acceptance.
