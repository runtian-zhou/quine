---
status: done
---

# Auto-Import CLAUDE.md into System Prompt

## Overview

Automatically load `CLAUDE.md` from the session's working directory and prepend its content to the system prompt. This gives the agent project-specific context, coding conventions, and architecture knowledge without requiring manual `--system-prompt` flags. It also fulfills the bootstrapping contract described in CLAUDE.md itself: "the agent using this CLAUDE.md must be capable of building the entire project from this specification."

Currently, sessions start with only a generic default system prompt or whatever is passed explicitly. CLAUDE.md is never loaded.

## Requirements

### 1. Load CLAUDE.md in `SessionContext::new` (`crates/quine-core/src/engine.rs`)

In the combined prompt construction block (around line 106), before assembling the prompt parts:

```rust
let combined_prompt = {
    let mut prompt_parts = Vec::new();

    // Auto-load CLAUDE.md from working directory if it exists.
    let claude_md_path = working_directory.join("CLAUDE.md");
    if claude_md_path.is_file() {
        if let Ok(content) = std::fs::read_to_string(&claude_md_path) {
            prompt_parts.push(format!(
                "# Project Instructions (from CLAUDE.md)\n\n{content}"
            ));
        }
    }

    // Existing: base system prompt
    if let Some(base) = &system_prompt {
        prompt_parts.push(base.clone());
    }

    // Existing: skill prompts
    for skill in &skills { ... }

    // Fallback to default if nothing else
    if prompt_parts.is_empty() {
        prompt_parts.push(DEFAULT_SYSTEM_PROMPT.to_string());
    }
    Some(prompt_parts.join("\n\n"))
};
```

Key decisions:
- CLAUDE.md content goes **first** in the system prompt so it sets the project context before any base prompt or skill instructions
- Use `std::fs::read_to_string` (synchronous) since this runs once at session creation and the file is small
- Wrap content with a `# Project Instructions` header so the LLM understands the provenance
- If CLAUDE.md doesn't exist, skip silently (no error)
- The default system prompt is still added as fallback if no other prompt parts exist

### 2. Also check parent directories (optional enhancement)

Walk up from `working_directory` looking for CLAUDE.md, similar to how git finds `.git`:

```rust
fn find_claude_md(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join("CLAUDE.md");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}
```

This handles cases where the working directory is a subdirectory of the project root.

### 3. No changes to CLI or harness

The loading happens transparently in `SessionContext::new`. No new CLI flags, no protocol changes, no config changes needed. Any session created with a working directory containing (or parent-containing) a CLAUDE.md will automatically get it.

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass
- Sessions created in a directory with CLAUDE.md include its content in the system prompt
- Sessions created in a directory without CLAUDE.md work as before (default prompt)
- CLAUDE.md content appears before any explicit system prompt or skill prompts
- Parent directory traversal finds CLAUDE.md in project root when running from subdirectory
- Existing tests continue to pass
- New unit tests:
  - `claude_md_loaded_when_present` — create temp dir with CLAUDE.md, verify prompt contains content
  - `claude_md_missing_uses_default` — create temp dir without CLAUDE.md, verify default prompt
  - `claude_md_parent_traversal` — create nested dirs, CLAUDE.md in parent, verify it's found

## QA Test Cases

```json
[
  {
    "name": "claude_md_in_system_prompt",
    "description": "Verify CLAUDE.md content is included in agent context",
    "steps": [
      "Start a chat session in the quine project directory",
      "Ask: What build system does this project use?",
      "Verify the agent knows it uses cargo (from CLAUDE.md)"
    ]
  },
  {
    "name": "no_claude_md_still_works",
    "description": "Sessions without CLAUDE.md use default prompt",
    "steps": [
      "Start a chat session in /tmp",
      "Send a message",
      "Verify the agent responds normally with generic assistant behavior"
    ]
  }
]
```

## Non-Goals (Deferred)

- Loading multiple instruction files (e.g., `.claude/instructions.md`)
- Hot-reloading CLAUDE.md changes mid-session
- Truncating very large CLAUDE.md files to fit context limits
- Loading .claudeignore or similar exclusion patterns
- Caching CLAUDE.md content across sessions
