---
status: pending
---

# Implement quine-llm Adapter and Integrate with quine-core

## Overview

Build the quine-llm crate with a unified `LlmProvider` trait and concrete adapters for both remote and local LLM providers, then wire it into quine-core's event loop so that `UserMessage` inputs produce real LLM responses.

## Requirements

### quine-llm Crate

1. **`LlmProvider` trait** — async trait that abstracts over any LLM backend:
   - `send(messages, tools) -> stream of LlmEvent`
   - `LlmEvent` enum: `TextDelta(String)`, `ToolCall { tool_use_id, tool_name, arguments }`, `Done`
   - Provider-agnostic message types: `Message { role, content }`, `ToolDefinition { name, description, parameters }`

2. **Anthropic adapter** — implements `LlmProvider` for the Anthropic Messages API:
   - Streaming via SSE (`text/event-stream`)
   - Model configurable (e.g., `claude-sonnet-4-20250514`)
   - API key from `ANTHROPIC_API_KEY` env var
   - Base URL configurable via `ANTHROPIC_BASE_URL` env var (default `https://api.anthropic.com`)

3. **OpenAI-compatible adapter** — implements `LlmProvider` for any OpenAI-compatible API:
   - This covers local models (LM Studio, ollama, vLLM, etc.) that expose an OpenAI-compatible endpoint
   - Streaming via SSE
   - Model, base URL, and optional API key all configurable
   - **Must work with**: local Qwen 3.5 at `http://127.0.0.1:1234` (LM Studio OpenAI-compatible endpoint)

4. **Provider configuration**:
   - Enum or builder to select provider: `anthropic`, `openai-compatible`
   - Config struct with: `provider`, `model`, `base_url`, `api_key`, `max_tokens`

### quine-core Integration

5. **Inject `LlmProvider` into the core event loop**:
   - `run_core_loop` takes a `Box<dyn LlmProvider>` (or generic `impl LlmProvider`)
   - On `UserMessage`: build message history, call provider, stream `LlmEvent`s, emit `CoreOutput::StreamDelta` / `CoreOutput::ToolRequest` / `CoreOutput::TextComplete` / `CoreOutput::TurnComplete`
   - On `ToolResult`: append tool result to conversation, call provider again (agent loop)

6. **Conversation history**: `SessionContext` maintains a `Vec<Message>` per session, appending user messages, assistant responses, and tool results.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- Unit tests for message serialization and provider config
- Integration test (can be `#[ignore]`): send a message to the local Qwen endpoint at `http://127.0.0.1:1234` and receive a streamed response
- The existing quine-core channel tests continue to pass
- A simple end-to-end test: create session → send user message → receive streamed text → turn complete

## Local Model Setup

LM Studio is running locally with Qwen 3.5 exposed at:
- **URL**: `http://127.0.0.1:1234`
- **API**: OpenAI-compatible (`/v1/chat/completions`)
- **No API key required**
