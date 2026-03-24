use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::client::IpcClient;
use quine_harness::protocol::{methods, notifications};

/// Structured JSON output for one-shot mode.
#[derive(Debug, Serialize, Deserialize)]
pub struct OneshotOutput {
    /// The session ID used for this run.
    pub session_id: String,
    /// The full text response from the agent.
    pub response: String,
    /// Tool calls made during the turn.
    pub tool_calls: Vec<ToolCallRecord>,
}

/// A record of a single tool call during a one-shot run.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// The tool name.
    pub tool_name: String,
    /// The tool use ID.
    pub tool_use_id: String,
}

/// Run a one-shot message: connect to daemon, send message, collect response, exit.
///
/// If `session_id` is `None`, creates a new session. If `json_output` is true,
/// prints structured JSON to stdout. Otherwise prints the text response to stdout
/// and the session ID to stderr.
pub async fn run_oneshot(
    socket_path: &Path,
    message: &str,
    session_id: Option<&str>,
    json_output: bool,
) -> anyhow::Result<()> {
    let mut client = IpcClient::connect(socket_path).await?;

    // Create or reuse session.
    let session_id = match session_id {
        Some(sid) => sid.to_string(),
        None => {
            let result = client.call(methods::CREATE_SESSION, None).await?;
            match result {
                Ok(value) => {
                    let sid = value
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?
                        .to_string();
                    eprintln!("session: {sid}");
                    sid
                }
                Err(e) => anyhow::bail!("failed to create session: {e}"),
            }
        }
    };

    // Send message.
    let params = serde_json::json!({
        "session_id": session_id,
        "content": message,
    });
    let result = client.call(methods::SEND_MESSAGE, Some(params)).await?;
    if let Err(e) = result {
        anyhow::bail!("failed to send message: {e}");
    }

    // Collect response notifications until TurnComplete.
    let mut full_response = String::new();
    let mut tool_calls = Vec::new();

    loop {
        match client.recv_notification().await {
            Some(notif) => match notif.method.as_str() {
                notifications::STREAM_DELTA => {
                    if let Some(params) = &notif.params {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            full_response.push_str(delta);
                        }
                    }
                }
                notifications::TEXT_COMPLETE => {
                    // Use full_text if available, otherwise keep accumulated deltas.
                    if let Some(params) = &notif.params {
                        if let Some(full_text) = params.get("full_text").and_then(|v| v.as_str()) {
                            full_response = full_text.to_string();
                        }
                    }
                }
                notifications::TOOL_REQUEST => {
                    if let Some(params) = &notif.params {
                        let tool_name = params
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let tool_use_id = params
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        tool_calls.push(ToolCallRecord {
                            tool_name,
                            tool_use_id,
                        });
                    }
                }
                notifications::SESSION_ERROR => {
                    if let Some(params) = &notif.params {
                        if let Some(error) = params.get("error").and_then(|v| v.as_str()) {
                            anyhow::bail!("session error: {error}");
                        }
                    }
                }
                notifications::TURN_COMPLETE => {
                    break;
                }
                _ => {}
            },
            None => {
                anyhow::bail!("connection to daemon lost");
            }
        }
    }

    // Output results.
    if json_output {
        let output = OneshotOutput {
            session_id,
            response: full_response,
            tool_calls,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{full_response}");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oneshot_output_serialization() {
        let output = OneshotOutput {
            session_id: "test-123".to_string(),
            response: "Hello!".to_string(),
            tool_calls: vec![ToolCallRecord {
                tool_name: "bash".to_string(),
                tool_use_id: "call_1".to_string(),
            }],
        };

        let json = serde_json::to_string(&output).unwrap();
        let parsed: OneshotOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "test-123");
        assert_eq!(parsed.response, "Hello!");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].tool_name, "bash");
    }

    #[test]
    fn tool_call_record_serialization() {
        let record = ToolCallRecord {
            tool_name: "read".to_string(),
            tool_use_id: "id_1".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("read"));
        assert!(json.contains("id_1"));
    }
}
