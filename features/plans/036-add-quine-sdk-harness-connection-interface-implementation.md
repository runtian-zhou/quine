# 036 Add `quine-sdk` Harness Connection Interface — Implementation Plan

Short summary: Add a new `crates/quine-sdk` crate that exposes a first Rust API for establishing and managing a client connection to `quine-harness` over the existing Unix domain socket JSON-RPC transport, while keeping this feature scoped to the connection/interface layer rather than broad request coverage.

## Open Questions

- None. The user clarified that this feature should introduce a new `crates/quine-sdk` crate, use a Rust API first rather than a true C ABI, reuse the existing Unix domain socket transport, and stay limited to connection/interface concerns.

## Agreement Status

agreed — reviewed the latest QA plan revision, the connection-only scope and validation approach are aligned, and there are no unresolved open questions at planning time.

## Proposed Design

- Add a new crate `crates/quine-sdk` as the dedicated client SDK surface for external Rust consumers of `quine-harness`.
- Keep the first public API Rust-native but intentionally shaped for future FFI evolution:
  - favor opaque client and connection-state types over transport internals
  - keep public request/response values generic and serializable rather than introducing a large typed RPC model
  - avoid exposing `tokio::net::UnixStream`, split reader/writer handles, or transport task management through the public API
- Scope the first iteration strictly to connection establishment and lifecycle:
  - construct a client from a Unix socket path or small connection config type
  - open a connection to a running `quine-harness` daemon over the existing Unix domain socket JSON-RPC transport
  - expose connection state and graceful disconnect/close behavior
  - optionally expose one narrow raw JSON-RPC request/response primitive only if needed to prove the connection abstraction is usable end-to-end
- Keep the raw request primitive, if included, explicitly non-ergonomic and non-expansive:
  - accept generic JSON-RPC method/params or raw serializable payloads
  - return raw JSON success/error payloads or a thin SDK wrapper
  - do not add typed methods for `create_session`, session management, or other harness operations in this feature
- Reuse existing protocol definitions where practical instead of forking wire contracts:
  - prefer depending on existing shared JSON-RPC or harness protocol types if they are already reusable without introducing reverse dependencies
  - if direct reuse would create an awkward dependency or leak server-side details, add a tiny SDK-local adapter layer that preserves wire compatibility while keeping the SDK boundary client-oriented
- Prefer trait-based abstraction at the `quine-sdk` boundary, consistent with `CLAUDE.md`:
  - define SDK-owned abstractions around transport or connection behavior only if they improve testability and future extensibility
  - keep the concrete Unix socket transport implementation crate-private unless a public constructor boundary genuinely requires otherwise
  - avoid changing shared inter-crate traits in `quine-core`, `quine-harness`, or `quine-llm`
- Keep dependency direction aligned with the workspace architecture:
  - `quine-sdk` may depend on lower-level protocol or utility crates needed to speak to the daemon
  - existing crates should not gain dependencies on `quine-sdk`
  - the feature should not require restructuring the daemon or introducing a new transport
- Update `CLAUDE.md` to document the new crate and its narrow purpose so future agents can discover it and extend it safely.
- Document future evolution explicitly: this Rust-first SDK is a stepping stone toward a future C ABI or broader SDK surface, but ABI work and broad typed RPC coverage are out of scope here.

## File-by-File Changes

- `Cargo.toml`
  - Add `crates/quine-sdk` to the workspace members without changing existing dependency direction between current crates.
- `crates/quine-sdk/Cargo.toml`
  - Add a new crate manifest with workspace-consistent metadata, license, edition, and only the dependencies needed for async Unix socket JSON-RPC connectivity, serialization, and error handling.
- `crates/quine-sdk/src/lib.rs`
  - Re-export the intentionally small public surface.
  - Keep the public API centered on a client type, connection configuration, connection status/lifecycle methods, and crate-specific errors.
- `crates/quine-sdk/src/client.rs`
  - Implement the main opaque SDK client type.
  - Provide async constructors for connecting from a socket path or config.
  - Own connection lifecycle and, if included, the single raw request/response seam.
- `crates/quine-sdk/src/config.rs`
  - Add a minimal connection configuration type.
  - Limit options to values justified by the connection layer, such as socket path and any narrowly needed timeout behavior.
- `crates/quine-sdk/src/error.rs`
  - Define SDK-local errors with `thiserror` for connection failure, disconnect state, serialization/protocol issues, and other consumer-visible failures.
- `crates/quine-sdk/src/transport.rs`
  - Keep Unix domain socket transport details internal to the crate.
  - Add transport helpers or internal traits only if they materially improve testability without widening the public API.
- `crates/quine-sdk/src/protocol.rs`
  - Add only a lightweight adapter layer if needed to bridge SDK-facing request/response handling to existing JSON-RPC wire types.
  - Avoid duplicating server-side protocol definitions unless reuse is impractical.
- `crates/quine-sdk/tests/connection.rs`
  - Add integration tests for successful connect/disconnect against a local Unix socket server or daemon-compatible harness surface.
  - Cover missing-socket and malformed-response failures.
  - If a raw request seam exists, add one harmless end-to-end JSON-RPC exchange test only.
- `CLAUDE.md`
  - Update workspace structure and crate responsibilities to describe `quine-sdk` as a Rust-first client SDK for connecting to `quine-harness`.

## Validation Plan

- Add unit tests in `quine-sdk` for:
  - connection configuration defaults or parsing, if applicable
  - SDK error mapping for missing sockets, broken pipes, and malformed JSON
  - any internal framing or protocol adapter helpers that are introduced
- Add integration tests that start a temporary local Unix socket server and verify:
  - successful connection establishment
  - clean disconnect without hanging or panicking
  - predictable error when the socket path does not exist
  - predictable error when the server sends malformed JSON or closes early after connection establishment
- Add at least one daemon-backed acceptance test path using the real `quine-harness` transport behavior, either by launching the daemon binary in test setup or by exercising an equivalent local harness server that speaks the same wire protocol.
- If the final public API includes a raw JSON-RPC send/receive primitive, add exactly one focused end-to-end test that sends one harmless request such as `create_session` and asserts a valid JSON-RPC success response.
- Confirm by repository inspection that the public SDK surface remains small and connection-oriented rather than becoming a broad typed RPC client.
- Run targeted crate checks first:
  - `cargo test -p quine-sdk`
- Run workspace quality gates before handoff because the feature adds a new crate and updates workspace-level documentation:
  - `cargo build`
  - `cargo test`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo fmt --all -- --check`

## QA Feedback

- The implementation plan is well-scoped and QA agrees with the core boundaries: new `crates/quine-sdk` crate, Rust-first consumer API, existing Unix domain socket JSON-RPC transport, and connection/interface-only behavior.
- Please keep the validation plan explicit about three distinct failure modes at the SDK boundary: missing socket path, early peer disconnect after accept, and malformed/truncated JSON-RPC payloads. Those are the highest-value lifecycle regressions for this slice.
- Please preserve at least one happy-path local socket integration test in addition to the daemon-backed test so connection lifecycle behavior can be validated deterministically without depending only on live-daemon orchestration.
- If the optional raw request primitive is included, QA will require exactly one harmless end-to-end request/response test and will treat additional typed convenience methods as out-of-scope expansion.
- QA will inspect the final public exports to confirm transport internals remain crate-private and that the crate root exposes only a small client/config/error/lifecycle surface.
- QA will also verify that no shared inter-crate trait contracts are changed to make this feature work and that `CLAUDE.md` documents `quine-sdk` clearly enough for future agents to discover and extend safely.
