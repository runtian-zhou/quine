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
pub mod write;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;

use crate::conversation::ToolOutput;

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    async fn execute(&self, arguments: Value) -> Result<ToolOutput>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
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

    pub fn register_defaults(working_dir: &std::path::Path) -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(bash::BashTool::new(working_dir)));
        registry.register(Box::new(read::ReadTool::new(working_dir)));
        registry.register(Box::new(write::WriteTool::new(working_dir)));
        registry.register(Box::new(edit::EditTool::new(working_dir)));
        registry.register(Box::new(glob::GlobTool::new(working_dir)));
        registry.register(Box::new(grep::GrepTool::new(working_dir)));
        registry.register(Box::new(list_directory::ListDirectoryTool::new(working_dir)));
        registry.register(Box::new(skill::SkillTool::new(working_dir)));
        registry.register(Box::new(todo::TodoTool::new()));
        registry
    }
}
