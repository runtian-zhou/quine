# --json Output Flag Implementation Plan

## Overview

Add a `--json` global flag to the CLI so that every subcommand outputs structured
JSON instead of human-readable text. This enables piping output to `jq` and
integrating with other tooling.

## Design Decisions

### 1. Global flag via clap `#[arg(global = true)]`

The `--json` flag is defined once on the top-level `Cli` struct with
`global = true`, so it works regardless of which subcommand follows it:

```
quine-cli --json list .
quine-cli --json info Cargo.toml
quine-cli --json search "foo" --limit 5
```

### 2. Dual-trait pattern: `Serialize + Display`

Every command result struct implements both:
- `serde::Serialize` -- for JSON output
- `std::fmt::Display` -- for human-readable output

The `output::render()` and `output::render_list()` functions accept any type
satisfying both traits and dispatch based on the chosen `OutputFormat`.

### 3. Single JSON document per invocation

In JSON mode, list commands emit a JSON **array** (not one object per line).
This ensures the entire stdout is always one valid JSON document, making it
safe to pipe directly to `jq` without `-s` (slurp) mode.

### 4. Errors as JSON

When `--json` is active, errors are also emitted as `{"error": "..."}` on
stdout so downstream JSON consumers never encounter unstructured text.

## File Structure

```
src/
  main.rs           -- Entry point, parses CLI, dispatches to commands
  cli.rs            -- Clap derive structs (Cli, Commands)
  output.rs         -- OutputFormat enum, render/render_list/render_error helpers
  commands/
    mod.rs
    list.rs         -- `list` subcommand with ListItem struct
    info.rs         -- `info` subcommand with ItemInfo struct
    search.rs       -- `search` subcommand with SearchOutput/SearchResult structs
tests/
  cli_json_output.rs -- Integration tests using assert_cmd
```

## Testing Strategy

- **Unit tests** in `output.rs` verify serialization correctness (exact field
  values, array structure, error key presence).
- **Integration tests** in `tests/cli_json_output.rs` run the actual binary and
  verify:
  - Human mode does not produce JSON
  - JSON mode produces a parseable JSON document
  - All expected fields are present with correct types
  - The output is a single JSON document (pipe-safe)
  - The `--json` flag works when placed before the subcommand
