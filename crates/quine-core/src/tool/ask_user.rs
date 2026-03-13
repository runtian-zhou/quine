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
        "Ask the user a question and wait for their response. Supports three modes: (1) freeform text input when only 'question' is provided, (2) single/multiple selection when 'options' is provided — the user picks by number, (3) selection with freeform fallback when both 'options' and 'allow_text' are true."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "The question to ask the user"
                },
                "options": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Optional list of choices. The user selects by entering one or more numbers (comma-separated for multiple). Omit for freeform text input."
                },
                "allow_text": {
                    "type": "boolean",
                    "description": "When true and options are provided, the user may also type freeform text instead of picking a number. Default: false."
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
