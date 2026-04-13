use async_trait::async_trait;

use crate::tool::{ExecutionContext, Tool, ToolError, ToolOutput};

pub(crate) struct PythonListGlobalsTool;

#[async_trait]
impl Tool for PythonListGlobalsTool {
    fn name(&self) -> &str {
        "python_list_globals"
    }

    fn description(&self) -> &str {
        "List variables, functions, classes, and bound methods exposed in the current session group's shared Python environment."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": [],
            "additionalProperties": false
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
        _arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let result = context
            .python_runtime
            .list_globals(&context.session_group)
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
