use async_trait::async_trait;

use crate::tool::{ExecutionContext, Tool, ToolError, ToolOutput};

pub(crate) struct PythonInspectGlobalTool;

#[async_trait]
impl Tool for PythonInspectGlobalTool {
    fn name(&self) -> &str {
        "python_inspect_global"
    }

    fn description(&self) -> &str {
        "Inspect a variable, function, class, or method exposed in the current session group's shared Python environment."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" }
            },
            "required": ["name"]
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
        let name = arguments
            .get("name")
            .and_then(|value| value.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing required parameter: name".into(),
            })?;
        let result = context
            .python_runtime
            .inspect(&context.session_group, name)
            .await
            .map_err(|error| ToolError::Internal {
                message: error.to_string(),
            })?;
        serde_json::to_string(&result)
            .map(ToolOutput::success)
            .map_err(|error| ToolError::Internal {
                message: error.to_string(),
            })
    }
}
