use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// A single log entry for a session event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionLogEntry {
    /// When the event occurred.
    pub timestamp: DateTime<Utc>,
    /// The session this event belongs to.
    pub session_id: String,
    /// The type of event (e.g., "stream_delta", "user_message", "turn_complete").
    pub event_type: String,
    /// Whether the event is inbound (from user/harness) or outbound (from core/LLM).
    pub direction: EventDirection,
    /// The event payload.
    pub payload: serde_json::Value,
}

/// Direction of a session event.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventDirection {
    /// From user/client to the agent.
    Inbound,
    /// From the agent to the user/client.
    Outbound,
}

/// Summary of a session for listing purposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSummary {
    /// The session ID.
    pub session_id: String,
    /// When the session was created (first log entry timestamp).
    pub created_at: DateTime<Utc>,
    /// When the session was last active (last log entry timestamp).
    pub last_active: DateTime<Utc>,
    /// Number of log entries.
    pub entry_count: usize,
}

/// Returns the default log directory: `~/.quine/logs/`.
pub fn default_log_dir() -> PathBuf {
    dirs_path().join("logs")
}

/// Returns `~/.quine/`.
fn dirs_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".quine")
}

/// Returns the log file path for a given session ID.
pub fn log_file_path(session_id: &str) -> PathBuf {
    default_log_dir().join(format!("{session_id}.jsonl"))
}

/// Append a log entry to the session's JSONL file.
///
/// Creates the log directory and file if they don't exist.
pub async fn append_log_entry(entry: &SessionLogEntry) -> anyhow::Result<()> {
    let log_dir = default_log_dir();
    tokio::fs::create_dir_all(&log_dir).await?;

    let path = log_file_path(&entry.session_id);
    let mut line = serde_json::to_string(entry)?;
    line.push('\n');

    tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .await?
        .write_all_buf(&mut line.as_bytes())
        .await?;

    Ok(())
}

/// Read all log entries for a session.
pub async fn read_session_log(session_id: &str) -> anyhow::Result<Vec<SessionLogEntry>> {
    let path = log_file_path(session_id);
    if !path.exists() {
        anyhow::bail!("no log file for session {session_id}");
    }

    let content = tokio::fs::read_to_string(&path).await?;
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: SessionLogEntry = serde_json::from_str(line)?;
        entries.push(entry);
    }
    Ok(entries)
}

/// List all sessions that have log files, with summaries.
pub async fn list_sessions() -> anyhow::Result<Vec<SessionSummary>> {
    let log_dir = default_log_dir();
    if !log_dir.exists() {
        return Ok(Vec::new());
    }

    let mut summaries = Vec::new();
    let mut dir = tokio::fs::read_dir(&log_dir).await?;

    while let Some(entry) = dir.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }

        let session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        if let Ok(summary) = summarize_log_file(&path, &session_id).await {
            summaries.push(summary);
        }
    }

    // Sort by last_active descending.
    summaries.sort_by(|a, b| b.last_active.cmp(&a.last_active));
    Ok(summaries)
}

/// Read a JSONL log file and produce a summary.
async fn summarize_log_file(path: &Path, session_id: &str) -> anyhow::Result<SessionSummary> {
    let content = tokio::fs::read_to_string(path).await?;
    let mut first_ts: Option<DateTime<Utc>> = None;
    let mut last_ts: Option<DateTime<Utc>> = None;
    let mut count = 0usize;

    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(entry) = serde_json::from_str::<SessionLogEntry>(line) {
            if first_ts.is_none() {
                first_ts = Some(entry.timestamp);
            }
            last_ts = Some(entry.timestamp);
            count += 1;
        }
    }

    let now = Utc::now();
    Ok(SessionSummary {
        session_id: session_id.to_string(),
        created_at: first_ts.unwrap_or(now),
        last_active: last_ts.unwrap_or(now),
        entry_count: count,
    })
}

use tokio::io::AsyncWriteExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_entry_serialization() {
        let entry = SessionLogEntry {
            timestamp: Utc::now(),
            session_id: "test-session".to_string(),
            event_type: "user_message".to_string(),
            direction: EventDirection::Inbound,
            payload: serde_json::json!({"content": "hello"}),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: SessionLogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "test-session");
        assert_eq!(parsed.event_type, "user_message");
    }

    #[test]
    fn session_summary_serialization() {
        let summary = SessionSummary {
            session_id: "abc".to_string(),
            created_at: Utc::now(),
            last_active: Utc::now(),
            entry_count: 5,
        };

        let json = serde_json::to_string(&summary).unwrap();
        let parsed: SessionSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "abc");
        assert_eq!(parsed.entry_count, 5);
    }

    #[test]
    fn default_log_dir_is_under_home() {
        let dir = default_log_dir();
        let dir_str = dir.to_string_lossy();
        assert!(dir_str.contains(".quine") && dir_str.contains("logs"));
    }
}
