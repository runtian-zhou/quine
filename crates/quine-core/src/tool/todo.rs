use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Mutex;

use crate::conversation::ToolOutput;
use crate::tool::Tool;

#[derive(Debug, Clone)]
struct TodoItem {
    id: usize,
    content: String,
    status: TodoStatus,
    priority: Priority,
}

#[derive(Debug, Clone, PartialEq)]
enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

impl std::fmt::Display for TodoStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TodoStatus::Pending => write!(f, "pending"),
            TodoStatus::InProgress => write!(f, "in_progress"),
            TodoStatus::Done => write!(f, "done"),
        }
    }
}

impl TodoStatus {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TodoStatus::Pending),
            "in_progress" => Some(TodoStatus::InProgress),
            "done" => Some(TodoStatus::Done),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Priority {
    High,
    Medium,
    Low,
}

impl std::fmt::Display for Priority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Priority::High => write!(f, "high"),
            Priority::Medium => write!(f, "medium"),
            Priority::Low => write!(f, "low"),
        }
    }
}

impl Priority {
    fn from_str(s: &str) -> Self {
        match s {
            "high" => Priority::High,
            "low" => Priority::Low,
            _ => Priority::Medium,
        }
    }
}

pub struct TodoTool {
    items: Mutex<Vec<TodoItem>>,
    next_id: Mutex<usize>,
}

impl TodoTool {
    pub fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
            next_id: Mutex::new(1),
        }
    }

    fn add(&self, content: &str, priority: &str) -> String {
        let mut items = self.items.lock().unwrap();
        let mut next_id = self.next_id.lock().unwrap();
        let id = *next_id;
        *next_id += 1;
        let item = TodoItem {
            id,
            content: content.to_string(),
            status: TodoStatus::Pending,
            priority: Priority::from_str(priority),
        };
        items.push(item);
        format!("Added todo #{}: {}", id, content)
    }

    fn update(&self, id: usize, status: &str) -> String {
        let mut items = self.items.lock().unwrap();
        let Some(status_enum) = TodoStatus::from_str(status) else {
            return format!(
                "Invalid status '{}'. Use: pending, in_progress, done",
                status
            );
        };
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.status = status_enum;
            format!("Updated todo #{} to {}", id, status)
        } else {
            format!("Todo #{} not found", id)
        }
    }

    fn remove(&self, id: usize) -> String {
        let mut items = self.items.lock().unwrap();
        let len_before = items.len();
        items.retain(|i| i.id != id);
        if items.len() < len_before {
            format!("Removed todo #{}", id)
        } else {
            format!("Todo #{} not found", id)
        }
    }

    fn list(&self) -> String {
        let items = self.items.lock().unwrap();
        if items.is_empty() {
            return "No todos.".to_string();
        }
        let mut lines = Vec::new();
        for item in items.iter() {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Done => "[x]",
            };
            lines.push(format!(
                "{} #{} ({}, {}) {}",
                marker, item.id, item.status, item.priority, item.content
            ));
        }
        lines.join("\n")
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "Todo"
    }

    fn description(&self) -> &str {
        "Manage a todo list for planning and tracking tasks. Supports add, update, remove, and list operations."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "update", "remove", "list"],
                    "description": "The action to perform"
                },
                "content": {
                    "type": "string",
                    "description": "The todo item text (required for 'add')"
                },
                "id": {
                    "type": "integer",
                    "description": "The todo item ID (required for 'update' and 'remove')"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "done"],
                    "description": "The new status (required for 'update')"
                },
                "priority": {
                    "type": "string",
                    "enum": ["high", "medium", "low"],
                    "description": "Priority level (optional for 'add', defaults to 'medium')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let action = arguments["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("action is required"))?;

        let output = match action {
            "add" => {
                let content = arguments["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("content is required for 'add'"))?;
                let priority = arguments["priority"].as_str().unwrap_or("medium");
                self.add(content, priority)
            }
            "update" => {
                let id = arguments["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("id is required for 'update'"))? as usize;
                let status = arguments["status"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("status is required for 'update'"))?;
                self.update(id, status)
            }
            "remove" => {
                let id = arguments["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("id is required for 'remove'"))? as usize;
                self.remove(id)
            }
            "list" => self.list(),
            _ => format!("Unknown action '{}'. Use: add, update, remove, list", action),
        };

        Ok(ToolOutput {
            success: true,
            output,
        })
    }
}
