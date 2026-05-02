use std::path::Path;

use chrono::{DateTime, Local};

use crate::client::IpcClient;
use crate::ps::{format_session_summary, prepend_summary};
use quine_harness::protocol::methods;

/// Handle the `ps` command: list active agent sessions.
pub async fn handle_ps(
    socket_path: &Path,
    _all: bool,
    tree: bool,
    json: bool,
) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect_or_launch(socket_path).await?;
    let result = client.call(methods::LIST_SESSIONS, None).await?;

    match result {
        Ok(value) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let sessions: Vec<serde_json::Value> = serde_json::from_value(value)?;
                print_sessions_table(&sessions, tree);
            }
        }
        Err(e) => {
            eprintln!("Error listing sessions: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Handle the `spawn` command: create a new child agent session.
pub async fn handle_spawn(
    socket_path: &Path,
    task: &str,
    parent: Option<&str>,
    system_prompt: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect_or_launch(socket_path).await?;

    let mut params = serde_json::json!({ "task": task });
    if let Some(parent_id) = parent {
        params["parent_id"] = serde_json::Value::String(parent_id.to_string());
    }
    if let Some(prompt) = system_prompt {
        params["system_prompt"] = serde_json::Value::String(prompt.to_string());
    }

    let result = client.call(methods::SPAWN_SESSION, Some(params)).await?;

    match result {
        Ok(session_id) => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "session_id": session_id,
                    }))?
                );
            } else {
                let id_str = session_id
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| session_id.to_string());
                println!("Spawned session: {id_str}");
            }
        }
        Err(e) => {
            eprintln!("Error spawning session: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Handle the `signal` command: send a signal to a session.
pub async fn handle_signal(
    socket_path: &Path,
    session_id: &str,
    signal: &str,
) -> anyhow::Result<()> {
    let parsed = parse_signal_string(signal)?;
    let (mut client, _) = IpcClient::connect_or_launch(socket_path).await?;

    let params = serde_json::json!({
        "session_id": session_id,
        "signal": parsed,
    });

    let result = client.call(methods::SIGNAL_SESSION, Some(params)).await?;

    match result {
        Ok(_) => {
            eprintln!("Signal '{signal}' sent to session {session_id}");
        }
        Err(e) => {
            eprintln!("Error signaling session: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Handle the `send` command: send an IPC message to a target session.
pub async fn handle_send(
    socket_path: &Path,
    target: &str,
    message: Option<&str>,
) -> anyhow::Result<()> {
    let content = match message {
        Some(msg) => msg.to_string(),
        None => {
            // Read from stdin.
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf)?;
            buf
        }
    };

    let (mut client, _) = IpcClient::connect_or_launch(socket_path).await?;

    let params = serde_json::json!({
        "target": target,
        "content": content,
    });

    let result = client.call(methods::SEND_IPC_MESSAGE, Some(params)).await?;

    match result {
        Ok(_) => {
            eprintln!("Message sent to {target}");
        }
        Err(e) => {
            eprintln!("Error sending message: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Handle the `recv` command: receive an IPC message from a source session.
pub async fn handle_recv(
    socket_path: &Path,
    source: &str,
    non_blocking: bool,
) -> anyhow::Result<()> {
    let (mut client, _) = IpcClient::connect_or_launch(socket_path).await?;

    let params = serde_json::json!({
        "source": source,
        "non_blocking": non_blocking,
    });

    let result = client.call(methods::RECV_IPC_MESSAGE, Some(params)).await?;

    match result {
        Ok(value) => {
            if value.is_null() {
                if !non_blocking {
                    eprintln!("No message received");
                }
            } else if let Some(msg) = value.as_str() {
                println!("{msg}");
            } else {
                println!("{}", serde_json::to_string_pretty(&value)?);
            }
        }
        Err(e) => {
            eprintln!("Error receiving message: {e}");
            std::process::exit(1);
        }
    }

    Ok(())
}

/// Parse a signal string into its canonical form.
fn parse_signal_string(signal: &str) -> anyhow::Result<String> {
    match signal.to_lowercase().as_str() {
        "term" | "sigterm" => Ok("term".to_string()),
        "kill" | "sigkill" => Ok("kill".to_string()),
        "stop" | "sigstop" => Ok("stop".to_string()),
        "continue" | "cont" | "sigcont" => Ok("continue".to_string()),
        _ => anyhow::bail!("unknown signal: '{signal}'. Valid signals: term, kill, stop, continue"),
    }
}

/// Format sessions as a table for terminal output.
fn print_sessions_table(sessions: &[serde_json::Value], tree: bool) {
    let summary = format_status_summary(sessions);

    if sessions.is_empty() {
        println!("{summary}");
        return;
    }

    if tree {
        let body = format_tree_lines(sessions).join("\n");
        println!("{}", prepend_summary(&summary, &body));
        return;
    }

    let summary_width = sessions
        .iter()
        .map(|session| session_summary_label(session).len())
        .max()
        .unwrap_or(7)
        .max("SUMMARY".len());
    let created_width = 16usize;

    let mut lines = Vec::with_capacity(sessions.len() + 2);
    lines.push(format!(
        "{:<38} {:<12} {:<created_width$} {:<6} {:<summary_width$}",
        "SESSION ID",
        "STATUS",
        "CREATED",
        "EVENTS",
        "SUMMARY",
        created_width = created_width,
        summary_width = summary_width,
    ));
    lines.push("-".repeat(79 + created_width + summary_width));

    for session in sessions {
        let id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let status = session_status_label(session);
        let created = compact_timestamp(session);
        let event_count = session
            .get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let session_summary = session_summary_label(session);

        lines.push(format!(
            "{id:<38} {status:<12} {created:<created_width$} {event_count:<6} {session_summary:<summary_width$}",
            created_width = created_width,
            summary_width = summary_width,
        ));
    }

    println!("{}", prepend_summary(&summary, &lines.join("\n")));
}

fn format_status_summary(sessions: &[serde_json::Value]) -> String {
    format_session_summary(sessions.iter().map(session_status_label))
}

fn session_status_label(session: &serde_json::Value) -> &str {
    session
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
}

fn session_summary_label(session: &serde_json::Value) -> &str {
    session
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .or_else(|| session.get("title").and_then(serde_json::Value::as_str))
        .unwrap_or("")
}

fn compact_timestamp(session: &serde_json::Value) -> String {
    session
        .get("first_event")
        .and_then(|v| v.as_str())
        .and_then(parse_timestamp)
        .unwrap_or_else(|| "-".to_string())
}

fn session_last_active_value(session: &serde_json::Value) -> &str {
    session
        .get("last_active")
        .and_then(|value| value.as_str())
        .unwrap_or("")
}

fn sort_session_ids_by_last_active(
    ids: &mut [String],
    records: &std::collections::BTreeMap<String, &serde_json::Value>,
) {
    ids.sort_by(|left, right| {
        let left_session = records.get(left);
        let right_session = records.get(right);

        right_session
            .map(|session| session_last_active_value(session))
            .cmp(&left_session.map(|session| session_last_active_value(session)))
            .then_with(|| left.cmp(right))
    });
}

fn parse_timestamp(raw: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(raw).ok().map(|timestamp| {
        timestamp
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    })
}

/// Format sessions as a table and return as a string.
#[cfg(test)]
fn format_ps_table(sessions: &[serde_json::Value]) -> String {
    let summary = format_status_summary(sessions);
    if sessions.is_empty() {
        return summary;
    }

    let created_width = 16usize;
    let mut output = format!(
        "{:<38} {:<12} {:<created_width$} {:<6} SUMMARY\n",
        "SESSION ID",
        "STATUS",
        "CREATED",
        "EVENTS",
        created_width = created_width,
    );
    output.push_str(&"-".repeat(79 + created_width + "SUMMARY".len()));
    output.push('\n');

    for session in sessions {
        let id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let status = session_status_label(session);
        let created = compact_timestamp(session);
        let event_count = session
            .get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let session_summary = session_summary_label(session);

        output.push_str(&format!(
            "{id:<38} {status:<12} {created:<created_width$} {event_count:<6} {session_summary}\n",
            created_width = created_width,
        ));
    }

    prepend_summary(&summary, &output)
}

#[cfg(test)]
fn format_ps_tree(sessions: &[serde_json::Value]) -> String {
    let summary = format_status_summary(sessions);
    if sessions.is_empty() {
        return summary;
    }

    prepend_summary(&summary, &format_tree_lines(sessions).join("\n"))
}

fn format_tree_lines(sessions: &[serde_json::Value]) -> Vec<String> {
    use std::collections::BTreeMap;

    if sessions.is_empty() {
        return vec!["0 sessions".to_string()];
    }

    let mut records = BTreeMap::new();
    let mut children: BTreeMap<Option<String>, Vec<String>> = BTreeMap::new();

    for session in sessions {
        let id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let parent_id = session
            .get("parent_id")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        children
            .entry(parent_id.clone())
            .or_default()
            .push(id.clone());
        records.insert(id, session);
    }

    for ids in children.values_mut() {
        sort_session_ids_by_last_active(ids, &records);
    }

    let mut roots = children.remove(&None).unwrap_or_default();
    if roots.is_empty() {
        roots = records.keys().cloned().collect();
        sort_session_ids_by_last_active(&mut roots, &records);
    } else {
        sort_session_ids_by_last_active(&mut roots, &records);
    }

    let mut lines = Vec::new();
    for root_id in roots {
        push_tree_lines(&mut lines, &records, &children, &root_id, 0);
    }
    lines
}

fn push_tree_lines(
    lines: &mut Vec<String>,
    records: &std::collections::BTreeMap<String, &serde_json::Value>,
    children: &std::collections::BTreeMap<Option<String>, Vec<String>>,
    session_id: &str,
    depth: usize,
) {
    if let Some(session) = records.get(session_id) {
        let status = session
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let event_count = session
            .get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let created = compact_timestamp(session);
        let session_summary = session_summary_label(session);
        let indent = "  ".repeat(depth);
        lines.push(format!(
            "{indent}{} [{}] {} ev {}{}",
            session_id,
            status,
            event_count,
            created,
            if session_summary.is_empty() {
                String::new()
            } else {
                format!(" — {session_summary}")
            }
        ));
        if let Some(child_ids) = children.get(&Some(session_id.to_string())) {
            for child_id in child_ids {
                push_tree_lines(lines, records, children, child_id, depth + 1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_signal_term() {
        assert_eq!(parse_signal_string("term").unwrap(), "term");
        assert_eq!(parse_signal_string("TERM").unwrap(), "term");
        assert_eq!(parse_signal_string("sigterm").unwrap(), "term");
        assert_eq!(parse_signal_string("SIGTERM").unwrap(), "term");
    }

    #[test]
    fn parse_signal_kill() {
        assert_eq!(parse_signal_string("kill").unwrap(), "kill");
        assert_eq!(parse_signal_string("KILL").unwrap(), "kill");
        assert_eq!(parse_signal_string("sigkill").unwrap(), "kill");
    }

    #[test]
    fn parse_signal_stop() {
        assert_eq!(parse_signal_string("stop").unwrap(), "stop");
        assert_eq!(parse_signal_string("STOP").unwrap(), "stop");
        assert_eq!(parse_signal_string("sigstop").unwrap(), "stop");
    }

    #[test]
    fn parse_signal_continue() {
        assert_eq!(parse_signal_string("continue").unwrap(), "continue");
        assert_eq!(parse_signal_string("CONTINUE").unwrap(), "continue");
        assert_eq!(parse_signal_string("cont").unwrap(), "continue");
        assert_eq!(parse_signal_string("sigcont").unwrap(), "continue");
    }

    #[test]
    fn parse_signal_invalid() {
        assert!(parse_signal_string("invalid").is_err());
        assert!(parse_signal_string("").is_err());
        assert!(parse_signal_string("pause").is_err());
    }

    #[test]
    fn format_ps_table_empty() {
        let sessions: Vec<serde_json::Value> = vec![];
        let output = format_ps_table(&sessions);
        assert_eq!(output, "0 sessions");
    }

    #[test]
    fn format_ps_table_with_sessions() {
        let sessions = vec![serde_json::json!({
            "session_id": "abc-123",
            "status": "active",
            "first_event": "2026-01-01T00:00:00Z",
            "event_count": 42,
            "summary": "Working on ps output"
        })];
        let output = format_ps_table(&sessions);
        assert!(output.starts_with("1 sessions · 1 active\n\nSESSION ID"));
        assert!(output.contains("abc-123"));
        assert!(output.contains("active"));
        assert!(output.contains("42"));
        assert!(output.contains("Working on ps output"));
    }

    #[test]
    fn format_ps_table_missing_fields() {
        let sessions = vec![serde_json::json!({})];
        let output = format_ps_table(&sessions);
        assert!(output.starts_with("1 sessions · 1 unknown"));
    }

    #[test]
    fn format_ps_tree_groups_children_under_parent() {
        let sessions = vec![
            serde_json::json!({
                "session_id": "parent",
                "status": "idle",
                "first_event": "2026-01-01T00:00:00Z",
                "event_count": 2,
                "parent_id": null
            }),
            serde_json::json!({
                "session_id": "child",
                "status": "waiting",
                "first_event": "2026-01-01T00:01:00Z",
                "event_count": 5,
                "parent_id": "parent"
            }),
        ];

        let output = format_ps_tree(&sessions);
        assert!(output.starts_with("2 sessions · 1 idle · 1 waiting"));
        assert!(output.contains("parent [idle]"));
        assert!(output.contains("  child [waiting]"));
    }

    #[test]
    fn format_ps_tree_shows_summary() {
        let sessions = vec![serde_json::json!({
            "session_id": "parent",
            "status": "idle",
            "first_event": "2026-01-01T00:00:00Z",
            "event_count": 2,
            "parent_id": null,
            "summary": "Top level summary"
        })];

        let output = format_ps_tree(&sessions);
        assert!(output.contains("— Top level summary"));
    }

    #[test]
    fn format_ps_tree_orders_siblings_by_latest_activity() {
        let sessions = vec![
            serde_json::json!({
                "session_id": "older",
                "status": "idle",
                "parent_id": serde_json::Value::Null,
                "event_count": 1,
                "first_event": "2026-01-01T00:00:00Z",
                "last_active": "2026-01-01T00:00:00Z",
                "summary": "Older"
            }),
            serde_json::json!({
                "session_id": "newer",
                "status": "idle",
                "parent_id": serde_json::Value::Null,
                "event_count": 1,
                "first_event": "2026-01-01T00:00:00Z",
                "last_active": "2026-01-02T00:00:00Z",
                "summary": "Newer"
            }),
        ];

        let output = format_ps_tree(&sessions);
        let newer_index = output.find("newer [idle]").expect("newer in tree");
        let older_index = output.find("older [idle]").expect("older in tree");
        assert!(
            newer_index < older_index,
            "expected newer session first: {output}"
        );
    }
}
