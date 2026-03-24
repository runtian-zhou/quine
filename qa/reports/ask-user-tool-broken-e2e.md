# QA Report: ask_user Tool — Broken End-to-End Flow

**Date**: 2026-03-24
**Severity**: High
**Component**: ask_user tool (quine-core, quine-harness, quine-cli)

## Summary

The `ask_user` tool is correctly implemented at the core level but has no functioning end-to-end path. When the LLM invokes `ask_user`, the tool will hang indefinitely because the interaction response can never reach it from the CLI.

## Root Cause Analysis

The interaction flow has **four breaks** in the chain:

### 1. `HarnessService` trait missing `submit_interaction_response` method

**File**: `crates/quine-harness/src/service.rs`

The trait defines `submit_tool_result` but has no method for submitting interaction responses. The `LocalHarness` impl therefore has no way to forward a user's answer back to the core.

### 2. `LocalHarness` has no way to forward `InteractionResponse` to core

**File**: `crates/quine-harness/src/local.rs`

Even though `CoreInput::InteractionResponse` exists in the channel types, `LocalHarness` never sends it. The `harness_input` sender could be used, but there's no public method exposing this.

### 3. No JSON-RPC handler for interaction responses in server

**File**: `crates/quine-harness/src/server.rs`

The server converts `InteractionNeeded` core events into JSON-RPC notifications (line 480-490), but there's no corresponding RPC method handler for the client to send back the user's response. The `methods` module in `protocol.rs` has no `SUBMIT_INTERACTION_RESPONSE` constant.

### 4. CLI does not handle `interaction_needed` notifications

**File**: `crates/quine-cli/src/chat.rs` (line 135: `_ => Ok(false)`)
**File**: `crates/quine-cli/src/run.rs` (line 120: `_ => {}`)

Both the interactive chat and one-shot mode silently ignore `interaction_needed` notifications. The chat REPL should prompt the user for input; the one-shot mode should either auto-cancel or error.

## Expected Behavior

1. LLM calls `ask_user` with a question
2. Core emits `CoreOutput::InteractionNeeded` with the prompt
3. Harness broadcasts it → Server sends `interaction_needed` notification to CLI
4. CLI (chat mode) displays the prompt and reads user input from stdin
5. CLI sends the response back via a new RPC method (e.g., `submit_interaction_response`)
6. Server receives it → calls `HarnessService::submit_interaction_response`
7. `LocalHarness` forwards `CoreInput::InteractionResponse` to the core
8. Core relays it to the blocked `ask_user` tool via the `InteractionChannel`
9. Tool completes, result goes back to LLM

## Actual Behavior

Steps 1-3 work. At step 4, the notification is silently dropped. The `ask_user` tool hangs forever waiting on `InteractionChannel::ask()`.

## Reproduction

Any LLM prompt that triggers the agent to use `ask_user` will hang. This cannot be reliably triggered with a fixed test case since it depends on LLM behavior, but the code path is verifiable by inspection.

## Fix Applied

All four breaks were fixed:

1. **`protocol.rs`**: Added `SUBMIT_INTERACTION_RESPONSE` RPC method constant
2. **`service.rs`**: Added `submit_interaction_response(session_id, response)` to `HarnessService` trait
3. **`local.rs`**: Implemented the method — forwards `CoreInput::InteractionResponse` to core
4. **`server.rs`**: Added RPC handler that parses `session_id` + `response` and calls the service method
5. **`chat.rs`**: Added `handle_interaction()` — displays `[ask_user]` prompt, reads stdin, sends response via RPC
6. **`run.rs`**: Handles `interaction_needed` by auto-responding with "user not available" message (one-shot mode can't prompt)

**Status**: RESOLVED
