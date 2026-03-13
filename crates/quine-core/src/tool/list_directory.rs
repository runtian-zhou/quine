use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

pub struct ListDirectoryTool {
    working_dir: PathBuf,
}

impl ListDirectoryTool {
    pub fn new(working_dir: &Path) -> Self {
        Self {
            working_dir: working_dir.to_path_buf(),
        }
    }

    fn resolve_path(&self, dir_path: &str) -> PathBuf {
        let path = Path::new(dir_path);
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.working_dir.join(path)
        }
    }
}

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "ListDirectory"
    }

    fn description(&self) -> &str {
        "List the contents of a directory. Returns file and directory names with type indicators (trailing / for directories)."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the directory to list. Defaults to the working directory."
                }
            }
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let dir_path = arguments["path"].as_str().unwrap_or(".");
        let path = self.resolve_path(dir_path);

        if !path.is_dir() {
            return Ok(ToolOutput {
                success: false,
                output: format!("{} is not a directory", path.display()),
            });
        }

        match fs::read_dir(&path) {
            Ok(entries) => {
                let mut items: Vec<String> = Vec::new();
                for entry in entries {
                    let entry = entry?;
                    let name = entry.file_name().to_string_lossy().to_string();
                    let file_type = entry.file_type()?;
                    if file_type.is_dir() {
                        items.push(format!("{}/", name));
                    } else if file_type.is_symlink() {
                        items.push(format!("{}@", name));
                    } else {
                        items.push(name);
                    }
                }
                items.sort();
                Ok(ToolOutput {
                    success: true,
                    output: items.join("\n"),
                })
            }
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Error reading directory {}: {}", path.display(), e),
            }),
        }
    }
}
