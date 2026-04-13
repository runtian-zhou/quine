use async_trait::async_trait;

use crate::python::PythonExecRequest;
use crate::tool::{ExecutionContext, Tool, ToolError, ToolOutput};

pub(crate) struct PythonExecTool;

#[async_trait]
impl Tool for PythonExecTool {
    fn name(&self) -> &str {
        "python_exec"
    }

    fn description(&self) -> &str {
        "Execute Python code or call a Python function inside the current session group's shared Python environment."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "code": { "type": "string" },
                "function": { "type": "string" },
                "args": { "type": "array", "items": {} },
                "kwargs": { "type": "object" }
            }
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let request: PythonExecRequest =
            serde_json::from_value(arguments).map_err(|error| ToolError::InvalidArguments {
                message: error.to_string(),
            })?;
        let result = context
            .python_runtime
            .exec(&context.session_group, &request)
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
