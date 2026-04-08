use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use quine_llm::{PromptCacheUsage, TokenUsage};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cache_hit_tokens: u64,
    pub estimated_cache_miss_tokens: u64,
}

impl UsageTotals {
    fn record_turn(&mut self, usage: Option<&TokenUsage>, cache_usage: Option<&PromptCacheUsage>) {
        if let Some(usage) = usage {
            self.input_tokens += usage.input_tokens;
            self.output_tokens += usage.output_tokens;
        }
        if let Some(cache_usage) = cache_usage {
            self.estimated_cache_hit_tokens += cache_usage.estimated_hit_tokens;
            self.estimated_cache_miss_tokens += cache_usage.estimated_miss_tokens;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMetricsSummary {
    pub session_id: String,
    pub updated_at: DateTime<Utc>,
    pub turn_count: u64,
    pub totals: UsageTotals,
}

impl SessionMetricsSummary {
    fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
            updated_at: Utc::now(),
            turn_count: 0,
            totals: UsageTotals::default(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HistoricalMetricsSummary {
    pub updated_at: Option<DateTime<Utc>>,
    pub session_count: u64,
    pub turn_count: u64,
    pub totals: UsageTotals,
}

pub async fn record_turn_metrics(
    session_id: &str,
    usage: Option<&TokenUsage>,
    cache_usage: Option<&PromptCacheUsage>,
) -> anyhow::Result<()> {
    record_turn_metrics_in_root(&metrics_root_dir(), session_id, usage, cache_usage).await
}

pub fn default_metrics_dir() -> PathBuf {
    metrics_root_dir()
}

pub fn default_session_metrics_path(session_id: &str) -> PathBuf {
    session_metrics_path(&metrics_root_dir(), session_id)
}

pub fn default_historical_metrics_path() -> PathBuf {
    historical_metrics_path(&metrics_root_dir())
}

async fn record_turn_metrics_in_root(
    root: &Path,
    session_id: &str,
    usage: Option<&TokenUsage>,
    cache_usage: Option<&PromptCacheUsage>,
) -> anyhow::Result<()> {
    tokio::fs::create_dir_all(session_metrics_dir(root)).await?;

    let session_path = session_metrics_path(root, session_id);
    let mut session_summary = match tokio::fs::read_to_string(&session_path).await {
        Ok(content) => serde_json::from_str::<SessionMetricsSummary>(&content)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            SessionMetricsSummary::new(session_id)
        }
        Err(error) => return Err(error.into()),
    };
    session_summary.turn_count += 1;
    session_summary.updated_at = Utc::now();
    session_summary.totals.record_turn(usage, cache_usage);
    write_json(&session_path, &session_summary).await?;

    let history_path = historical_metrics_path(root);
    let mut historical_summary = match tokio::fs::read_to_string(&history_path).await {
        Ok(content) => serde_json::from_str::<HistoricalMetricsSummary>(&content)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            HistoricalMetricsSummary::default()
        }
        Err(error) => return Err(error.into()),
    };
    if historical_summary.session_count == 0 || session_summary.turn_count == 1 {
        historical_summary.session_count += 1;
    }
    historical_summary.turn_count += 1;
    historical_summary.updated_at = Some(Utc::now());
    historical_summary.totals.record_turn(usage, cache_usage);
    write_json(&history_path, &historical_summary).await?;

    Ok(())
}

fn metrics_root_dir() -> PathBuf {
    base_dir().join("metrics")
}

fn base_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home).join(".quine")
}

fn session_metrics_dir(root: &Path) -> PathBuf {
    root.join("sessions")
}

fn session_metrics_path(root: &Path, session_id: &str) -> PathBuf {
    session_metrics_dir(root).join(format!("{session_id}.json"))
}

fn historical_metrics_path(root: &Path) -> PathBuf {
    root.join("history.json")
}

async fn write_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let bytes = serde_json::to_vec_pretty(value)?;
    tokio::fs::write(path, bytes).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[tokio::test]
    async fn record_turn_metrics_updates_session_and_history() {
        let tempdir = TempDir::new().unwrap();
        let root = tempdir.path().join("metrics");

        record_turn_metrics_in_root(
            &root,
            "session-a",
            Some(&TokenUsage {
                input_tokens: 100,
                output_tokens: 25,
            }),
            Some(&PromptCacheUsage {
                estimated_hit_tokens: 80,
                estimated_miss_tokens: 20,
            }),
        )
        .await
        .unwrap();

        record_turn_metrics_in_root(
            &root,
            "session-a",
            Some(&TokenUsage {
                input_tokens: 50,
                output_tokens: 10,
            }),
            Some(&PromptCacheUsage {
                estimated_hit_tokens: 30,
                estimated_miss_tokens: 10,
            }),
        )
        .await
        .unwrap();

        let session_summary: SessionMetricsSummary = serde_json::from_str(
            &tokio::fs::read_to_string(session_metrics_path(&root, "session-a"))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(session_summary.turn_count, 2);
        assert_eq!(session_summary.totals.input_tokens, 150);
        assert_eq!(session_summary.totals.output_tokens, 35);
        assert_eq!(session_summary.totals.estimated_cache_hit_tokens, 110);
        assert_eq!(session_summary.totals.estimated_cache_miss_tokens, 30);

        let history_summary: HistoricalMetricsSummary = serde_json::from_str(
            &tokio::fs::read_to_string(historical_metrics_path(&root))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(history_summary.session_count, 1);
        assert_eq!(history_summary.turn_count, 2);
        assert_eq!(
            history_summary.totals.input_tokens + history_summary.totals.output_tokens,
            185
        );
    }

    #[tokio::test]
    async fn record_turn_metrics_counts_distinct_sessions() {
        let tempdir = TempDir::new().unwrap();
        let root = tempdir.path().join("metrics");

        record_turn_metrics_in_root(&root, "session-a", None, None)
            .await
            .unwrap();
        record_turn_metrics_in_root(&root, "session-b", None, None)
            .await
            .unwrap();

        let history_summary: HistoricalMetricsSummary = serde_json::from_str(
            &tokio::fs::read_to_string(historical_metrics_path(&root))
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(history_summary.session_count, 2);
        assert_eq!(history_summary.turn_count, 2);
    }
}
