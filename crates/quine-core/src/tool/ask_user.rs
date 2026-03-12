use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

/// An async function that displays a question to the user and returns their response.
pub type AskUserFn =
    Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = Result<String>> + Send>> + Send + Sync>;

pub struct AskUserTool {
    ask_fn: AskUserFn,
}

impl AskUserTool {
    pub fn new(ask_fn: AskUserFn) -> Self {
        Self { ask_fn }
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

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let question = arguments["question"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("question is required"))?;

        match (self.ask_fn)(question.to_string()).await {
            Ok(response) => Ok(ToolOutput {
                success: true,
                output: response,
            }),
            Err(e) => Ok(ToolOutput {
                success: false,
                output: format!("Failed to get user response: {}", e),
            }),
        }
    }
}
