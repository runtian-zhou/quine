use std::path::{Path, PathBuf};

/// Discover CLAUDE.md files by walking up from the given directory.
pub fn discover_claude_md(start_dir: &Path) -> Vec<PathBuf> {
    let mut results = Vec::new();
    let mut dir = start_dir.to_path_buf();
    loop {
        let candidate = dir.join("CLAUDE.md");
        if candidate.is_file() {
            results.push(candidate);
        }
        if !dir.pop() {
            break;
        }
    }
    results.reverse(); // root-first order
    results
}

/// Build the system prompt from discovered CLAUDE.md files.
pub fn build_system_prompt(working_dir: &Path) -> String {
    let claude_files = discover_claude_md(working_dir);
    let mut parts = vec![
        "You are Quine, an interactive CLI assistant that helps with software engineering tasks."
            .to_string(),
        "You have access to file tools: Read, Write, Edit, Glob, Grep.".to_string(),
        "Use these tools to explore and modify the codebase as needed.".to_string(),
    ];

    for path in &claude_files {
        if let Ok(content) = std::fs::read_to_string(path) {
            parts.push(format!(
                "\n--- Contents of {} ---\n{}",
                path.display(),
                content
            ));
        }
    }

    parts.join("\n\n")
}
