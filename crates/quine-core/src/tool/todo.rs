use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use crate::conversation::ToolOutput;
use crate::tool::Tool;

#[derive(Debug, Clone)]
struct TodoItem {
    id: usize,
    content: String,
    status: TodoStatus,
    priority: Priority,
    /// IDs of tasks that must be `done` before this one can start.
    depends_on: Vec<usize>,
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

    /// Returns `Err` if adding an edge from each dep -> new_id would create a cycle.
    /// We do a simple reachability check: if `new_id` can already reach any dep, it's a cycle.
    /// Since `new_id` doesn't exist yet, we just check that no dep transitively depends on
    /// any other dep in a way that would form a loop — actually we just need to ensure the
    /// deps themselves are valid existing IDs and that the new node won't be in its own
    /// dependency chain (impossible for a brand-new node). So the only real cycle risk is
    /// if `depends_on` contains the new id itself, which we reject explicitly.
    fn would_create_cycle(_items: &[TodoItem], new_id: usize, depends_on: &[usize]) -> bool {
        // A brand-new node can't be part of an existing cycle; the only invalid case is
        // self-dependency.
        depends_on.contains(&new_id)
    }

    /// Check for cycles when adding a dependency edge to an *existing* node.
    /// Returns true if making `target_id` depend on `dep_id` would create a cycle
    /// (i.e., `target_id` is already reachable from `dep_id`).
    fn edge_creates_cycle(items: &[TodoItem], target_id: usize, dep_id: usize) -> bool {
        // BFS/DFS from dep_id following depends_on edges; if we reach target_id, it's a cycle.
        let map: HashMap<usize, &TodoItem> = items.iter().map(|i| (i.id, i)).collect();
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(dep_id);
        while let Some(cur) = queue.pop_front() {
            if cur == target_id {
                return true;
            }
            if visited.insert(cur) {
                if let Some(item) = map.get(&cur) {
                    for &d in &item.depends_on {
                        queue.push_back(d);
                    }
                }
            }
        }
        false
    }

    fn add(&self, content: &str, priority: &str, depends_on: Vec<usize>) -> (bool, String) {
        let mut items = self.items.lock().unwrap();
        let mut next_id = self.next_id.lock().unwrap();
        let id = *next_id;

        // Validate all dep IDs exist.
        for &dep in &depends_on {
            if !items.iter().any(|i| i.id == dep) {
                return (false, format!("Dependency #{} not found", dep));
            }
        }

        if Self::would_create_cycle(&items, id, &depends_on) {
            return (false, "Cannot add: self-dependency detected".to_string());
        }

        *next_id += 1;
        items.push(TodoItem {
            id,
            content: content.to_string(),
            status: TodoStatus::Pending,
            priority: Priority::from_str(priority),
            depends_on,
        });
        (true, format!("Added todo #{}: {}", id, content))
    }

    fn update(&self, id: usize, status: &str) -> (bool, String) {
        let mut items = self.items.lock().unwrap();
        let Some(status_enum) = TodoStatus::from_str(status) else {
            return (false, format!("Invalid status '{}'. Use: pending, in_progress, done", status));
        };

        // If moving to in_progress, check all deps are done.
        if status_enum == TodoStatus::InProgress {
            let item = match items.iter().find(|i| i.id == id) {
                Some(i) => i.clone(),
                None => return (false, format!("Todo #{} not found", id)),
            };
            let map: HashMap<usize, &TodoItem> =
                items.iter().map(|i| (i.id, i)).collect();
            let blocking: Vec<usize> = item
                .depends_on
                .iter()
                .filter(|&&dep| {
                    map.get(&dep)
                        .map(|d| d.status != TodoStatus::Done)
                        .unwrap_or(false)
                })
                .copied()
                .collect();
            if !blocking.is_empty() {
                let ids: Vec<String> = blocking.iter().map(|i| format!("#{}", i)).collect();
                return (false, format!(
                    "Cannot start #{}: blocked by {}",
                    id,
                    ids.join(", ")
                ));
            }
        }

        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            item.status = status_enum;
            (true, format!("Updated todo #{} to {}", id, status))
        } else {
            (false, format!("Todo #{} not found", id))
        }
    }

    fn add_dep(&self, id: usize, dep_id: usize) -> (bool, String) {
        let mut items = self.items.lock().unwrap();
        if !items.iter().any(|i| i.id == dep_id) {
            return (false, format!("Dependency #{} not found", dep_id));
        }
        if !items.iter().any(|i| i.id == id) {
            return (false, format!("Todo #{} not found", id));
        }
        if id == dep_id {
            return (false, "Cannot depend on itself".to_string());
        }
        if Self::edge_creates_cycle(&items, id, dep_id) {
            return (false, format!("Cannot add dependency: would create a cycle between #{} and #{}", id, dep_id));
        }
        let item = items.iter_mut().find(|i| i.id == id).unwrap();
        if item.depends_on.contains(&dep_id) {
            return (false, format!("#{} already depends on #{}", id, dep_id));
        }
        item.depends_on.push(dep_id);
        (true, format!("#{} now depends on #{}", id, dep_id))
    }

    fn remove_dep(&self, id: usize, dep_id: usize) -> (bool, String) {
        let mut items = self.items.lock().unwrap();
        if let Some(item) = items.iter_mut().find(|i| i.id == id) {
            let before = item.depends_on.len();
            item.depends_on.retain(|&d| d != dep_id);
            if item.depends_on.len() < before {
                (true, format!("Removed dependency #{} -> #{}", id, dep_id))
            } else {
                (false, format!("#{} does not depend on #{}", id, dep_id))
            }
        } else {
            (false, format!("Todo #{} not found", id))
        }
    }

    fn remove(&self, id: usize) -> (bool, String) {
        let mut items = self.items.lock().unwrap();
        // Also strip this id from other items' depends_on.
        for item in items.iter_mut() {
            item.depends_on.retain(|&d| d != id);
        }
        let len_before = items.len();
        items.retain(|i| i.id != id);
        if items.len() < len_before {
            (true, format!("Removed todo #{}", id))
        } else {
            (false, format!("Todo #{} not found", id))
        }
    }

    fn list(&self) -> (bool, String) {
        let items = self.items.lock().unwrap();
        if items.is_empty() {
            return (true, "No todos.".to_string());
        }
        let map: HashMap<usize, &TodoItem> = items.iter().map(|i| (i.id, i)).collect();
        let mut lines = Vec::new();
        for item in items.iter() {
            let marker = match item.status {
                TodoStatus::Pending => "[ ]",
                TodoStatus::InProgress => "[~]",
                TodoStatus::Done => "[x]",
            };
            let deps_str = if item.depends_on.is_empty() {
                String::new()
            } else {
                let dep_labels: Vec<String> = item
                    .depends_on
                    .iter()
                    .map(|&d| {
                        let done = map
                            .get(&d)
                            .map(|dep| dep.status == TodoStatus::Done)
                            .unwrap_or(false);
                        if done {
                            format!("#{} ✓", d)
                        } else {
                            format!("#{} ✗", d)
                        }
                    })
                    .collect();
                format!(" [deps: {}]", dep_labels.join(", "))
            };
            lines.push(format!(
                "{} #{} ({}, {}){} {}",
                marker, item.id, item.status, item.priority, deps_str, item.content
            ));
        }
        (true, lines.join("\n"))
    }
}

#[async_trait]
impl Tool for Arc<TodoTool> {
    fn name(&self) -> &str {
        "Todo"
    }

    fn description(&self) -> &str {
        "Manage a DAG-structured todo list. Tasks can declare dependencies on other tasks. \
         Supports add, update, remove, list, add_dep, and remove_dep operations."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "update", "remove", "list", "add_dep", "remove_dep"],
                    "description": "The action to perform"
                },
                "content": {
                    "type": "string",
                    "description": "The todo item text (required for 'add')"
                },
                "id": {
                    "type": "integer",
                    "description": "The todo item ID (required for 'update', 'remove', 'add_dep', 'remove_dep')"
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
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "List of task IDs this task depends on (optional for 'add')"
                },
                "dep_id": {
                    "type": "integer",
                    "description": "The dependency task ID (required for 'add_dep' and 'remove_dep')"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, arguments: Value) -> Result<ToolOutput> {
        let action = arguments["action"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("action is required"))?;

        let (success, output) = match action {
            "add" => {
                let content = arguments["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("content is required for 'add'"))?;
                let priority = arguments["priority"].as_str().unwrap_or("medium");
                let depends_on: Vec<usize> = arguments["depends_on"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_u64().map(|n| n as usize))
                            .collect()
                    })
                    .unwrap_or_default();
                self.add(content, priority, depends_on)
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
            "add_dep" => {
                let id = arguments["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("id is required for 'add_dep'"))? as usize;
                let dep_id = arguments["dep_id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("dep_id is required for 'add_dep'"))? as usize;
                self.add_dep(id, dep_id)
            }
            "remove_dep" => {
                let id = arguments["id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("id is required for 'remove_dep'"))? as usize;
                let dep_id = arguments["dep_id"]
                    .as_u64()
                    .ok_or_else(|| anyhow::anyhow!("dep_id is required for 'remove_dep'"))? as usize;
                self.remove_dep(id, dep_id)
            }
            _ => (false, format!(
                "Unknown action '{}'. Use: add, update, remove, list, add_dep, remove_dep",
                action
            )),
        };

        Ok(ToolOutput {
            success,
            output,
        })
    }
}
