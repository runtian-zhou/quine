use std::path::{Path, PathBuf};

/// A discovered skill definition parsed from a SKILL.md file.
#[derive(Debug, Clone)]
pub struct SkillDef {
    /// Kebab-case skill name from frontmatter.
    pub name: String,
    /// One-line description from frontmatter.
    pub description: String,
    /// Directory containing the SKILL.md and its reference files.
    pub dir: PathBuf,
    /// Full markdown body (everything after the frontmatter).
    pub body: String,
}

/// Discover all skills by scanning `skills/` directories.
///
/// Looks for `skills/*/SKILL.md` relative to the working directory,
/// then walks up to the repo root doing the same.
pub fn discover_skills(working_dir: &Path) -> Vec<SkillDef> {
    let mut seen_names = std::collections::HashSet::new();
    let mut skills = Vec::new();

    let mut dir = working_dir.to_path_buf();
    loop {
        let skills_dir = dir.join("skills");
        if skills_dir.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.flatten() {
                    let skill_md = entry.path().join("SKILL.md");
                    if skill_md.is_file() {
                        if let Some(skill) = parse_skill_md(&skill_md) {
                            if seen_names.insert(skill.name.clone()) {
                                skills.push(skill);
                            }
                        }
                    }
                }
            }
        }

        // Also check for .quine/skills/ (user-level skills)
        let dot_quine_skills = dir.join(".quine").join("skills");
        if dot_quine_skills.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&dot_quine_skills) {
                for entry in entries.flatten() {
                    let skill_md = entry.path().join("SKILL.md");
                    if skill_md.is_file() {
                        if let Some(skill) = parse_skill_md(&skill_md) {
                            if seen_names.insert(skill.name.clone()) {
                                skills.push(skill);
                            }
                        }
                    }
                }
            }
        }

        if !dir.pop() {
            break;
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

/// Parse a SKILL.md file with YAML frontmatter.
///
/// Expected format:
/// ```text
/// ---
/// name: my-skill
/// description: >
///   What this skill does.
/// ---
///
/// # Skill Body
/// ...
/// ```
fn parse_skill_md(path: &Path) -> Option<SkillDef> {
    let content = std::fs::read_to_string(path).ok()?;
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        return None;
    }

    // Find the closing ---
    let after_open = &trimmed[3..];
    let close_idx = after_open.find("\n---")?;
    let frontmatter = &after_open[..close_idx];
    let body_start = 3 + close_idx + 4; // skip "---" + "\n---"
    let body = trimmed[body_start..].trim().to_string();

    let name = extract_yaml_field(frontmatter, "name")?;
    let description = extract_yaml_field(frontmatter, "description").unwrap_or_default();

    Some(SkillDef {
        name,
        description,
        dir: path.parent()?.to_path_buf(),
        body,
    })
}

/// Extract a simple YAML scalar field value.
/// Handles both single-line (`name: value`) and multi-line folded (`description: >\n  line1\n  line2`).
fn extract_yaml_field(frontmatter: &str, field: &str) -> Option<String> {
    let prefix = format!("{}:", field);
    let lines: Vec<&str> = frontmatter.lines().collect();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with(&prefix) {
            let after_colon = trimmed[prefix.len()..].trim();

            // Multi-line folded scalar (>)
            if after_colon == ">" || after_colon == "|" {
                let mut parts = Vec::new();
                for continuation in &lines[i + 1..] {
                    let cont_trimmed = continuation.trim();
                    if cont_trimmed.is_empty() {
                        break;
                    }
                    // Continuation lines are indented
                    if continuation.starts_with(' ') || continuation.starts_with('\t') {
                        parts.push(cont_trimmed);
                    } else {
                        break;
                    }
                }
                return Some(parts.join(" "));
            }

            // Single-line value
            return Some(after_colon.to_string());
        }
    }
    None
}

/// Build the full prompt for executing a skill.
///
/// Reads the SKILL.md body and inlines any referenced files from the skill's
/// `references/` directory.
pub fn build_skill_prompt(skill: &SkillDef, user_input: &str) -> String {
    let mut prompt = String::new();

    prompt.push_str(&format!("# Skill: {}\n\n", skill.name));
    prompt.push_str(&skill.body);

    // Inline reference files if they exist
    let refs_dir = skill.dir.join("references");
    if refs_dir.is_dir() {
        if let Ok(mut entries) = std::fs::read_dir(&refs_dir) {
            let mut ref_files: Vec<_> = entries
                .by_ref()
                .flatten()
                .filter(|e| e.path().is_file())
                .collect();
            ref_files.sort_by_key(|e| e.file_name());

            for entry in ref_files {
                let path = entry.path();
                if let Ok(content) = std::fs::read_to_string(&path) {
                    let filename = path.file_name().unwrap_or_default().to_string_lossy();
                    prompt.push_str(&format!(
                        "\n\n---\n## Reference: {}\n\n{}",
                        filename, content
                    ));
                }
            }
        }
    }

    if !user_input.is_empty() {
        prompt.push_str(&format!("\n\n---\n## User Input\n\n{}", user_input));
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn parse_skill_md_parses_frontmatter_and_body() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("test-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: test-skill\ndescription: A test skill.\n---\n\n# Test\n\nBody here.",
        )
        .unwrap();

        let skill = parse_skill_md(&skill_dir.join("SKILL.md")).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill.");
        assert_eq!(skill.body, "# Test\n\nBody here.");
    }

    #[test]
    fn parse_skill_md_multiline_description() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("my-skill");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: my-skill\ndescription: >\n  Line one\n  line two.\n---\n\nBody.",
        )
        .unwrap();

        let skill = parse_skill_md(&skill_dir.join("SKILL.md")).unwrap();
        assert_eq!(skill.name, "my-skill");
        assert_eq!(skill.description, "Line one line two.");
    }

    #[test]
    fn parse_skill_md_returns_none_without_frontmatter() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("SKILL.md"), "# No frontmatter").unwrap();
        assert!(parse_skill_md(&tmp.path().join("SKILL.md")).is_none());
    }

    #[test]
    fn discover_skills_finds_skills_in_skills_dir() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("alpha");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: alpha\ndescription: Alpha skill.\n---\n\nAlpha body.",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "alpha");
    }

    #[test]
    fn discover_skills_deduplicates_by_name() {
        let tmp = TempDir::new().unwrap();

        // Create same-named skill in two locations
        let dir1 = tmp.path().join("skills").join("dup");
        fs::create_dir_all(&dir1).unwrap();
        fs::write(
            dir1.join("SKILL.md"),
            "---\nname: dup\ndescription: First.\n---\n\nFirst.",
        )
        .unwrap();

        let dir2 = tmp.path().join(".quine").join("skills").join("dup");
        fs::create_dir_all(&dir2).unwrap();
        fs::write(
            dir2.join("SKILL.md"),
            "---\nname: dup\ndescription: Second.\n---\n\nSecond.",
        )
        .unwrap();

        let skills = discover_skills(tmp.path());
        assert_eq!(skills.len(), 1);
        // The skills/ dir is scanned first, so first one wins
        assert_eq!(skills[0].description, "First.");
    }

    #[test]
    fn build_skill_prompt_includes_references() {
        let tmp = TempDir::new().unwrap();
        let skill_dir = tmp.path().join("skills").join("ref-skill");
        let refs_dir = skill_dir.join("references");
        fs::create_dir_all(&refs_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: ref-skill\ndescription: Has refs.\n---\n\n# Main body.",
        )
        .unwrap();
        fs::write(refs_dir.join("01-setup.md"), "Setup instructions.").unwrap();

        let skill = parse_skill_md(&skill_dir.join("SKILL.md")).unwrap();
        let prompt = build_skill_prompt(&skill, "do the thing");

        assert!(prompt.contains("# Skill: ref-skill"));
        assert!(prompt.contains("# Main body."));
        assert!(prompt.contains("## Reference: 01-setup.md"));
        assert!(prompt.contains("Setup instructions."));
        assert!(prompt.contains("## User Input"));
        assert!(prompt.contains("do the thing"));
    }
}
