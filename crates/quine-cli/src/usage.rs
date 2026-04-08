use std::path::Path;

use quine_harness::{
    default_historical_metrics_path, default_metrics_dir, default_session_metrics_path,
    HistoricalMetricsSummary, SessionMetricsSummary, UsageTotals,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct UsageView<'a, T> {
    scope: &'a str,
    path: String,
    summary: T,
}

pub async fn show_usage(session_id: Option<&str>, json: bool) -> anyhow::Result<()> {
    match session_id {
        Some(session_id) => {
            let path = default_session_metrics_path(session_id);
            let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
                anyhow::anyhow!(
                    "failed to read usage metrics for session {session_id} at {}: {error}",
                    path.display()
                )
            })?;
            let summary: SessionMetricsSummary = serde_json::from_str(&content)?;
            if json {
                let view = UsageView {
                    scope: "session",
                    path: path.display().to_string(),
                    summary,
                };
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                print_session_usage(&path, &summary);
            }
        }
        None => {
            let path = default_historical_metrics_path();
            let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
                anyhow::anyhow!(
                    "failed to read usage history at {}: {error}",
                    path.display()
                )
            })?;
            let summary: HistoricalMetricsSummary = serde_json::from_str(&content)?;
            if json {
                let view = UsageView {
                    scope: "history",
                    path: path.display().to_string(),
                    summary,
                };
                println!("{}", serde_json::to_string_pretty(&view)?);
            } else {
                print_history_usage(&path, &summary);
            }
        }
    }

    Ok(())
}

fn print_history_usage(path: &Path, summary: &HistoricalMetricsSummary) {
    println!("Usage history: {}", path.display());
    println!("Metrics root: {}", default_metrics_dir().display());
    println!(
        "Updated: {}",
        format_optional_timestamp(summary.updated_at.as_ref())
    );
    println!("Sessions: {}", summary.session_count);
    println!("Turns: {}", summary.turn_count);
    print_totals(&summary.totals);
}

fn print_session_usage(path: &Path, summary: &SessionMetricsSummary) {
    println!("Session usage: {}", path.display());
    println!("Metrics root: {}", default_metrics_dir().display());
    println!("Session ID: {}", summary.session_id);
    println!(
        "Updated: {}",
        summary.updated_at.format("%Y-%m-%d %H:%M:%S UTC")
    );
    println!("Turns: {}", summary.turn_count);
    print_totals(&summary.totals);
}

fn print_totals(totals: &UsageTotals) {
    println!("Input tokens: {}", totals.input_tokens);
    println!("Output tokens: {}", totals.output_tokens);
    println!(
        "Estimated cache hit tokens: {}",
        totals.estimated_cache_hit_tokens
    );
    println!(
        "Estimated cache miss tokens: {}",
        totals.estimated_cache_miss_tokens
    );
}

fn format_optional_timestamp(timestamp: Option<&chrono::DateTime<chrono::Utc>>) -> String {
    timestamp
        .map(|value| value.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "never".to_string())
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    #[test]
    fn format_optional_timestamp_handles_missing_values() {
        assert_eq!(format_optional_timestamp(None), "never");
    }

    #[test]
    fn format_optional_timestamp_formats_present_values() {
        let timestamp = Utc::now();
        assert!(format_optional_timestamp(Some(&timestamp)).ends_with("UTC"));
    }
}
