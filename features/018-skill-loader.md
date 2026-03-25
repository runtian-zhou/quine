---
status: done
---

# Skill Loader: Load, Display, and Invoke Skills from CLI

## Overview

Skills are markdown files that extend the agent's capabilities by injecting additional system prompt instructions and optionally registering custom tool schemas. Currently the agent has a fixed system prompt and a hardcoded set of tools. This feature adds:

1. A **skill loader** in `quine-core` that parses skill markdown files
2. CLI integration so skills can be **listed** (`quine skills`), **invoked** (`quine run --skill <name>`), and **displayed** (`quine skill show <name>`)
3. Skill-aware session creation that merges skill prompts with the base system prompt

Skills are loaded from the project directory (`.quine/skills/`) and the user home directory (`~/.quine/skills/`), with project skills taking precedence.

## Requirements

### 1. Skill File Format

Skills are markdown files with YAML frontmatter stored in `.quine/skills/<name>.md` or `~/.quine/skills/<name>.md`:

```markdown
---
name: code-reviewer
description: Reviews code for bugs, style, and security issues
version: "1.0"
---

# Code Reviewer

## System Prompt

You are a code reviewer. When asked to review code:
1. Check for bugs and logic errors
2. Check for security vulnerabilities (OWASP top 10)
3. Check for style and naming convention violations
4. Suggest concrete improvements with code examples

## Tools

### lint_check

**Description**: Run a linter on a file and return results.

**Parameters**:
```json
{
  "type": "object",
  "properties": {
    "file_path": { "type": "string", "description": "Path to the file to lint" },
    "linter": { "type": "string", "enum": ["clippy", "eslint", "pylint"], "description": "Which linter to use" }
  },
  "required": ["file_path"]
}
```

**Handler**: bash
**Command**: `{linter} {file_path} 2>&1`
```

The `## Tools` section is optional. Tools defined there are **template tools** — thin wrappers around `bash` with a predefined command template. The `**Handler**: bash` + `**Command**:` pattern defines the implementation.

### 2. Core Types — `crates/quine-core/src/skill.rs` (new file)

```rust
use serde::{Deserialize, Serialize};

/// Metadata from skill frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String { "1.0".into() }

/// A parsed skill ready for use.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Metadata from frontmatter.
    pub meta: SkillMeta,
    /// Additional system prompt text to prepend/append.
    pub system_prompt: Option<String>,
    /// Tool definitions extracted from the ## Tools section.
    pub tool_definitions: Vec<SkillToolDef>,
    /// Raw markdown source.
    pub raw_source: String,
    /// File path this skill was loaded from.
    pub source_path: std::path::PathBuf,
}

/// A tool defined within a skill file.
#[derive(Debug, Clone)]
pub struct SkillToolDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    /// Handler type (currently only "bash").
    pub handler: String,
    /// Command template with `{param}` placeholders.
    pub command_template: String,
}

/// Trait for loading skills from a source.
#[async_trait::async_trait]
pub trait SkillLoader: Send + Sync {
    /// List all available skill names.
    async fn list(&self) -> anyhow::Result<Vec<SkillMeta>>;
    /// Load a skill by name.
    async fn load(&self, name: &str) -> anyhow::Result<Skill>;
}
```

### 3. Filesystem Skill Loader — `crates/quine-core/src/skill.rs`

Implement `FileSystemSkillLoader`:

```rust
pub struct FileSystemSkillLoader {
    /// Directories to search, in priority order (first match wins).
    search_paths: Vec<std::path::PathBuf>,
}

impl FileSystemSkillLoader {
    pub fn new(search_paths: Vec<std::path::PathBuf>) -> Self { ... }

    /// Default search paths: .quine/skills/ (project), ~/.quine/skills/ (user).
    pub fn default_paths(project_root: &Path) -> Self { ... }
}
```

**Parsing logic:**
1. Read the `.md` file
2. Extract YAML frontmatter between `---` delimiters → `SkillMeta`
3. Find `## System Prompt` section → extract text until next `##` heading
4. Find `## Tools` section → parse each `### <tool_name>` subsection:
   - `**Description**:` line → tool description
   - `**Parameters**:` followed by JSON code block → parameters schema
   - `**Handler**:` line → handler type
   - `**Command**:` line → command template

### 4. Skill-Aware Session Creation — `crates/quine-core/src/engine.rs`

Modify `SessionContext::new()` to accept optional skills:

```rust
pub fn new(
    system_prompt: Option<String>,
    skills: Vec<Skill>,  // NEW
    working_directory: Option<std::path::PathBuf>,
    // ... existing params
) -> Self {
    // Build combined system prompt
    let mut prompt_parts = Vec::new();
    if let Some(base) = &system_prompt {
        prompt_parts.push(base.clone());
    }
    for skill in &skills {
        if let Some(sp) = &skill.system_prompt {
            prompt_parts.push(format!("\n## Skill: {}\n{}", skill.meta.name, sp));
        }
    }
    let combined_prompt = if prompt_parts.is_empty() { None } else { Some(prompt_parts.join("\n")) };

    // Register built-in tools + skill template tools
    let mut tool_registry = ToolRegistry::new();
    // ... register built-in tools as before ...
    for skill in &skills {
        for tool_def in &skill.tool_definitions {
            tool_registry.register(Arc::new(SkillTemplateTool::new(tool_def.clone())));
        }
    }
    // ...
}
```

### 5. Skill Template Tool — `crates/quine-core/src/tool/skill_template.rs` (new file)

A generic tool that wraps a skill's tool definition and executes via bash:

```rust
pub struct SkillTemplateTool {
    def: SkillToolDef,
}

#[async_trait]
impl Tool for SkillTemplateTool {
    fn name(&self) -> &str { &self.def.name }
    fn description(&self) -> &str { &self.def.description }
    fn parameters_schema(&self) -> serde_json::Value { self.def.parameters.clone() }

    async fn execute(&self, arguments: serde_json::Value, context: &ExecutionContext)
        -> Result<ToolOutput, ToolError>
    {
        // Substitute {param} placeholders in command_template with argument values
        let mut command = self.def.command_template.clone();
        if let Some(obj) = arguments.as_object() {
            for (key, value) in obj {
                let replacement = match value {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                };
                command = command.replace(&format!("{{{}}}", key), &replacement);
            }
        }
        // Execute via bash (reuse BashTool logic or shell out)
        // ...
    }
}
```

### 6. Extend SessionConfig — `crates/quine-harness/src/config.rs`

```rust
pub struct SessionConfig {
    pub system_prompt: Option<String>,
    pub working_directory: Option<std::path::PathBuf>,
    pub skills: Vec<String>,  // NEW: skill names to load
}
```

### 7. Extend CoreInput — `crates/quine-core/src/channel.rs`

Add `skills: Vec<String>` to `CoreInput::CreateSession` so skill names flow from harness to engine.

### 8. Harness Skill Loading — `crates/quine-harness/src/local.rs`

In `LocalHarness::create_session()`:
1. Use `FileSystemSkillLoader` to resolve and load each skill name
2. Pass loaded `Vec<Skill>` into the engine's session creation

### 9. CLI Commands — `crates/quine-cli/src/main.rs`

Add `--skill` flag to `Run` and `Chat` commands:

```rust
Commands::Run {
    message: String,
    #[arg(long)]
    session: Option<String>,
    #[arg(long)]
    json: bool,
    #[arg(long)]
    socket: Option<String>,
    #[arg(long, short = 's')]
    skill: Vec<String>,       // NEW: --skill can be repeated
}
```

Add new `Skills` subcommand:

```rust
/// Manage and inspect skills.
Skills {
    #[command(subcommand)]
    command: SkillsCommands,
}

enum SkillsCommands {
    /// List all available skills.
    List {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        socket: Option<String>,
    },
    /// Show details of a specific skill.
    Show {
        /// Skill name.
        name: String,
        #[arg(long)]
        socket: Option<String>,
    },
}
```

**`quine skills list`** output:
```
Name              | Version | Description
------------------|---------|----------------------------------
code-reviewer     | 1.0     | Reviews code for bugs and style
deploy-helper     | 1.0     | Helps with deployment workflows
```

**`quine skills show code-reviewer`** output:
```
Skill: code-reviewer (v1.0)
Description: Reviews code for bugs, style, and security issues
Source: .quine/skills/code-reviewer.md

System Prompt:
  You are a code reviewer. When asked to review code...

Tools (1):
  - lint_check: Run a linter on a file and return results
    Parameters: file_path (string, required), linter (string, optional)
    Handler: bash
```

### 10. IPC Protocol Extension — `crates/quine-harness/src/protocol.rs`

Add methods:

```rust
pub const LIST_SKILLS: &str = "list_skills";    // → Vec<SkillMeta>
pub const GET_SKILL: &str = "get_skill";        // name → Skill details
```

Extend `CREATE_SESSION` params to accept `skills: Vec<String>`.

### 11. Default Skills Directory

Create `.quine/skills/` directory in the project root with a sample skill for testing:

**`.quine/skills/greeter.md`** (sample/test skill):
```markdown
---
name: greeter
description: A simple greeting skill for testing
version: "1.0"
---

# Greeter

## System Prompt

When the user says hello, respond with "Greetings from the Greeter skill!" followed by a friendly message. Always mention that you are using the Greeter skill.
```

## Acceptance Criteria

- `cargo build` / `cargo test` / `cargo clippy --all-targets -- -D warnings` / `cargo fmt --all -- --check` must pass
- Unit tests in `crates/quine-core/src/skill.rs`:
  - Parse skill frontmatter (name, description, version)
  - Parse system prompt section
  - Parse tool definitions section (name, description, params, handler, command)
  - Handle missing optional sections gracefully
  - `FileSystemSkillLoader::list()` returns skills from search paths
  - `FileSystemSkillLoader::load()` loads by name, project path takes precedence over user path
- Unit tests in `crates/quine-core/src/tool/skill_template.rs`:
  - Parameter substitution in command templates
  - Missing parameter handling
- `quine skills list` displays available skills
- `quine skills show <name>` displays skill details
- `quine run --skill greeter "Hello"` creates a session with the greeter skill's system prompt merged
- Existing tests continue to pass (no skills = original behavior)

## QA Test Cases (add to `.claude/qa-tests.md`)

```
## skill_list
**Description**: Verify `quine skills list` shows available skills.
- **Pre-setup**: Ensure `.quine/skills/greeter.md` exists in the project.
- **Command**: `quine skills list --socket /tmp/quine-qa.sock`
- **Expect**: Output contains `greeter`

## skill_show
**Description**: Verify `quine skills show` displays skill details.
- **Command**: `quine skills show greeter --socket /tmp/quine-qa.sock`
- **Expect**: Output contains `Greeter` and `System Prompt`

## skill_invoke
**Description**: Verify a skill modifies agent behavior when invoked.
- **Send**: `quine run --skill greeter --socket /tmp/quine-qa.sock "Hello"`
- **Expect**: Output contains `Greeter skill` (the skill instructs the agent to mention it)

## skill_with_tool
**Description**: Verify a skill-defined template tool is available.
- **Pre-setup**: Create a test skill with a custom bash-handler tool.
- **Send**: `quine run --skill <test-skill> --socket /tmp/quine-qa.sock "Use the <tool_name> tool..."`
- **Expect**: Tool executes and returns expected output
```

## Non-Goals (Deferred)

- **Dynamic tool implementations** beyond bash command templates (e.g., WASM, Rust plugins)
- **Skill dependencies** (one skill requiring another)
- **Skill versioning / registry** (remote skill fetching)
- **Skill-level permissions** (restricting which tools a skill can use)
- **Skill hot-reloading** during an active session
