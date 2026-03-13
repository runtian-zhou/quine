use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

/// Schema-only tool — the dispatcher intercepts Subagent calls and handles them
/// directly (spawning a child agent). The `execute` method is never called.
pub struct SubagentTool;

impl Default for SubagentTool {
    fn default() -> Self {
        Self
    }
}

impl SubagentTool {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Tool for SubagentTool {
    fn name(&self) -> &str {
        "Subagent"
    }

    fn description(&self) -> &str {
        "Spawn a subagent to handle a subtask. The subagent has access to the same tools (Read, Write, Edit, Glob, Grep, Todo) but cannot spawn further subagents. Use this to delegate independent subtasks like research, file exploration, or isolated edits. Set worktree=true to run in an isolated git worktree."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "The task prompt for the subagent. Be specific about what you want it to accomplish."
                },
                "worktree": {
                    "type": "boolean",
                    "description": "If true, run the subagent in an isolated git worktree. Changes stay in the worktree and don't affect the main working tree. Default: false."
                }
            },
            "required": ["prompt"]
        })
    }

    async fn execute(&self, _arguments: Value) -> Result<ToolOutput> {
        // The dispatcher intercepts Subagent tool calls before reaching execute().
        // If we get here, something is misconfigured.
        anyhow::bail!("SubagentTool.execute() should never be called — the dispatcher handles subagent spawning directly")
    }
}
