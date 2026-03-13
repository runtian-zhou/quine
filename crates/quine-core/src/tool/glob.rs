use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct GlobTool {
    working_dir: PathBuf,
}

impl GlobTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "Glob"
    }

    fn description(&self) -> &str {
        "Find files matching a glob pattern. Returns matching file paths."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match files (e.g. '**/*.rs')"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search in (defaults to working directory)"
                }
            },
            "required": ["pattern"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let pattern = arguments["pattern"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("pattern is required"))?;

        let base_dir = arguments["path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.working_dir.clone());

        let full_pattern = base_dir.join(pattern);
        let full_pattern_str = full_pattern.to_string_lossy();

        match glob::glob(&full_pattern_str) {
            Ok(paths) => {
                let matches: Vec<String> = paths
                    .filter_map(|p| p.ok())
                    .map(|p| p.display().to_string())
                    .collect();
                if matches.is_empty() {
                    Ok(ToolOutput {
                        success: true,
                        output: "No files matched the pattern.".to_string(),
                    })
                } else {
                    Ok(ToolOutput {
                        success: true,
                        output: matches.join("\n"),
                    })
                }
            }
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Invalid glob pattern: {}", e),
            }),
        }
    }
}
