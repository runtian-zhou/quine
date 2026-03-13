use serde::Serialize;
use std::fmt;

use crate::output::{self, OutputFormat};

#[derive(Debug, Serialize)]
pub struct ListItem {
    pub name: String,
    pub kind: String,
    pub size_bytes: u64,
}

impl fmt::Display for ListItem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let icon = match self.kind.as_str() {
            "directory" => "d",
            "file" => "-",
            _ => "?",
        };
        write!(f, "{icon} {:>8}  {}", self.size_bytes, self.name)
    }
}

pub fn run(path: &str, format: &OutputFormat) -> anyhow::Result<()> {
    let items = gather_items(path)?;
    output::render_list(&items, format)
}

fn gather_items(path: &str) -> anyhow::Result<Vec<ListItem>> {
    let rd = std::fs::read_dir(path)?;
    let mut items = Vec::new();
    for entry in rd {
        let entry = entry?;
        let meta = entry.metadata()?;
        let kind = if meta.is_dir() {
            "directory"
        } else {
            "file"
        };
        items.push(ListItem {
            name: entry.file_name().to_string_lossy().into_owned(),
            kind: kind.to_string(),
            size_bytes: meta.len(),
        });
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}
