---
status: done
---

# Tool Interface and Session Isolation

## Overview

Implement the `Tool` trait, `SessionFilesystem` trait with `OverlayFilesystem`, concrete tools (Read, Write, Bash, AskUser), and integrate tool execution directly into the core engine. The core now executes tools itself instead of delegating to the harness.

## Requirements

### Tool trait and supporting types (`quine-core/src/tool/mod.rs`)
- `Tool` async trait with `name()`, `description()`, `parameters_schema()`, `is_interactive()`, `execute()`
- `ExecutionContext` with `session_id`, `filesystem`, `working_directory`, `interaction_channel`
- `InteractionChannel`, `InteractionRequest`, `InteractionKind`, `InteractionResponse`
- `ToolOutput`, `ToolError`
- `ToolRegistry` — register tools, get by name, generate `ToolDefinition` list

### SessionFilesystem trait and OverlayFilesystem (`quine-core/src/filesystem/`)
- `SessionFilesystem` async trait: `read_file`, `write_file`, `exists`, `list_dir`, `create_dir_all`, `remove_file`, `remove_dir_all`, `resolve_path`, `root`
- `FsError` enum, `DirEntry` struct
- `OverlayFilesystem`: base layer (read-only) + session layer (read-write), copy-on-write, whiteout markers for deletes, path traversal prevention

### Concrete tools (`quine-core/src/tool/`)
- `ReadTool` (`read.rs`): read file with offset/limit, cat -n style output
- `WriteTool` (`write.rs`): write file, create parent dirs
- `BashTool` (`bash.rs`): spawn `/bin/sh -c`, capture stdout+stderr, timeout, cwd = session root
- `AskUserTool` (`ask_user.rs`): interactive tool using InteractionChannel

### Channel updates
- Add `InteractionNeeded` variant to `CoreOutput`
- Add `InteractionResponse` variant to `CoreInput`

### Engine integration
- `SessionContext` gains `ToolRegistry` and per-session `OverlayFilesystem`
- On `CreateSession`: create overlay filesystem, populate tool registry with Read/Write/Bash/AskUser
- Tools field populated from `ToolRegistry::tool_definitions()`
- When LLM emits tool call: look up tool in registry, create ExecutionContext, execute tool directly
- For interactive tools: emit `CoreOutput::InteractionNeeded`, wait for `CoreInput::InteractionResponse`
- Remove the old `ToolRequest` -> harness -> `ToolResult` flow for non-interactive tools

### Harness update
- Remove the tool execution stub in `local.rs`
- Handle `InteractionNeeded` events

## Acceptance Criteria

- All cargo build/test/clippy/fmt pass
- Unit tests for filesystem, tools, registry, and engine integration
