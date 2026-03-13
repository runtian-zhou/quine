pub mod ask_user;
pub mod bash;
pub mod edit;
pub mod glob;
pub mod grep;
pub mod list_directory;
pub mod read;
pub mod skill;
pub mod subagent;
pub mod todo;
pub mod web_fetch;
pub mod web_search;
pub mod write;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

use crate::conversation::ToolOutput;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, arguments: Value) -> Result<ToolOutput>;
}

/// Shared state that persists across the parent agent and all subagents.
/// Clone is cheap — every field is an Arc.
#[derive(Clone)]
pub struct GlobalContext {
    pub todo: Arc<todo::TodoTool>,
}

impl Default for GlobalContext {
    fn default() -> Self {
        Self::new()
    }
}

impl GlobalContext {
    pub fn new() -> Self {
        Self {
            todo: Arc::new(todo::TodoTool::new()),
        }
    }
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    pub fn all_schemas(&self) -> Vec<Value> {
        self.tools
            .values()
            .map(|tool| {
                serde_json::json!({
                    "name": tool.name(),
                    "description": tool.description(),
                    "input_schema": tool.parameters_schema(),
                })
            })
            .collect()
    }

    /// Build a registry with stateless defaults plus a fresh GlobalContext.
    /// Use this when you don't need to share state with another registry.
    pub fn register_defaults(working_dir: &std::path::Path) -> Self {
        Self::register_defaults_with_context(working_dir, &GlobalContext::new())
    }

    /// Build a registry that shares the given GlobalContext.
    /// Pass the same context to every registry (parent + subagents) so stateful
    /// tools like Todo are shared across all of them.
    pub fn register_defaults_with_context(
        working_dir: &std::path::Path,
        ctx: &GlobalContext,
    ) -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(bash::BashTool::new(working_dir)));
        registry.register(Box::new(read::ReadTool::new(working_dir)));
        registry.register(Box::new(write::WriteTool::new(working_dir)));
        registry.register(Box::new(edit::EditTool::new(working_dir)));
        registry.register(Box::new(glob::GlobTool::new(working_dir)));
        registry.register(Box::new(grep::GrepTool::new(working_dir)));
        registry.register(Box::new(list_directory::ListDirectoryTool::new(
            working_dir,
        )));
        registry.register(Box::new(skill::SkillTool::new(working_dir)));
        // Register the shared Arc — all registries built from the same context
        // point at the same TodoTool instance.
        registry.register(Box::new(Arc::clone(&ctx.todo)));
        registry.register(Box::new(web_fetch::WebFetchTool::new()));
        registry.register(Box::new(web_search::WebSearchTool::new()));
        registry
    }
}
