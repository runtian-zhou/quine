use std::path::Path;

use tokio::io::{AsyncBufReadExt, BufReader};

use crate::client::IpcClient;
use crate::render::{Renderer, TerminalRenderer};
use quine_harness::protocol::{methods, notifications};

/// Run the interactive chat REPL.
///
/// Connects to the harness daemon, creates a session, then loops:
/// read user input -> send message -> print streamed response.
pub async fn run_chat(socket_path: &Path) -> anyhow::Result<()> {
    let mut client = IpcClient::connect(socket_path).await?;
    let mut renderer = TerminalRenderer::new();

    // Create a session.
    let result = client.call(methods::CREATE_SESSION, None).await?;
    let session_id = match result {
        Ok(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?
            .to_string(),
        Err(e) => anyhow::bail!("failed to create session: {e}"),
    };

    eprintln!("Session created: {session_id}");
    eprintln!("Type /quit or Ctrl-D to exit.\n");

    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();

    loop {
        // Print prompt.
        eprint!("> ");

        match lines.next_line().await? {
            Some(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if trimmed == "/quit" {
                    break;
                }

                // Send message.
                let params = serde_json::json!({
                    "session_id": session_id,
                    "content": trimmed,
                });
                let result = client.call(methods::SEND_MESSAGE, Some(params)).await?;
                if let Err(e) = result {
                    renderer.render_error(&e).await?;
                    continue;
                }

                // Read notifications until TurnComplete.
                loop {
                    match client.recv_notification().await {
                        Some(notif) => {
                            let handled = handle_notification(&notif, &mut renderer).await?;
                            if handled {
                                break;
                            }
                        }
                        None => {
                            renderer.render_error("connection to daemon lost").await?;
                            return Ok(());
                        }
                    }
                }
            }
            None => {
                // EOF (Ctrl-D).
                eprintln!("\nGoodbye!");
                break;
            }
        }
    }

    // Shutdown.
    let _ = client.call(methods::SHUTDOWN, None).await;
    Ok(())
}

/// Handle a single notification. Returns `true` if the turn is complete.
async fn handle_notification(
    notif: &quine_harness::protocol::JsonRpcNotification,
    renderer: &mut impl Renderer,
) -> anyhow::Result<bool> {
    match notif.method.as_str() {
        notifications::STREAM_DELTA => {
            if let Some(params) = &notif.params {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    renderer.render_delta(delta).await?;
                }
            }
            Ok(false)
        }
        notifications::TEXT_COMPLETE => {
            if let Some(params) = &notif.params {
                if let Some(full_text) = params.get("full_text").and_then(|v| v.as_str()) {
                    renderer.render_text_complete(full_text).await?;
                }
            }
            Ok(false)
        }
        notifications::TOOL_REQUEST => {
            if let Some(params) = &notif.params {
                let tool_name = params
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let tool_use_id = params
                    .get("tool_use_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                renderer.render_tool_request(tool_name, tool_use_id).await?;
            }
            Ok(false)
        }
        notifications::SESSION_ERROR => {
            if let Some(params) = &notif.params {
                if let Some(error) = params.get("error").and_then(|v| v.as_str()) {
                    renderer.render_error(error).await?;
                }
            }
            Ok(false)
        }
        notifications::TURN_COMPLETE => {
            renderer.render_turn_complete().await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}
