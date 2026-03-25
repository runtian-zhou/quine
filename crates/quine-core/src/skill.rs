use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Metadata from skill frontmatter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    #[serde(default = "default_version")]
    pub version: String,
}

fn default_version() -> String {
    "1.0".into()
}

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
    pub source_path: PathBuf,
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

/// Filesystem-based skill loader that searches multiple directories.
pub struct FileSystemSkillLoader {
    /// Directories to search, in priority order (first match wins).
    search_paths: Vec<PathBuf>,
}

impl FileSystemSkillLoader {
    pub fn new(search_paths: Vec<PathBuf>) -> Self {
        Self { search_paths }
    }

    /// Default search paths: .quine/skills/ (project), ~/.quine/skills/ (user).
    pub fn default_paths(project_root: &Path) -> Self {
        let mut paths = vec![project_root.join(".quine").join("skills")];
        if let Some(home) = dirs_home() {
            paths.push(home.join(".quine").join("skills"));
        }
        Self::new(paths)
    }

    /// Find the first file matching the skill name across search paths.
    fn find_skill_path(&self, name: &str) -> Option<PathBuf> {
        let filename = format!("{name}.md");
        for dir in &self.search_paths {
            let path = dir.join(&filename);
            if path.is_file() {
                return Some(path);
            }
        }
        None
    }
}

/// Get the user's home directory.
fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[async_trait::async_trait]
impl SkillLoader for FileSystemSkillLoader {
    async fn list(&self) -> anyhow::Result<Vec<SkillMeta>> {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        for dir in &self.search_paths {
            if !dir.is_dir() {
                continue;
            }
            let mut entries = tokio::fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        if seen.insert(stem.to_string()) {
                            let content = tokio::fs::read_to_string(&path).await?;
                            if let Ok(meta) = parse_frontmatter(&content) {
                                results.push(meta);
                            }
                        }
                    }
                }
            }
        }

        results.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(results)
    }

    async fn load(&self, name: &str) -> anyhow::Result<Skill> {
        let path = self
            .find_skill_path(name)
            .ok_or_else(|| anyhow::anyhow!("skill not found: {name}"))?;
        let content = tokio::fs::read_to_string(&path).await?;
        parse_skill(&content, path)
    }
}

/// Parse YAML frontmatter from a markdown string.
///
/// Expects `---` delimiters at the start of the content.
pub fn parse_frontmatter(content: &str) -> anyhow::Result<SkillMeta> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        anyhow::bail!("missing YAML frontmatter");
    }

    let after_first = &trimmed[3..];
    let end_idx = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("unterminated YAML frontmatter"))?;
    let yaml_str = &after_first[..end_idx];

    // Simple key-value YAML parser (no external dependency).
    let mut name = None;
    let mut description = None;
    let mut version = None;

    for line in yaml_str.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            let value = value.trim().trim_matches('"');
            match key {
                "name" => name = Some(value.to_string()),
                "description" => description = Some(value.to_string()),
                "version" => version = Some(value.to_string()),
                _ => {}
            }
        }
    }

    Ok(SkillMeta {
        name: name.ok_or_else(|| anyhow::anyhow!("missing 'name' in frontmatter"))?,
        description: description
            .ok_or_else(|| anyhow::anyhow!("missing 'description' in frontmatter"))?,
        version: version.unwrap_or_else(default_version),
    })
}

/// Parse a full skill file into a `Skill`.
pub fn parse_skill(content: &str, source_path: PathBuf) -> anyhow::Result<Skill> {
    let meta = parse_frontmatter(content)?;

    // Find the body after the second `---`.
    let trimmed = content.trim_start();
    let after_first = &trimmed[3..];
    let end_idx = after_first
        .find("\n---")
        .ok_or_else(|| anyhow::anyhow!("unterminated YAML frontmatter"))?;
    let body = &after_first[end_idx + 4..]; // skip past "\n---"

    let system_prompt = extract_section(body, "System Prompt");
    let tools_section = extract_section(body, "Tools");
    let tool_definitions = if let Some(tools_text) = &tools_section {
        parse_tool_definitions(tools_text)
    } else {
        Vec::new()
    };

    Ok(Skill {
        meta,
        system_prompt,
        tool_definitions,
        raw_source: content.to_string(),
        source_path,
    })
}

/// Extract the content of a `## <heading>` section from markdown body.
///
/// Returns the text between `## <heading>` and the next `## ` heading (or end of string).
fn extract_section(body: &str, heading: &str) -> Option<String> {
    let marker = format!("## {heading}");
    let start = body.find(&marker)?;
    let after_marker = &body[start + marker.len()..];

    // Skip to the next line after the heading.
    let content_start = after_marker.find('\n').map(|i| i + 1).unwrap_or(0);
    let content = &after_marker[content_start..];

    // Find the next `## ` heading.
    let end = content.find("\n## ").unwrap_or(content.len());

    let section = content[..end].trim();
    if section.is_empty() {
        None
    } else {
        Some(section.to_string())
    }
}

/// Parse tool definitions from the `## Tools` section content.
///
/// Each tool is defined under a `### <tool_name>` subsection.
fn parse_tool_definitions(tools_text: &str) -> Vec<SkillToolDef> {
    let mut tools = Vec::new();

    // Split on `### ` headings.
    let parts: Vec<&str> = tools_text.split("\n### ").collect();
    // The first part is text before the first ### (if any), skip it.
    for part in parts.iter().skip(1) {
        if let Some(tool) = parse_single_tool_def(part) {
            tools.push(tool);
        }
    }

    // Also handle the case where the first line starts with `### `.
    if let Some(first_part) = tools_text.strip_prefix("### ") {
        let end = first_part.find("\n### ").unwrap_or(first_part.len());
        if let Some(tool) = parse_single_tool_def(&first_part[..end]) {
            // Only add if not already present.
            if !tools.iter().any(|t| t.name == tool.name) {
                tools.insert(0, tool);
            }
        }
    }

    tools
}

/// Parse a single tool definition from its subsection text.
///
/// Expected format:
/// ```text
/// tool_name
///
/// **Description**: ...
/// **Parameters**:
/// ```json
/// { ... }
/// ```
/// **Handler**: bash
/// **Command**: `command template`
/// ```
fn parse_single_tool_def(text: &str) -> Option<SkillToolDef> {
    let mut lines = text.lines();
    let name_line = lines.next()?.trim().to_string();
    let name = name_line.trim();
    if name.is_empty() {
        return None;
    }

    let full_text = text;

    // Extract description.
    let description = extract_field(full_text, "**Description**:").unwrap_or_default();

    // Extract parameters JSON from code block.
    let parameters = extract_json_block(full_text).unwrap_or(serde_json::json!({"type": "object"}));

    // Extract handler.
    let handler = extract_field(full_text, "**Handler**:").unwrap_or_else(|| "bash".to_string());

    // Extract command template.
    let command_template = extract_command_template(full_text).unwrap_or_default();

    Some(SkillToolDef {
        name: name.to_string(),
        description,
        parameters,
        handler,
        command_template,
    })
}

/// Extract a simple field value from `**Label**: value` pattern.
fn extract_field(text: &str, label: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(label) {
            let value = rest.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Extract a JSON code block from text.
fn extract_json_block(text: &str) -> Option<serde_json::Value> {
    let start_marker = "```json";
    let start = text.find(start_marker)?;
    let after = &text[start + start_marker.len()..];
    let end = after.find("```")?;
    let json_str = after[..end].trim();
    serde_json::from_str(json_str).ok()
}

/// Extract the command template from `**Command**: \`...\`` pattern.
fn extract_command_template(text: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("**Command**:") {
            let value = rest.trim();
            // Strip backticks if present.
            let value = value.trim_matches('`').trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL_SKILL: &str = r#"---
name: code-reviewer
description: Reviews code for bugs, style, and security issues
version: "1.0"
---

# Code Reviewer

## System Prompt

You are a code reviewer. When asked to review code:
1. Check for bugs and logic errors
2. Check for security vulnerabilities

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
"#;

    const MINIMAL_SKILL: &str = r#"---
name: greeter
description: A simple greeting skill
---

# Greeter

## System Prompt

Say hello!
"#;

    #[test]
    fn parse_frontmatter_full() {
        let meta = parse_frontmatter(FULL_SKILL).unwrap();
        assert_eq!(meta.name, "code-reviewer");
        assert_eq!(
            meta.description,
            "Reviews code for bugs, style, and security issues"
        );
        assert_eq!(meta.version, "1.0");
    }

    #[test]
    fn parse_frontmatter_default_version() {
        let meta = parse_frontmatter(MINIMAL_SKILL).unwrap();
        assert_eq!(meta.name, "greeter");
        assert_eq!(meta.version, "1.0");
    }

    #[test]
    fn parse_frontmatter_missing_delimiters() {
        let result = parse_frontmatter("no frontmatter here");
        assert!(result.is_err());
    }

    #[test]
    fn parse_frontmatter_missing_name() {
        let content = "---\ndescription: test\n---\n";
        let result = parse_frontmatter(content);
        assert!(result.is_err());
    }

    #[test]
    fn parse_skill_system_prompt() {
        let skill = parse_skill(FULL_SKILL, PathBuf::from("test.md")).unwrap();
        let prompt = skill.system_prompt.unwrap();
        assert!(prompt.contains("code reviewer"));
        assert!(prompt.contains("Check for bugs"));
    }

    #[test]
    fn parse_skill_no_tools_section() {
        let skill = parse_skill(MINIMAL_SKILL, PathBuf::from("test.md")).unwrap();
        assert!(skill.tool_definitions.is_empty());
        assert!(skill.system_prompt.is_some());
        assert!(skill.system_prompt.unwrap().contains("Say hello!"));
    }

    #[test]
    fn parse_skill_tool_definitions() {
        let skill = parse_skill(FULL_SKILL, PathBuf::from("test.md")).unwrap();
        assert_eq!(skill.tool_definitions.len(), 1);

        let tool = &skill.tool_definitions[0];
        assert_eq!(tool.name, "lint_check");
        assert_eq!(
            tool.description,
            "Run a linter on a file and return results."
        );
        assert_eq!(tool.handler, "bash");
        assert_eq!(tool.command_template, "{linter} {file_path} 2>&1");

        // Check parameters schema.
        let props = tool.parameters.get("properties").unwrap();
        assert!(props.get("file_path").is_some());
        assert!(props.get("linter").is_some());
    }

    #[test]
    fn parse_skill_preserves_raw_source() {
        let skill = parse_skill(FULL_SKILL, PathBuf::from("test.md")).unwrap();
        assert_eq!(skill.raw_source, FULL_SKILL);
        assert_eq!(skill.source_path, PathBuf::from("test.md"));
    }

    #[tokio::test]
    async fn filesystem_loader_list_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        std::fs::create_dir_all(&skills_dir).unwrap();

        // Write a test skill.
        std::fs::write(skills_dir.join("test-skill.md"), MINIMAL_SKILL).unwrap();

        let loader = FileSystemSkillLoader::new(vec![skills_dir]);
        let skills = loader.list().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "greeter");

        let skill = loader.load("test-skill").await.unwrap();
        assert_eq!(skill.meta.name, "greeter");
        assert!(skill.system_prompt.is_some());
    }

    #[tokio::test]
    async fn filesystem_loader_project_precedence() {
        let project_dir = tempfile::tempdir().unwrap();
        let user_dir = tempfile::tempdir().unwrap();

        let project_skills = project_dir.path().join("skills");
        let user_skills = user_dir.path().join("skills");
        std::fs::create_dir_all(&project_skills).unwrap();
        std::fs::create_dir_all(&user_skills).unwrap();

        // Project skill.
        std::fs::write(
            project_skills.join("hello.md"),
            "---\nname: hello\ndescription: project version\n---\n\n## System Prompt\n\nProject hello\n",
        )
        .unwrap();

        // User skill with same filename.
        std::fs::write(
            user_skills.join("hello.md"),
            "---\nname: hello\ndescription: user version\n---\n\n## System Prompt\n\nUser hello\n",
        )
        .unwrap();

        // Project path comes first, so it takes precedence.
        let loader = FileSystemSkillLoader::new(vec![project_skills, user_skills]);

        let skills = loader.list().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].description, "project version");

        let skill = loader.load("hello").await.unwrap();
        assert_eq!(skill.meta.description, "project version");
        assert!(skill.system_prompt.unwrap().contains("Project hello"));
    }

    #[tokio::test]
    async fn filesystem_loader_skill_not_found() {
        let loader = FileSystemSkillLoader::new(vec![PathBuf::from("/nonexistent/path")]);
        let result = loader.load("missing").await;
        assert!(result.is_err());
    }
}
