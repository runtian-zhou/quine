use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

use crate::conversation::Entry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationLog {
    pub version: u32,
    pub created_at: DateTime<Utc>,
    pub model: String,
    pub provider: String,
    pub system_prompt: String,
    pub entries: Vec<Entry>,
}

impl ConversationLog {
    pub fn new(model: String, provider: String, system_prompt: String) -> Self {
        Self {
            version: 1,
            created_at: Utc::now(),
            model,
            provider,
            system_prompt,
            entries: Vec::new(),
        }
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json)?;
        Ok(())
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let json = fs::read_to_string(path)?;
        let log: ConversationLog = serde_json::from_str(&json)?;
        Ok(log)
    }
}
