use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::session::SessionId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MemoryScope {
    User,
    Project { root: PathBuf },
    Session { session_id: SessionId },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryRecord {
    pub id: String,
    pub scope: MemoryScope,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct MemoryDocument {
    pub schema_version: u32,
    #[serde(default)]
    pub records: Vec<MemoryRecord>,
}

#[async_trait]
pub trait MemoryService: Send + Sync {
    async fn load_applicable(
        &self,
        working_directory: &Path,
        session_id: SessionId,
    ) -> Result<Vec<MemoryRecord>>;

    async fn list(&self, scope: &MemoryScope) -> Result<Vec<MemoryRecord>>;

    async fn upsert(&self, record: MemoryRecord) -> Result<MemoryRecord>;

    async fn delete(&self, scope: &MemoryScope, id: &str) -> Result<()>;
}

pub fn render_memory_section(records: &[MemoryRecord]) -> Option<String> {
    if records.is_empty() {
        return None;
    }

    let mut rendered = String::from("## Memory\n");
    for record in records {
        rendered.push_str("- ");
        rendered.push_str(&record.title);
        rendered.push_str(": ");
        rendered.push_str(record.body.trim());
        if !record.tags.is_empty() {
            rendered.push_str(" [tags: ");
            rendered.push_str(&record.tags.join(", "));
            rendered.push(']');
        }
        rendered.push('\n');
    }

    Some(rendered.trim_end().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> MemoryRecord {
        MemoryRecord {
            id: "rust-style".into(),
            scope: MemoryScope::User,
            title: "Rust style".into(),
            body: "Use anyhow for app-level errors.".into(),
            tags: vec!["rust".into(), "style".into()],
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn memory_record_round_trips_through_json() {
        let record = sample_record();
        let json = serde_json::to_string(&record).unwrap();
        let parsed: MemoryRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.id, record.id);
        assert_eq!(parsed.scope, record.scope);
        assert_eq!(parsed.title, record.title);
        assert_eq!(parsed.body, record.body);
        assert_eq!(parsed.tags, record.tags);
    }

    #[test]
    fn render_memory_section_returns_none_when_empty() {
        assert_eq!(render_memory_section(&[]), None);
    }

    #[test]
    fn render_memory_section_is_deterministic() {
        let rendered = render_memory_section(&[sample_record()]).unwrap();
        assert_eq!(
            rendered,
            "## Memory\n- Rust style: Use anyhow for app-level errors. [tags: rust, style]"
        );
    }
}
