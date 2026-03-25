use std::path::Path;

use crate::client::IpcClient;
use quine_harness::protocol::methods;

/// Handle the `ps` command: list active agent sessions.
pub async fn handle_ps(
    socket_path: &Path,
    _all: bool,
    _tree: bool,
    json: bool,
) -> anyhow::Result<()> {
    let mut client = IpcClient::connect(socket_path).await?;
    let result = client.call(methods::LIST_SESSIONS, None).await?;

    match result {
        Ok(value) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let sessions: Vec<serde_json::Value> = serde_json::from_value(value)?;
                print_sessions_table(&sessions);
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
    let mut client = IpcClient::connect(socket_path).await?;

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
    let mut client = IpcClient::connect(socket_path).await?;

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

    let mut client = IpcClient::connect(socket_path).await?;

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
    let mut client = IpcClient::connect(socket_path).await?;

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
fn print_sessions_table(sessions: &[serde_json::Value]) {
    if sessions.is_empty() {
        println!("No active sessions.");
        return;
    }

    println!(
        "{:<38} {:<12} {:<24} EVENTS",
        "SESSION ID", "STATUS", "CREATED"
    );
    println!("{}", "-".repeat(80));

    for session in sessions {
        let id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let status = session
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let created = session
            .get("first_event")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let event_count = session
            .get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        println!("{id:<38} {status:<12} {created:<24} {event_count}");
    }
}

/// Format sessions as a table and return as a string.
#[cfg(test)]
fn format_ps_table(sessions: &[serde_json::Value]) -> String {
    if sessions.is_empty() {
        return "No active sessions.".to_string();
    }

    let mut output = format!(
        "{:<38} {:<12} {:<24} EVENTS\n",
        "SESSION ID", "STATUS", "CREATED"
    );
    output.push_str(&"-".repeat(80));
    output.push('\n');

    for session in sessions {
        let id = session
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let status = session
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let created = session
            .get("first_event")
            .and_then(|v| v.as_str())
            .unwrap_or("-");
        let event_count = session
            .get("event_count")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        output.push_str(&format!(
            "{id:<38} {status:<12} {created:<24} {event_count}\n"
        ));
    }

    output
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
        assert_eq!(output, "No active sessions.");
    }

    #[test]
    fn format_ps_table_with_sessions() {
        let sessions = vec![serde_json::json!({
            "session_id": "abc-123",
            "status": "active",
            "first_event": "2026-01-01T00:00:00Z",
            "event_count": 42
        })];
        let output = format_ps_table(&sessions);
        assert!(output.contains("SESSION ID"));
        assert!(output.contains("abc-123"));
        assert!(output.contains("active"));
        assert!(output.contains("42"));
    }

    #[test]
    fn format_ps_table_missing_fields() {
        let sessions = vec![serde_json::json!({})];
        let output = format_ps_table(&sessions);
        assert!(output.contains("unknown"));
    }
}
