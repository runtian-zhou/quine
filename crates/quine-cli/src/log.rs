use std::path::Path;

use crate::client::IpcClient;
use quine_harness::protocol::methods;
use quine_harness::session_log::{self, SessionLogEntry, SessionSummary};

/// Dump the session log for a given session ID.
///
/// Tries to read the log directly from the filesystem first. If a socket path
/// is provided and the direct read fails, falls back to an RPC call.
pub async fn dump_session_log(session_id: &str, socket_path: Option<&Path>) -> anyhow::Result<()> {
    // Try reading directly from the log file first.
    match session_log::read_session_log(session_id).await {
        Ok(entries) => {
            print_log_entries(&entries);
            return Ok(());
        }
        Err(_) if socket_path.is_some() => {
            // Fall back to RPC call.
        }
        Err(e) => return Err(e),
    }

    // RPC fallback.
    let socket_path = socket_path.unwrap();
    let mut client = IpcClient::connect(socket_path).await?;
    let params = serde_json::json!({"session_id": session_id});
    let result = client.call(methods::GET_SESSION_LOG, Some(params)).await?;

    match result {
        Ok(value) => {
            let entries: Vec<SessionLogEntry> = serde_json::from_value(value)?;
            print_log_entries(&entries);
        }
        Err(e) => anyhow::bail!("failed to get session log: {e}"),
    }

    Ok(())
}

/// List all sessions with summaries.
///
/// Reads directly from the log directory.
pub async fn list_session_logs(socket_path: Option<&Path>) -> anyhow::Result<()> {
    // Try reading directly from the log directory first.
    match session_log::list_sessions().await {
        Ok(summaries) if !summaries.is_empty() => {
            print_session_list(&summaries);
            return Ok(());
        }
        Ok(_) => {
            // Empty list — try RPC fallback if available.
        }
        Err(_) if socket_path.is_some() => {
            // Fall back to RPC.
        }
        Err(e) => return Err(e),
    }

    // RPC fallback.
    if let Some(socket_path) = socket_path {
        let mut client = IpcClient::connect(socket_path).await?;
        let result = client.call(methods::LIST_SESSIONS, None).await?;

        match result {
            Ok(value) => {
                let summaries: Vec<SessionSummary> = serde_json::from_value(value)?;
                if summaries.is_empty() {
                    eprintln!("No sessions found.");
                } else {
                    print_session_list(&summaries);
                }
            }
            Err(e) => anyhow::bail!("failed to list sessions: {e}"),
        }
    } else {
        eprintln!("No sessions found.");
    }

    Ok(())
}

/// Print log entries to stdout.
fn print_log_entries(entries: &[SessionLogEntry]) {
    for entry in entries {
        let direction = match entry.direction {
            session_log::EventDirection::Inbound => "->",
            session_log::EventDirection::Outbound => "<-",
        };
        println!(
            "[{}] {} {} {}",
            entry.timestamp.format("%Y-%m-%d %H:%M:%S%.3f"),
            direction,
            entry.event_type,
            serde_json::to_string(&entry.payload).unwrap_or_default(),
        );
    }
}

/// Print session list to stdout.
fn print_session_list(summaries: &[SessionSummary]) {
    println!(
        "{:<40} {:<24} {:<24} ENTRIES",
        "SESSION_ID", "CREATED", "LAST_ACTIVE"
    );
    println!("{}", "-".repeat(96));
    for s in summaries {
        println!(
            "{:<40} {:<24} {:<24} {}",
            s.session_id,
            s.created_at.format("%Y-%m-%d %H:%M:%S"),
            s.last_active.format("%Y-%m-%d %H:%M:%S"),
            s.entry_count,
        );
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use quine_harness::session_log::{EventDirection, SessionLogEntry, SessionSummary};

    #[test]
    fn print_log_entries_does_not_panic() {
        let entries = vec![SessionLogEntry {
            timestamp: Utc::now(),
            session_id: "test".to_string(),
            event_type: "user_message".to_string(),
            direction: EventDirection::Inbound,
            payload: serde_json::json!({"content": "hello"}),
        }];
        // Just verify it doesn't panic.
        super::print_log_entries(&entries);
    }

    #[test]
    fn print_session_list_does_not_panic() {
        let summaries = vec![SessionSummary {
            session_id: "abc".to_string(),
            created_at: Utc::now(),
            last_active: Utc::now(),
            entry_count: 3,
        }];
        super::print_session_list(&summaries);
    }
}
