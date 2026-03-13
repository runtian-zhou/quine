# Design: JSON Output Flag

## Status
Implemented

## Summary
Add a `--json` flag to the CLI that makes every command output structured JSON instead of human-readable text. This allows users to pipe output to tools like `jq` for programmatic consumption, enabling scripting and integration with other systems.

## Motivation
Currently, the CLI only produces human-readable text output. While this is fine for interactive use, it makes the tool difficult to use in scripts, CI pipelines, and automation workflows. Users who want to extract specific fields must resort to fragile text parsing with `grep`/`awk`/`sed`. A `--json` flag gives them stable, structured output they can rely on.

## Design

### Public API Changes

**New global CLI flag:**
- `--json` (short: `-j`): When present, all command output is emitted as JSON instead of human-readable text. This is a global flag available on every subcommand.

**New public types (in `src/output.rs`):**
- `OutputFormat` enum: `Human` | `Json` -- represents the selected output mode.
- `OutputWriter` struct: Accepts an `OutputFormat` and provides methods to emit results. Commands call `OutputWriter::write_result()` with a serializable value, and the writer handles formatting.

**New trait (in `src/output.rs`):**
- `CommandResult`: A marker trait requiring `serde::Serialize + std::fmt::Display`. Every command's result type must implement this. `Display` is used for human output; `Serialize` is used for JSON output.

### Internal Design

The implementation touches three layers:

1. **CLI argument parsing (`src/cli.rs`):** The `--json` flag is added to the top-level `Cli` struct using `clap`. It is a global argument so it applies to all subcommands without repeating the definition.

2. **Output abstraction (`src/output.rs`):** A new module that owns the `OutputFormat` enum, `OutputWriter` struct, and `CommandResult` trait. The writer examines the format and either calls `Display::fmt()` for human output or `serde_json::to_string_pretty()` for JSON output. Errors are also serialized as JSON when `--json` is active (with a top-level `{"error": "..."}` envelope).

3. **Command handlers (`src/commands/*.rs`):** Each command handler returns a type implementing `CommandResult`. The `main` function passes the `OutputWriter` into each handler, and the handler calls `writer.write_result(&result)` instead of printing directly.

**Data flow:**
```
CLI args parsed --> OutputFormat determined --> OutputWriter created
    --> Command executes --> Returns impl CommandResult
    --> OutputWriter formats and prints to stdout
```

**Error envelope (JSON mode):**
```json
{
  "error": {
    "code": "not_found",
    "message": "File 'foo.txt' not found"
  }
}
```

**Success envelope (JSON mode):**
```json
{
  "status": "ok",
  "data": { ... }
}
```

### Error Handling

- When `--json` is active, errors are serialized as JSON to stdout (not stderr) with a non-zero exit code. This ensures the caller always gets parseable JSON.
- When `--json` is NOT active, errors print to stderr as usual.
- Serialization failures (e.g., a type cannot be serialized) are caught and produce a fallback JSON error message.

## Alternatives Considered

**1. Environment variable (`OUTPUT_FORMAT=json`) instead of a flag:**
Rejected because CLI flags are more discoverable, more explicit, and work better in shell aliases. An env var could be added later as a secondary mechanism.

**2. Per-command `--json` flags instead of a global one:**
Rejected because it creates inconsistency -- users would have to remember which commands support it. A global flag guarantees uniform behavior.

**3. Separate `--output-format` flag with values like `json`, `yaml`, `csv`:**
Considered, but YAGNI applies. JSON is the clear winner for structured CLI output. If more formats are needed later, `--json` can be deprecated in favor of `--output-format` without breaking scripts (just add a deprecation warning).

## Testing Plan

- **Unit tests for `OutputWriter`:** Verify that `write_result` produces correct JSON when format is `Json`, and correct display text when format is `Human`.
- **Unit tests for error formatting:** Verify the JSON error envelope is correct.
- **Integration tests for CLI flag parsing:** Run the binary with `--json` and verify the output is valid JSON. Run without `--json` and verify human-readable output.
- **Integration test for piping to jq:** Demonstrate that `tool --json <command> | jq .data` works.
- **Edge cases:** Empty results, results with special characters (unicode, newlines), very large outputs.

## Unresolved Questions

- Should `--json` also affect `--help` output? (Probably not -- help is always human-readable.)
- Should there be a `--json-compact` variant for minimal whitespace? (Defer to a follow-up if requested.)
