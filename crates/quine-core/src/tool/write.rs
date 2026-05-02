use std::path::Path;

use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

#[derive(Debug)]
struct PatchEdit {
    old_text: String,
    new_text: String,
    replace_all: bool,
}

/// Tool for applying targeted edits to a file.
pub(crate) struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "apply_patch"
    }

    fn description(&self) -> &str {
        "Apply targeted edits to a file using search/replace patch operations. Use this instead \
         of bash for file modifications. To create a new file, provide `new_file_content`."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to modify."
                },
                "new_file_content": {
                    "type": "string",
                    "description": "Optional full content used only when creating a brand new file."
                },
                "edits": {
                    "type": "array",
                    "description": "Ordered patch operations to apply to an existing file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_text": {
                                "type": "string",
                                "description": "Exact text to replace."
                            },
                            "new_text": {
                                "type": "string",
                                "description": "Replacement text."
                            },
                            "replace_all": {
                                "type": "boolean",
                                "description": "Replace every match instead of requiring exactly one match."
                            }
                        },
                        "required": ["old_text", "new_text"]
                    }
                }
            },
            "required": ["file_path"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let file_path = arguments
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: file_path".into(),
            })?;

        let path = Path::new(file_path);
        let new_file_content = arguments
            .get("new_file_content")
            .and_then(|v| v.as_str())
            .map(str::to_owned);
        let edits = parse_edits(&arguments)?;

        // Create parent directories if needed.
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                context
                    .filesystem
                    .create_dir_all(parent)
                    .await
                    .map_err(|e| ToolError::FilesystemError {
                        message: e.to_string(),
                    })?;
            }
        }

        let file_exists =
            context
                .filesystem
                .exists(path)
                .await
                .map_err(|e| ToolError::FilesystemError {
                    message: e.to_string(),
                })?;

        let (content, operation_count) = if file_exists {
            let original = context.filesystem.read_file(path).await.map_err(|e| {
                ToolError::FilesystemError {
                    message: e.to_string(),
                }
            })?;
            let updated = apply_edits(original, &edits)?;
            let operation_count = edits.len();
            (updated, operation_count)
        } else {
            let content = new_file_content.ok_or_else(|| ToolError::InvalidArguments {
                message: "file does not exist; provide new_file_content to create it".into(),
            })?;
            if !edits.is_empty() {
                return Err(ToolError::InvalidArguments {
                    message: "cannot provide edits when creating a new file".into(),
                });
            }
            (content, 1)
        };

        context
            .filesystem
            .write_file(path, &content)
            .await
            .map_err(|e| ToolError::FilesystemError {
                message: e.to_string(),
            })?;

        Ok(ToolOutput::success(format!(
            "Successfully applied {operation_count} patch operation(s) to {file_path}"
        )))
    }
}

fn parse_edits(arguments: &serde_json::Value) -> Result<Vec<PatchEdit>, ToolError> {
    let Some(edits) = arguments.get("edits") else {
        return Ok(Vec::new());
    };
    let edits = edits
        .as_array()
        .ok_or_else(|| ToolError::InvalidArguments {
            message: "edits must be an array".into(),
        })?;

    let mut parsed = Vec::with_capacity(edits.len());
    for edit in edits {
        let old_text = edit
            .get("old_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "each edit must include old_text".into(),
            })?;
        let new_text = edit
            .get("new_text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "each edit must include new_text".into(),
            })?;
        if old_text.is_empty() {
            return Err(ToolError::InvalidArguments {
                message: "old_text must not be empty; use new_file_content for new files".into(),
            });
        }
        parsed.push(PatchEdit {
            old_text: old_text.to_string(),
            new_text: new_text.to_string(),
            replace_all: edit
                .get("replace_all")
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
        });
    }

    Ok(parsed)
}

fn apply_edits(mut content: String, edits: &[PatchEdit]) -> Result<String, ToolError> {
    if edits.is_empty() {
        return Err(ToolError::InvalidArguments {
            message: "provide at least one edit for an existing file".into(),
        });
    }

    for edit in edits {
        let match_count = content.matches(&edit.old_text).count();
        if match_count == 0 {
            return Err(ToolError::InvalidArguments {
                message: format!("old_text not found in file: {:?}", edit.old_text),
            });
        }
        if !edit.replace_all && match_count != 1 {
            return Err(ToolError::InvalidArguments {
                message: format!(
                    "old_text matched {match_count} times; add more context or set replace_all=true"
                ),
            });
        }

        content = if edit.replace_all {
            content.replace(&edit.old_text, &edit.new_text)
        } else {
            content.replacen(&edit.old_text, &edit.new_text, 1)
        };
    }

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::session::SessionId;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn make_context(base: &TempDir) -> (TempDir, ExecutionContext) {
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            permission_runtime: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };
        (session_dir, ctx)
    }

    #[tokio::test]
    async fn apply_patch_creates_new_file() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": "new.txt", "new_file_content": "hello world"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("1 patch operation"));

        let content = ctx
            .filesystem
            .read_file(Path::new("new.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn apply_patch_replaces_unique_match() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("test.txt"), "hello world\n").unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "test.txt",
                    "edits": [
                        {"old_text": "world", "new_text": "patch"}
                    ]
                }),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("1 patch operation"));

        let content = ctx
            .filesystem
            .read_file(Path::new("test.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello patch\n");
    }

    #[tokio::test]
    async fn apply_patch_rejects_ambiguous_match() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("test.txt"), "x\nx\n").unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({
                    "file_path": "test.txt",
                    "edits": [
                        {"old_text": "x", "new_text": "y"}
                    ]
                }),
                &ctx,
            )
            .await;
        assert!(
            matches!(result, Err(ToolError::InvalidArguments { message }) if message.contains("matched 2 times"))
        );
    }

    #[tokio::test]
    async fn apply_patch_requires_creation_content_for_new_file() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(serde_json::json!({"file_path": "test.txt"}), &ctx)
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }
}
