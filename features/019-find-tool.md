---
status: done
---

# Built-in Find Tool

## Overview

Add a `find` tool that searches for files and directories by name pattern, type, and content. Currently the agent must use the `bash` tool to run `find` or `ls -R`, which is slow, hard to parse, and doesn't go through the `SessionFilesystem` abstraction (so overlay filesystem boundaries aren't respected). A built-in `find` tool uses the existing `SessionFilesystem::list_dir()` and `SessionFilesystem::read_file()` methods to provide fast, sandbox-aware file discovery.

## Requirements

### 1. New Tool File — `crates/quine-core/src/tool/find.rs`

Create a `FindTool` struct implementing the `Tool` trait (pattern from `crates/quine-core/src/tool/read.rs`).

**Tool name**: `find`

**Description**: Search for files and directories by name pattern, type, or content. Walks the directory tree using glob matching. Results are returned as a newline-separated list of relative paths.

**Parameters schema**:

```json
{
  "type": "object",
  "properties": {
    "path": {
      "type": "string",
      "description": "The directory to search in. Defaults to the working directory."
    },
    "pattern": {
      "type": "string",
      "description": "Glob pattern to match file/directory names (e.g., '*.rs', 'test_*'). Defaults to '*' (all)."
    },
    "type": {
      "type": "string",
      "enum": ["file", "directory", "any"],
      "description": "Filter by entry type. Defaults to 'any'."
    },
    "content": {
      "type": "string",
      "description": "Optional text to search for within file contents (simple substring match). Only files whose contents contain this string are returned."
    },
    "max_depth": {
      "type": "integer",
      "description": "Maximum directory depth to recurse. 0 means only the given path itself. Defaults to no limit."
    },
    "max_results": {
      "type": "integer",
      "description": "Maximum number of results to return. Defaults to 200."
    }
  }
}
```

No required parameters — all have sensible defaults.

### 2. Implementation

```rust
pub(crate) struct FindTool;

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str { "find" }
    fn description(&self) -> &str { /* see above */ }
    fn parameters_schema(&self) -> serde_json::Value { /* see above */ }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        // 1. Parse parameters with defaults
        // 2. Resolve `path` relative to context.working_directory
        // 3. Recursive walk using context.filesystem.list_dir()
        // 4. For each entry:
        //    a. Check type filter (file/directory/any)
        //    b. Check glob pattern against entry name
        //    c. If content filter set and entry is a file, read and check substring
        // 5. Collect up to max_results matching paths
        // 6. Return newline-separated relative paths
    }
}
```

**Key implementation details:**

- Use `context.filesystem.list_dir()` for directory traversal — do NOT use `std::fs` directly. This ensures the overlay filesystem is respected.
- For glob matching, use simple glob logic: `*` matches any sequence of characters, `?` matches a single character. Implement inline or use the `glob-match` crate if already in dependencies, otherwise implement a minimal matcher (avoid adding new dependencies).
- For content search, use `context.filesystem.read_file()` and `String::contains()`. Skip files that fail to read (binary, permission errors) silently.
- Respect `max_depth` by tracking recursion depth.
- Respect `max_results` by stopping early once the limit is reached.
- Return paths relative to the search `path` parameter.
- Skip hidden directories (`.git`, `.hg`, `node_modules`, `target`) by default to avoid noise. If the user explicitly includes them in the pattern, respect that.

**Output format:**
```
Found 15 matches in ./src:
./src/main.rs
./src/lib.rs
./src/tool/find.rs
./src/tool/read.rs
...
```

If no matches: `No files found matching pattern '<pattern>' in '<path>'`

### 3. Register the Tool — `crates/quine-core/src/engine.rs`

Add to the tool registration block in `SessionContext::new()`:

```rust
tool_registry.register(Arc::new(crate::tool::find::FindTool));
```

### 4. Module Declaration — `crates/quine-core/src/tool/mod.rs`

Add `pub mod find;` to the module list.

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must pass
- Unit tests in `crates/quine-core/src/tool/find.rs`:
  - `find_all_files` — find all files in a directory (no pattern filter)
  - `find_by_glob_pattern` — `*.rs` matches only `.rs` files
  - `find_by_type_file` — type=file excludes directories
  - `find_by_type_directory` — type=directory excludes files
  - `find_with_content` — content filter returns only files containing the string
  - `find_with_max_depth` — max_depth=0 only returns entries in the root dir
  - `find_with_max_results` — stops at max_results
  - `find_empty_directory` — returns no matches for empty dir
  - `find_nested` — finds files in subdirectories
  - `find_default_path` — uses working directory when path is not specified
- Tests follow the same `make_context` + `TempDir` pattern from `crates/quine-core/src/tool/read.rs`
- Existing tests continue to pass

## QA Test Cases (add to `.claude/qa-tests.md`)

```
## find_tool_basic
**Description**: Verify the find tool discovers files by glob pattern.
- **Send**: `"Use the find tool to search for files matching '*.toml' in the current directory. List the results."`
- **Expect**: Output contains `Cargo.toml`

## find_tool_content_search
**Description**: Verify the find tool can search file contents.
- **Send**: `"Use the find tool to search for files containing 'quine-core' in the current directory with max_depth 1. List the results."`
- **Expect**: Output contains `Cargo.toml`

## find_tool_type_filter
**Description**: Verify the find tool filters by type.
- **Send**: `"Use the find tool to search for directories named 'src' in the 'crates' directory. List the results."`
- **Expect**: Output contains `src`
```

## Non-Goals (Deferred)

- Regex pattern matching (glob is sufficient for now)
- File size or modification time filters
- Exclude patterns (beyond default hidden directory skipping)
- Streaming results for very large directory trees
- Integration with external search tools (ripgrep, fd)
