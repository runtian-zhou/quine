use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct ReadTool {
    working_dir: PathBuf,
}

impl ReadTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }

    fn resolve_path(&self, file_path: &str) -> PathBuf {
        let path = Path::new(file_path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.join(path)
        }
    }
}

impl Tool for ReadTool {
    fn name(&self) -> &str {
        "Read"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the file content with line numbers."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to read"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (1-based)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read"
                }
            },
            "required": ["file_path"]
        })
    }

    fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let file_path = arguments["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;
        let path = self.resolve_path(file_path);

        let offset = arguments["offset"].as_u64().unwrap_or(1) as usize;
        let limit = arguments["limit"].as_u64().unwrap_or(2000) as usize;

        match std::fs::read_to_string(&path) {
            Ok(content) => {
                let lines: Vec<&str> = content.lines().collect();
                let start = (offset.saturating_sub(1)).min(lines.len());
                let end = (start + limit).min(lines.len());
                let numbered: Vec<String> = lines[start..end]
                    .iter()
                    .enumerate()
                    .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, line))
                    .collect();
                Ok(ToolOutput {
                    success: true,
                    output: numbered.join("\n"),
                })
            }
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Error reading {}: {}", path.display(), e),
            }),
        }
    }
}
