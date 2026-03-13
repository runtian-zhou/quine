# Quine

[![CI](https://github.com/runtian-zhou/quine/actions/workflows/ci.yml/badge.svg)](https://github.com/runtian-zhou/quine/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE-MIT)

A self-bootstrapping CLI assistant. Quine records its own construction conversation as a JSON log, then can replay that log to rebuild itself from scratch — like a compiler bootstrapping itself.

## Features

- **Interactive chat** with LLM-powered coding assistance
- **File tools** — Read, Write, Edit, Glob, Grep
- **Streaming** — token-by-token output with syntax highlighting
- **Multi-provider** — Anthropic and OpenAI support
- **Conversation logging** — every session saved as structured JSON
- **Replay engine** — re-execute a recorded session, detect drift
- **Self-bootstrap** — replay the construction log to rebuild from scratch
- **Eval harness** — run test suites against Quine with file checks and LLM-as-judge scoring

## Quick Start

```bash
# Build
cargo build

# Start an interactive chat
export ANTHROPIC_API_KEY="your-key"
cargo run --bin quine -- chat

# Non-interactive (single prompt)
cargo run --bin quine -- chat -p "Create hello.py that prints Hello World"

# Use OpenAI instead
export OPENAI_API_KEY="your-key"
cargo run --bin quine -- chat --provider openai --model gpt-4o
```

## Replay

Every chat session is saved to `.quine/logs/`. Replay re-executes tool calls without any LLM API calls and detects drift between recorded and actual output.

```bash
# Replay a session
cargo run --bin quine -- replay .quine/logs/20260312_100000.json

# Dry run (show what would happen without executing)
cargo run --bin quine -- replay --dry-run .quine/logs/20260312_100000.json

# Strict mode (abort on any output difference)
cargo run --bin quine -- replay --strict .quine/logs/20260312_100000.json
```

## Eval

Run test suites against Quine with file checks and optional LLM-as-judge scoring.

```bash
# Generate an example test suite
cargo run --bin quine-eval -- init eval_suite.json

# Run the eval
cargo run --bin quine-eval -- run eval_suite.json

# Skip LLM judge (file checks + timing only)
cargo run --bin quine-eval -- run eval_suite.json --skip-judge

# View past results
cargo run --bin quine-eval -- show .quine/eval/eval_20260312_100000.json
```

Test suites are JSON files with test cases:

```json
{
  "name": "My Tests",
  "tests": [
    {
      "id": "create-file",
      "description": "Create a Python script",
      "prompt": "Create hello.py that prints 'Hello, World!'",
      "setup_files": [],
      "expected_files": [
        { "path": "hello.py", "contains": "Hello, World!" }
      ],
      "eval_criteria": [
        "Code is correct and runnable"
      ]
    }
  ]
}
```

Each test runs Quine in an isolated temp directory, checks expected files, and optionally sends results to an LLM judge that scores 0–10.

## Architecture

```
quine/
  crates/
    quine-core/       Core types, tools, replay engine
      conversation.rs   Message, ToolCall, ToolOutput, Entry
      log.rs            ConversationLog (versioned JSON)
      replay.rs         ReplayEngine with drift detection
      prompt.rs         CLAUDE.md discovery + system prompt
      tool/             Read, Write, Edit, Glob, Grep
    quine-llm/        LLM provider abstraction
      provider.rs       LlmProvider async trait
      types.rs          Unified request/response types
      anthropic/        Anthropic API client (streaming + non-streaming)
      openai/           OpenAI API client (streaming + non-streaming)
    quine-cli/        Binary entry point
      main.rs           clap CLI (chat, replay subcommands)
      interactive.rs    Conversation loop + print mode
      render.rs         Markdown + syntax highlighting
    quine-eval/       Eval harness
      harness.rs        Orchestrates both CLIs in temp dirs
      judge.rs          LLM-as-judge evaluation
      report.rs         Terminal summary + scoreboard
```

## Testing

```bash
cargo test
```

## JSON Log Format

```json
{
  "version": 1,
  "created_at": "2026-03-12T10:00:00Z",
  "model": "claude-sonnet-4-20250514",
  "provider": "anthropic",
  "system_prompt": "...",
  "entries": [
    { "type": "UserMessage", "content": "..." },
    { "type": "AssistantMessage", "content": "...", "tool_calls": [...] },
    { "type": "ToolExecution", "tool_call_id": "...", "tool_name": "Write", "arguments": {...}, "result": { "success": true, "output": "..." } }
  ]
}
```
