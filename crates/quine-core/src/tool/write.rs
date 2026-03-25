use std::path::Path;

use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Tool for writing content to a file.
///
/// Creates parent directories automatically if they don't exist.
pub(crate) struct WriteTool;

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file. Creates the file if it doesn't exist, or overwrites it if it does. \
         Parent directories are created automatically."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to write."
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file."
                }
            },
            "required": ["file_path", "content"]
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

        let content = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: content".into(),
            })?;

        let path = Path::new(file_path);

        // Create parent directories if needed
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

        context
            .filesystem
            .write_file(path, content)
            .await
            .map_err(|e| ToolError::FilesystemError {
                message: e.to_string(),
            })?;

        Ok(ToolOutput::success(format!(
            "Successfully wrote {} bytes to {file_path}",
            content.len()
        )))
    }
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
            core_input: None,
        };
        (session_dir, ctx)
    }

    #[tokio::test]
    async fn write_file_creates_new() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": "new.txt", "content": "hello world"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.content.contains("11 bytes"));

        // Verify we can read it back
        let content = ctx
            .filesystem
            .read_file(Path::new("new.txt"))
            .await
            .unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn write_file_creates_parent_dirs() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": "a/b/c.txt", "content": "deep"}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error);

        let content = ctx
            .filesystem
            .read_file(Path::new("a/b/c.txt"))
            .await
            .unwrap();
        assert_eq!(content, "deep");
    }

    #[tokio::test]
    async fn write_file_missing_args() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = WriteTool;

        let result = tool
            .execute(serde_json::json!({"file_path": "test.txt"}), &ctx)
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }
}
