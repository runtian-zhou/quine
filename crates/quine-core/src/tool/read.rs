use std::path::Path;

use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Tool for reading file contents with optional offset and limit.
///
/// Produces cat -n style output with line numbers.
pub(crate) struct ReadTool;

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the file content with line numbers (cat -n style). \
         Supports optional offset and limit parameters to read a specific range of lines."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "The path to the file to read."
                },
                "offset": {
                    "type": "integer",
                    "description": "The 1-based line number to start reading from. Defaults to 1."
                },
                "limit": {
                    "type": "integer",
                    "description": "The maximum number of lines to read. Defaults to reading all lines."
                }
            },
            "required": ["file_path"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    fn is_idempotent(&self) -> bool {
        true
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

        let offset = arguments
            .get("offset")
            .and_then(|v| v.as_u64())
            .map(|v| v.max(1) as usize)
            .unwrap_or(1);

        let limit = arguments
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);

        let path = Path::new(file_path);
        let content =
            context
                .filesystem
                .read_file(path)
                .await
                .map_err(|e| ToolError::FilesystemError {
                    message: e.to_string(),
                })?;

        let lines: Vec<&str> = content.lines().collect();
        let total_lines = lines.len();

        // offset is 1-based
        let start = (offset - 1).min(total_lines);
        let end = match limit {
            Some(lim) => (start + lim).min(total_lines),
            None => total_lines,
        };

        let mut output = String::new();
        for (idx, line) in lines[start..end].iter().enumerate() {
            let line_num = start + idx + 1;
            output.push_str(&format!("{line_num:>6}\t{line}\n"));
        }

        if output.is_empty() && total_lines > 0 {
            output = format!("(file has {total_lines} lines, offset {offset} is past the end)\n");
        }

        Ok(ToolOutput::success(output))
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
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            cancellation: crate::tool::CancellationChannel::never(),
        };
        (session_dir, ctx)
    }

    #[tokio::test]
    async fn read_file_basic() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("test.txt"), "line1\nline2\nline3\n").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = ReadTool;

        let result = tool
            .execute(serde_json::json!({"file_path": "test.txt"}), &ctx)
            .await
            .unwrap();

        assert!(result.content.contains("line1"));
        assert!(result.content.contains("line2"));
        assert!(result.content.contains("line3"));
        assert!(!result.is_error);
    }

    #[tokio::test]
    async fn read_file_with_offset_and_limit() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("test.txt"), "a\nb\nc\nd\ne\n").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = ReadTool;

        let result = tool
            .execute(
                serde_json::json!({"file_path": "test.txt", "offset": 2, "limit": 2}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(result.content.contains("b"));
        assert!(result.content.contains("c"));
        assert!(!result.content.contains("a"));
        assert!(!result.content.contains("d"));
    }

    #[tokio::test]
    async fn read_file_not_found() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = ReadTool;

        let result = tool
            .execute(serde_json::json!({"file_path": "nonexistent.txt"}), &ctx)
            .await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn read_file_missing_path_arg() {
        let base = TempDir::new().unwrap();
        let (_session, ctx) = make_context(&base).await;
        let tool = ReadTool;

        let result = tool.execute(serde_json::json!({}), &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }
}
