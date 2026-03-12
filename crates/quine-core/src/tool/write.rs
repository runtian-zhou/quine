use anyhow::Result;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct WriteTool {
    working_dir: PathBuf,
}

impl WriteTool {
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

impl Tool for WriteTool {
    fn name(&self) -> &str {
        "Write"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating it if it doesn't exist. Creates parent directories as needed."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Content to write to the file"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let file_path = arguments["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;
        let content = arguments["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("content is required"))?;

        let path = self.resolve_path(file_path);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match std::fs::write(&path, content) {
            Ok(()) => Ok(ToolOutput {
                success: true,
                output: format!("File written successfully: {}", path.display()),
            }),
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Error writing {}: {}", path.display(), e),
            }),
        }
    }
}
