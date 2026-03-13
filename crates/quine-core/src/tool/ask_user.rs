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
        "Ask the user a question and wait for their response. Supports three modes: (1) freeform text input when only 'question' is provided, (2) single selection (default) or multiple selection (multi_select: true) when 'options' is provided — shown as an interactive arrow-key selector, (3) selection with freeform fallback when 'allow_text' is true."
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
                    "description": "Optional list of choices presented as an interactive selector. Omit for freeform text input."
                },
                "multi_select": {
                    "type": "boolean",
                    "description": "When true and options are provided, the user can select multiple options. When false (default), only a single option can be selected."
                },
                "allow_text": {
                    "type": "boolean",
                    "description": "When true and options are provided, the user may also type freeform text instead of picking from the list. Default: false."
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
