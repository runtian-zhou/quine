use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct EditTool {
    working_dir: PathBuf,
}

impl EditTool {
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

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "Edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing an exact string match with new content."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The replacement string"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "Replace all occurrences (default: false)"
                }
            },
            "required": ["file_path", "old_string", "new_string"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let file_path = arguments["file_path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("file_path is required"))?;
        let old_string = arguments["old_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("old_string is required"))?;
        let new_string = arguments["new_string"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("new_string is required"))?;
        let replace_all = arguments["replace_all"].as_bool().unwrap_or(false);

        let path = self.resolve_path(file_path);

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolOutput {
                    success: false,
                    output: format!("Error reading {}: {}", path.display(), e),
                });
            }
        };

        if !content.contains(old_string) {
            return Ok(ToolOutput {
                success: false,
                output: format!("old_string not found in {}", path.display()),
            });
        }

        if !replace_all {
            let count = content.matches(old_string).count();
            if count > 1 {
                return Ok(ToolOutput {
                    success: false,
                    output: format!(
                        "old_string found {} times in {}. Use replace_all or provide more context.",
                        count,
                        path.display()
                    ),
                });
            }
        }

        let new_content = if replace_all {
            content.replace(old_string, new_string)
        } else {
            content.replacen(old_string, new_string, 1)
        };

        std::fs::write(&path, new_content)?;

        Ok(ToolOutput {
            success: true,
            output: format!("File edited successfully: {}", path.display()),
        })
    }
}
