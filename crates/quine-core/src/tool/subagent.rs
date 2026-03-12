use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

/// An async function that takes a prompt and returns the final assistant response text.
/// The implementation is responsible for running the full agent loop (LLM calls + tool execution).
pub type CompletionFn =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

pub struct SubagentTool {
    completion_fn: CompletionFn,
}

impl SubagentTool {
    pub fn new(completion_fn: CompletionFn) -> Self {
        Self { completion_fn }
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "Subagent"
    }

    fn description(&self) -> &str {
        "Spawn a subagent to handle a subtask. The subagent has access to the same tools (Read, Write, Edit, Glob, Grep, Todo) but cannot spawn further subagents. Use this to delegate independent subtasks like research, file exploration, or isolated edits."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the subagent. Be specific about what you want it to accomplish."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let prompt = arguments["prompt"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("prompt is required"))?;

        match (self.completion_fn)(prompt.to_string()).await {
            Ok(response) => Ok(ToolOutput {
                success: true,
                output: response,
            }),
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Subagent error: {}", e),
            }),
        }
    }
}
