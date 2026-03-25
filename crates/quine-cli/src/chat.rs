use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::IpcClient;
use crate::render::{Renderer, TerminalRenderer};
use quine_harness::protocol::{methods, notifications};

/// Shut down the daemon if we spawned it.
async fn shutdown_if_spawned(client: &mut IpcClient, daemon_spawned: bool) {
    if daemon_spawned {
        let _ = client.call(methods::SHUTDOWN, None).await;
    }
}

/// Run the interactive chat REPL.
///
/// Connects to the harness daemon, creates a session, then loops:
/// read user input -> send message -> print streamed response.
pub async fn run_chat(socket_path: &Path) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;
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

        // Wait for either user input or Ctrl-C.
        tokio::select! {
            line_result = lines.next_line() => {
                match line_result? {
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
                            tokio::select! {
                                notif = client.recv_notification() => {
                                    match notif {
                                        Some(notif) => {
                                            if notif.method == notifications::INTERACTION_NEEDED {
                                                handle_interaction(&notif, &mut client, &session_id).await?;
                                                continue;
                                            }
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
                                _ = tokio::signal::ctrl_c() => {
                                    eprintln!("\nInterrupted.");
                                    shutdown_if_spawned(&mut client, daemon_spawned).await;
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
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nInterrupted.");
                shutdown_if_spawned(&mut client, daemon_spawned).await;
                return Ok(());
            }
        }
    }

    shutdown_if_spawned(&mut client, daemon_spawned).await;
    drop(client);
    Ok(())
}

/// Handle an `interaction_needed` notification by prompting the user and
/// sending the response back to the daemon.
async fn handle_interaction(
    notif: &quine_harness::protocol::JsonRpcNotification,
    client: &mut IpcClient,
    session_id: &str,
) -> anyhow::Result<()> {
    let prompt = notif
        .params
        .as_ref()
        .and_then(|p| p.get("prompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("(tool is asking for input)");

    let mut stderr = tokio::io::stderr();
    stderr
        .write_all(format!("\n[ask_user] {prompt}\n> ").as_bytes())
        .await?;
    stderr.flush().await?;

    let mut line = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    stdin.read_line(&mut line).await?;
    let response = line.trim().to_string();

    let params = serde_json::json!({
        "session_id": session_id,
        "response": response,
    });
    let result = client
        .call(methods::SUBMIT_INTERACTION_RESPONSE, Some(params))
        .await?;
    if let Err(e) = result {
        eprintln!("warning: failed to submit interaction response: {e}");
    }

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
