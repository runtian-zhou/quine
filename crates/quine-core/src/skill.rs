use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// Core-owned skill service API for discovery and loading.
#[async_trait::async_trait]
pub trait SkillService: Send + Sync {
    /// List all available skills.
    async fn list_skills(&self) -> anyhow::Result<Vec<SkillMeta>>;
    /// Load a full skill by name.
    async fn get_skill(&self, name: &str) -> anyhow::Result<Skill>;
    /// Load multiple skills, preserving the requested order.
    async fn load_skills(&self, names: &[String]) -> Vec<Skill>;
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

    pub fn search_paths(&self) -> &[PathBuf] {
        &self.search_paths
    }

    fn project_paths(project_root: &Path) -> Vec<PathBuf> {
        vec![
            project_root.join(".quine").join("skills"),
            project_root.join(".claude").join("commands"),
            project_root.join(".codex").join("skills"),
            project_root.join(".codex").join("skills").join(".system"),
        ]
    }

    /// Project-scoped search paths only.
    pub fn project_only(project_root: &Path) -> Self {
        Self::new(Self::project_paths(project_root))
    }

    /// Default search paths include native Quine skills plus legacy Claude/Codex locations.
    pub fn default_paths(project_root: &Path) -> Self {
        let mut paths = Self::project_paths(project_root);
        if let Some(home) = dirs_home() {
            paths.push(home.join(".quine").join("skills"));
            paths.push(home.join(".claude").join("commands"));
            paths.push(home.join(".codex").join("skills"));
            paths.push(home.join(".codex").join("skills").join(".system"));
            paths.push(
                home.join(".codex")
                    .join("vendor_imports")
                    .join("skills")
                    .join("skills")
                    .join(".curated"),
            );
        }
        Self::new(paths)
    }

    /// Find the first file matching the skill name across search paths.
    fn find_skill_path(&self, name: &str) -> Option<PathBuf> {
        let filename = format!("{name}.md");
        for dir in &self.search_paths {
            let file_path = dir.join(&filename);
            if file_path.is_file() {
                return Some(file_path);
            }

            let directory_path = dir.join(name).join("SKILL.md");
            if directory_path.is_file() {
                return Some(directory_path);
            }
        }
        None
    }

    async fn discover_skill_paths(&self) -> anyhow::Result<Vec<(String, PathBuf)>> {
        let mut seen = std::collections::HashSet::new();
        let mut results = Vec::new();

        for dir in &self.search_paths {
            if !dir.is_dir() {
                continue;
            }
            let mut entries = tokio::fs::read_dir(dir).await?;
            while let Some(entry) = entries.next_entry().await? {
                let path = entry.path();
                let candidate = if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .map(|stem| (stem.to_string(), path.clone()))
                } else if path.is_dir() {
                    let skill_path = path.join("SKILL.md");
                    if skill_path.is_file() {
                        path.file_name()
                            .and_then(|s| s.to_str())
                            .map(|name| (name.to_string(), skill_path))
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some((name, skill_path)) = candidate {
                    if seen.insert(name.clone()) {
                        results.push((name, skill_path));
                    }
                }
            }
        }

        results.sort_by(|left, right| left.0.cmp(&right.0));
        Ok(results)
    }

    /// Load every discoverable skill from this loader's search paths.
    pub async fn load_all(&self) -> Vec<Skill> {
        let mut skills = Vec::new();
        let Ok(paths) = self.discover_skill_paths().await else {
            return skills;
        };

        for (_lookup_name, skill_path) in paths {
            let Ok(content) = tokio::fs::read_to_string(&skill_path).await else {
                continue;
            };
            let Ok(skill) = parse_skill(&content, skill_path) else {
                continue;
            };
            skills.push(skill);
        }

        skills.sort_by(|left, right| left.meta.name.cmp(&right.meta.name));
        skills
    }
}

/// Get the user's home directory.
fn dirs_home() -> Option<PathBuf> {
    std::env::var("HOME").ok().map(PathBuf::from)
}

#[async_trait::async_trait]
impl SkillLoader for FileSystemSkillLoader {
    async fn list(&self) -> anyhow::Result<Vec<SkillMeta>> {
        let mut results = Vec::new();

        for (name, skill_path) in self.discover_skill_paths().await? {
            let content = tokio::fs::read_to_string(&skill_path).await?;
            if let Ok(meta) = parse_skill_meta(&content, &skill_path, &name) {
                results.push(meta);
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

/// Default `SkillService` backed by a `SkillLoader`.
pub struct DefaultSkillService {
    loader: Arc<dyn SkillLoader>,
}

impl DefaultSkillService {
    pub fn new(loader: Arc<dyn SkillLoader>) -> Self {
        Self { loader }
    }

    /// Build a filesystem-backed service using the standard skill search paths.
    pub fn from_default_paths(project_root: &Path) -> Self {
        Self::new(Arc::new(FileSystemSkillLoader::default_paths(project_root)))
    }
}

#[async_trait::async_trait]
impl SkillService for DefaultSkillService {
    async fn list_skills(&self) -> anyhow::Result<Vec<SkillMeta>> {
        self.loader.list().await
    }

    async fn get_skill(&self, name: &str) -> anyhow::Result<Skill> {
        self.loader.load(name).await
    }

    async fn load_skills(&self, names: &[String]) -> Vec<Skill> {
        let mut skills = Vec::new();

        for name in names {
            match self.loader.load(name).await {
                Ok(skill) => skills.push(skill),
                Err(_error) => {}
            }
        }

        skills
    }
}

/// Create the default filesystem-backed skill service for the given project root.
pub fn default_skill_service(project_root: &Path) -> DefaultSkillService {
    DefaultSkillService::from_default_paths(project_root)
}

/// List skills using the standard default skill search paths.
pub async fn list_available_skills(project_root: &Path) -> anyhow::Result<Vec<SkillMeta>> {
    default_skill_service(project_root).list_skills().await
}

/// Load a single skill using the standard default skill search paths.
pub async fn load_skill(project_root: &Path, name: &str) -> anyhow::Result<Skill> {
    default_skill_service(project_root).get_skill(name).await
}

/// Load multiple skills using the standard default skill search paths.
pub async fn load_skills(project_root: &Path, names: &[String]) -> Vec<Skill> {
    default_skill_service(project_root).load_skills(names).await
}

/// Load every project-scoped skill from `.quine`, `.claude`, and `.codex`.
pub async fn load_project_skills(project_root: &Path) -> Vec<Skill> {
    FileSystemSkillLoader::project_only(project_root)
        .load_all()
        .await
}

/// Resolve the full session skill set by auto-attaching project skills and
/// then layering explicitly requested skills on top without duplicates.
pub async fn load_session_skills(project_root: &Path, names: &[String]) -> Vec<Skill> {
    let mut seen = std::collections::HashSet::new();
    let mut skills = Vec::new();

    for skill in load_project_skills(project_root).await {
        if seen.insert(skill.meta.name.clone()) {
            skills.push(skill);
        }
    }

    for skill in load_skills(project_root, names).await {
        if seen.insert(skill.meta.name.clone()) {
            skills.push(skill);
        }
    }

    skills
}

fn parse_skill_meta(
    content: &str,
    source_path: &Path,
    default_name: &str,
) -> anyhow::Result<SkillMeta> {
    parse_frontmatter(content)
        .or_else(|_| legacy_claude_command_meta(source_path, content, default_name))
}

fn legacy_claude_command_meta(
    source_path: &Path,
    content: &str,
    default_name: &str,
) -> anyhow::Result<SkillMeta> {
    if !is_claude_command_path(source_path) {
        anyhow::bail!("missing YAML frontmatter");
    }

    Ok(SkillMeta {
        name: default_name.to_string(),
        description: legacy_description(content, default_name),
        version: default_version(),
    })
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
    let meta = match parse_frontmatter(content) {
        Ok(meta) => meta,
        Err(_) if is_claude_command_path(&source_path) => SkillMeta {
            name: source_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("legacy-command")
                .to_string(),
            description: legacy_description(
                content,
                source_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("legacy-command"),
            ),
            version: default_version(),
        },
        Err(err) => return Err(err),
    };

    if is_claude_command_path(&source_path) && !content.trim_start().starts_with("---") {
        return Ok(Skill {
            meta,
            system_prompt: Some(content.trim().to_string()),
            tool_definitions: Vec::new(),
            raw_source: content.to_string(),
            source_path,
        });
    }

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

fn is_claude_command_path(path: &Path) -> bool {
    path.parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("commands")
}

fn legacy_description(content: &str, default_name: &str) -> String {
    content
        .lines()
        .map(str::trim)
        .find_map(|line| {
            if line.is_empty() {
                None
            } else {
                Some(
                    line.trim_start_matches('#')
                        .trim()
                        .trim_end_matches('.')
                        .to_string(),
                )
            }
        })
        .filter(|line| !line.is_empty())
        .unwrap_or_else(|| format!("Legacy Claude command '{default_name}'"))
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

    #[tokio::test]
    async fn filesystem_loader_loads_directory_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skills_dir = dir.path().join("skills");
        let doc_dir = skills_dir.join("doc");
        std::fs::create_dir_all(&doc_dir).unwrap();
        std::fs::write(doc_dir.join("SKILL.md"), MINIMAL_SKILL).unwrap();

        let loader = FileSystemSkillLoader::new(vec![skills_dir]);
        let skills = loader.list().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "greeter");

        let skill = loader.load("doc").await.unwrap();
        assert_eq!(skill.meta.name, "greeter");
        assert!(skill.system_prompt.unwrap().contains("Say hello!"));
    }

    #[tokio::test]
    async fn filesystem_loader_loads_legacy_claude_command() {
        let dir = tempfile::tempdir().unwrap();
        let commands_dir = dir.path().join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("qa.md"),
            "You are running QA tests for the quine project.\n\nFollow this workflow exactly.\n",
        )
        .unwrap();

        let loader = FileSystemSkillLoader::new(vec![commands_dir]);
        let skills = loader.list().await.unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "qa");
        assert!(skills[0].description.contains("QA tests"));

        let skill = loader.load("qa").await.unwrap();
        assert_eq!(skill.meta.name, "qa");
        assert!(skill
            .system_prompt
            .unwrap()
            .contains("Follow this workflow exactly."));
        assert!(skill.tool_definitions.is_empty());
    }

    #[tokio::test]
    async fn load_project_skills_loads_markdown_commands_and_ignores_python_helpers() {
        let project_dir = tempfile::tempdir().unwrap();
        let commands_dir = project_dir.path().join(".claude").join("commands");
        let codex_skill_dir = project_dir
            .path()
            .join(".codex")
            .join("skills")
            .join("review");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::create_dir_all(&codex_skill_dir).unwrap();
        std::fs::write(
            commands_dir.join("qa.md"),
            "You are running QA tests for the quine project.\n\nFollow this workflow exactly.\n",
        )
        .unwrap();
        std::fs::write(
            commands_dir.join("feature_planning.py"),
            "print('helper')\n",
        )
        .unwrap();
        std::fs::write(
            codex_skill_dir.join("SKILL.md"),
            "---\nname: review\ndescription: Review project changes\n---\n\n## System Prompt\n\nReview carefully.\n",
        )
        .unwrap();

        let skills = load_project_skills(project_dir.path()).await;
        let names = skills
            .iter()
            .map(|skill| skill.meta.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["qa", "review"]);
        assert!(skills
            .iter()
            .any(|skill| skill.source_path == commands_dir.join("qa.md")));
        assert!(skills
            .iter()
            .all(|skill| { skill.source_path != commands_dir.join("feature_planning.py") }));
    }

    #[tokio::test]
    async fn load_session_skills_keeps_project_auto_skills_without_duplicates() {
        let project_dir = tempfile::tempdir().unwrap();
        let commands_dir = project_dir.path().join(".claude").join("commands");
        std::fs::create_dir_all(&commands_dir).unwrap();
        std::fs::write(
            commands_dir.join("auto-attached.md"),
            "Project auto-attached instructions.\n",
        )
        .unwrap();
        std::fs::write(
            commands_dir.join("second-auto.md"),
            "Another project auto-attached skill.\n",
        )
        .unwrap();

        let skills = load_session_skills(project_dir.path(), &["auto-attached".into()]).await;
        let names = skills
            .iter()
            .map(|skill| skill.meta.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["auto-attached", "second-auto"]);
    }
}
