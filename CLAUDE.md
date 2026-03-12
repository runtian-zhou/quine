# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build & Test Commands

- Build: `cargo build`
- Run: `cargo run -- chat` or `cargo run -- replay <log-file>`
- Test: `cargo test`
- Lint: `cargo clippy`

## Architecture

Quine is a self-bootstrapping CLI: it records its own construction conversation as JSON, then can replay that log to rebuild itself from scratch.

### Crate Structure

- **quine-core** (`crates/quine-core/`): Core types, tools, conversation log, replay engine
  - `conversation.rs`: Message, ToolCall, ToolOutput, Entry types
  - `log.rs`: ConversationLog (versioned JSON format)
  - `replay.rs`: ReplayEngine - re-executes tools, skips LLM calls, detects drift
  - `prompt.rs`: CLAUDE.md discovery + system prompt building
  - `config.rs`: Runtime configuration
  - `tool/`: Tool trait + implementations (Read, Write, Edit, Glob, Grep)

- **quine-llm** (`crates/quine-llm/`): LLM provider abstraction
  - `provider.rs`: LlmProvider async trait
  - `types.rs`: Unified request/response types
  - `anthropic/`: Anthropic API client (non-streaming + streaming)
  - `openai/`: OpenAI API client (non-streaming + streaming)

- **quine-cli** (`crates/quine-cli/`): Binary entry point
  - `main.rs`: clap CLI with `chat` and `replay` subcommands
  - `interactive.rs`: Conversation loop (stdin -> LLM -> tools -> stdout)
  - `render.rs`: Markdown + syntax highlighting in terminal
  - `replay_cmd.rs`: Replay subcommand handler
