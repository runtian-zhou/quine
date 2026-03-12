use anyhow::Result;
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct GrepTool {
    working_dir: PathBuf,
}

impl GrepTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }
}

impl Tool for GrepTool {
    fn name(&self) -> &str {
        "Grep"
    }

    fn description(&self) -> &str {
        "Search file contents using a regex pattern. Returns matching lines with file paths and line numbers."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Regex pattern to search for"
                },
                "path": {
                    "type": "string",
                    "description": "Directory or file to search in"
                },
                "file_pattern": {
                    "type": "string",
                    "description": "Glob pattern to filter files (e.g. '*.rs')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let pattern = arguments["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;

        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Invalid regex: {}", e),
                });
            }
        };

        let search_path = arguments["path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.working_dir.clone());

        let file_pattern = arguments["file_pattern"].as_str().unwrap_or("**/*");

        let glob_pattern = if search_path.is_file() {
            search_path.to_string_lossy().to_string()
        } else {
            search_path.join(file_pattern).to_string_lossy().to_string()
        };

        let mut results = Vec::new();
        if let Ok(paths) = glob::glob(&glob_pattern) {
            for entry in paths.filter_map(|p| p.ok()) {
                if !entry.is_file() {
                    continue;
                }
                if let Ok(content) = std::fs::read_to_string(&entry) {
                    for (i, line) in content.lines().enumerate() {
                        if re.is_match(line) {
                            results.push(format!("{}:{}:{}", entry.display(), i + 1, line));
                        }
                    }
                }
            }
        }

        if results.is_empty() {
            Ok(ToolOutput {
                success: true,
                output: "No matches found.".to_string(),
            })
        } else {
            // Truncate if too many results
            let truncated = results.len() > 100;
            let output: Vec<String> = results.into_iter().take(100).collect();
            let mut text = output.join("\n");
            if truncated {
                text.push_str("\n... (results truncated)");
            }
            Ok(ToolOutput {
                success: true,
                output: text,
            })
        }
    }
}
