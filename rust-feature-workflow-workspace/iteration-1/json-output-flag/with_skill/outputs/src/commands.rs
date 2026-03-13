//! Command handler implementations.
//!
//! Each handler returns a result type that implements `Serialize + Display`,
//! allowing the [`OutputWriter`] to format it in either human or JSON mode.

use serde::Serialize;
use std::fmt;

use crate::output::OutputWriter;

// ---------------------------------------------------------------------------
// StatusResult
// ---------------------------------------------------------------------------

/// Result of the `status` command.
#[derive(Debug, Serialize)]
pub struct StatusResult {
    pub version: String,
    pub uptime_secs: u64,
    pub items_count: usize,
}

impl fmt::Display for StatusResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Version: {}", self.version)?;
        writeln!(f, "Uptime:  {} seconds", self.uptime_secs)?;
        write!(f, "Items:   {}", self.items_count)
    }
}

/// Execute the `status` command.
pub fn run_status(writer: &OutputWriter) -> Result<(), Box<dyn std::error::Error>> {
    // In a real app, these values come from the system.
    let result = StatusResult {
        version: env!("CARGO_PKG_VERSION").to_string(),
        uptime_secs: 3600,
        items_count: 42,
    };
    writer.write_result(&result)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ListResult
// ---------------------------------------------------------------------------

/// A single item in the list output.
#[derive(Debug, Serialize)]
pub struct ListItem {
    pub name: String,
    pub kind: String,
}

/// Result of the `list` command.
#[derive(Debug, Serialize)]
pub struct ListResult {
    pub items: Vec<ListItem>,
    pub total: usize,
}

impl fmt::Display for ListResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for item in &self.items {
            writeln!(f, "  {} ({})", item.name, item.kind)?;
        }
        write!(f, "Total: {} items", self.total)
    }
}

/// Execute the `list` command with an optional filter.
pub fn run_list(
    writer: &OutputWriter,
    filter: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    // In a real app, items come from storage.
    let all_items = vec![
        ListItem {
            name: "alpha".to_string(),
            kind: "widget".to_string(),
        },
        ListItem {
            name: "beta".to_string(),
            kind: "gadget".to_string(),
        },
        ListItem {
            name: "gamma".to_string(),
            kind: "widget".to_string(),
        },
    ];

    let items: Vec<ListItem> = match filter {
        Some(pattern) => all_items
            .into_iter()
            .filter(|item| item.name.contains(pattern) || item.kind.contains(pattern))
            .collect(),
        None => all_items,
    };

    let total = items.len();
    let result = ListResult { items, total };
    writer.write_result(&result)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// ShowResult
// ---------------------------------------------------------------------------

/// Result of the `show` command.
#[derive(Debug, Serialize)]
pub struct ShowResult {
    pub name: String,
    pub kind: String,
    pub description: String,
    pub created_at: String,
}

impl fmt::Display for ShowResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Name:        {}", self.name)?;
        writeln!(f, "Kind:        {}", self.kind)?;
        writeln!(f, "Description: {}", self.description)?;
        write!(f, "Created:     {}", self.created_at)
    }
}

/// Execute the `show` command.
pub fn run_show(
    writer: &OutputWriter,
    name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    // In a real app, this would look up the item by name.
    if name == "nonexistent" {
        writer.write_error("not_found", &format!("Item '{}' not found", name))?;
        return Ok(());
    }

    let result = ShowResult {
        name: name.to_string(),
        kind: "widget".to_string(),
        description: format!("Details about {}", name),
        created_at: "2026-01-15T10:30:00Z".to_string(),
    };
    writer.write_result(&result)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_result_display() {
        let result = StatusResult {
            version: "1.0.0".to_string(),
            uptime_secs: 120,
            items_count: 5,
        };
        let text = format!("{}", result);
        assert_eq!(
            text,
            "Version: 1.0.0\nUptime:  120 seconds\nItems:   5",
            "Status display must match exact expected format"
        );
    }

    #[test]
    fn test_status_result_json_serialization() {
        let result = StatusResult {
            version: "1.0.0".to_string(),
            uptime_secs: 120,
            items_count: 5,
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["version"], "1.0.0", "Version must serialize as '1.0.0'");
        assert_eq!(json["uptime_secs"], 120, "Uptime must serialize as exactly 120");
        assert_eq!(json["items_count"], 5, "Items count must serialize as exactly 5");
    }

    #[test]
    fn test_list_result_display_with_items() {
        let result = ListResult {
            items: vec![
                ListItem {
                    name: "a".to_string(),
                    kind: "widget".to_string(),
                },
                ListItem {
                    name: "b".to_string(),
                    kind: "gadget".to_string(),
                },
            ],
            total: 2,
        };
        let text = format!("{}", result);
        assert_eq!(
            text,
            "  a (widget)\n  b (gadget)\nTotal: 2 items",
            "List display must show each item on its own line with total"
        );
    }

    #[test]
    fn test_list_result_display_empty() {
        let result = ListResult {
            items: vec![],
            total: 0,
        };
        let text = format!("{}", result);
        assert_eq!(text, "Total: 0 items", "Empty list must show only the total line");
    }

    #[test]
    fn test_list_result_json_serialization() {
        let result = ListResult {
            items: vec![ListItem {
                name: "x".to_string(),
                kind: "widget".to_string(),
            }],
            total: 1,
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["total"], 1, "Total must be exactly 1");
        assert_eq!(json["items"].as_array().unwrap().len(), 1, "Items array must have exactly 1 element");
        assert_eq!(json["items"][0]["name"], "x", "First item name must be 'x'");
        assert_eq!(json["items"][0]["kind"], "widget", "First item kind must be 'widget'");
    }

    #[test]
    fn test_show_result_display() {
        let result = ShowResult {
            name: "alpha".to_string(),
            kind: "widget".to_string(),
            description: "A test item".to_string(),
            created_at: "2026-01-15T10:30:00Z".to_string(),
        };
        let text = format!("{}", result);
        assert_eq!(
            text,
            "Name:        alpha\nKind:        widget\nDescription: A test item\nCreated:     2026-01-15T10:30:00Z",
            "Show display must match exact expected format"
        );
    }

    #[test]
    fn test_show_result_json_serialization() {
        let result = ShowResult {
            name: "alpha".to_string(),
            kind: "widget".to_string(),
            description: "A test item".to_string(),
            created_at: "2026-01-15T10:30:00Z".to_string(),
        };
        let json: serde_json::Value = serde_json::to_value(&result).unwrap();
        assert_eq!(json["name"], "alpha", "Name must be exactly 'alpha'");
        assert_eq!(json["kind"], "widget", "Kind must be exactly 'widget'");
        assert_eq!(json["description"], "A test item", "Description must be exactly 'A test item'");
        assert_eq!(json["created_at"], "2026-01-15T10:30:00Z", "Timestamp must be exact");
    }
}
