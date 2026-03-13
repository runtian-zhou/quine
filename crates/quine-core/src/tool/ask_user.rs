use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

/// Schema-only tool — the dispatcher intercepts AskUserQuestion calls and handles
/// them directly (prompting the user via stdin). The `execute` method is never called.
pub struct AskUserTool;

impl Default for AskUserTool {
    fn default() -> Self {
        Self
    }
}

impl AskUserTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        "AskUserQuestion"
    }

    fn description(&self) -> &str {
        "Ask the user a question and wait for their response. Use this when you need clarification, confirmation, or additional information from the user to proceed with a task."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                }
            },
            "required": ["question"]
        })
    }

    async fn execute(&self, _arguments: Value) -> Result<ToolOutput> {
        // The dispatcher intercepts AskUserQuestion calls before reaching execute().
        // If we get here, something is misconfigured.
        anyhow::bail!("AskUserTool.execute() should never be called — the dispatcher handles user prompting directly")
    }
}
