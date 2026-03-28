use std::path::Path;

use quine_llm::Message;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::IpcClient;
use crate::render::{Renderer, TerminalRenderer};
use crate::session::{create_session, create_session_with_initial_messages};
use crate::slash_command::parse_slash_command;
use quine_harness::protocol::{methods, notifications};

/// Shut down the daemon if we spawned it.
async fn shutdown_if_spawned(client: &mut IpcClient, daemon_spawned: bool) {
    if daemon_spawned {
        let _ = client.call(methods::SHUTDOWN, None).await;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PlanExitHandoff {
    final_plan: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatCommandAction {
    Quit,
    ShowError(String),
    SendMessage(String),
    EnterPlanModeAndSend(String),
}

async fn maybe_exit_plan_mode(
    client: &mut IpcClient,
    skills: &[String],
    auto_approve_permissions: bool,
    session_in_plan_mode: &mut bool,
    session: &mut crate::session::CreatedSession,
    completed_text: &str,
) -> anyhow::Result<Option<PlanExitHandoff>> {
    if !*session_in_plan_mode {
        return Ok(None);
    }

    let final_plan = completed_text.trim();
    if final_plan.is_empty() {
        return Ok(None);
    }

    let initial_messages = [Message::assistant(final_plan.to_string())];
    *session = create_session_with_initial_messages(
        client,
        skills,
        false,
        auto_approve_permissions,
        &initial_messages,
    )
    .await?;
    *session_in_plan_mode = false;

    Ok(Some(PlanExitHandoff {
        final_plan: final_plan.to_string(),
    }))
}

fn handle_chat_command(input: &str, plan_mode: bool) -> ChatCommandAction {
    let trimmed = input.trim();
    if let Some(command) = parse_slash_command(trimmed) {
        match command.name.as_str() {
            "quit" => ChatCommandAction::Quit,
            "plan" => {
                if command.arguments.is_empty() {
                    ChatCommandAction::ShowError("Usage: /plan <request>".to_string())
                } else if plan_mode {
                    ChatCommandAction::SendMessage(command.arguments)
                } else {
                    ChatCommandAction::EnterPlanModeAndSend(command.arguments)
                }
            }
            other => ChatCommandAction::ShowError(format!("Unknown slash command: /{other}")),
        }
    } else {
        ChatCommandAction::SendMessage(trimmed.to_string())
    }
}

/// Run the interactive chat REPL.
///
/// Connects to the harness daemon, creates a session, then loops:
/// read user input -> send message -> print streamed response.
pub async fn run_chat(
    socket_path: &Path,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;
    let mut renderer = TerminalRenderer::new();

    // Create a session.
    let mut session =
        create_session(&mut client, skills, plan_mode, auto_approve_permissions).await?;
    let mut session_in_plan_mode = plan_mode;

    eprintln!("Session created: {}", session.session_id);
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

                        let content = match handle_chat_command(trimmed, session_in_plan_mode) {
                            ChatCommandAction::Quit => break,
                            ChatCommandAction::ShowError(message) => {
                                renderer.render_error(&message).await?;
                                continue;
                            }
                            ChatCommandAction::SendMessage(content) => content,
                            ChatCommandAction::EnterPlanModeAndSend(content) => {
                                session = create_session(
                                    &mut client,
                                    skills,
                                    true,
                                    auto_approve_permissions,
                                )
                                .await?;
                                session_in_plan_mode = true;
                                eprintln!("Switched to plan mode: {}", session.session_id);
                                content
                            }
                        };

                        // Send message.
                        let params = serde_json::json!({
                            "session_id": session.session_id,
                            "content": content,
                        });
                        let result = client.call(methods::SEND_MESSAGE, Some(params)).await?;
                        if let Err(e) = result {
                            renderer.render_error(&e).await?;
                            continue;
                        }

                        // Read notifications until TurnComplete.
                        let mut completed_text = String::new();
                        loop {
                            tokio::select! {
                                notif = client.recv_notification() => {
                                    match notif {
                                        Some(notif) => {
                                            if notif.method == notifications::INTERACTION_NEEDED {
                                                handle_interaction(&notif, &mut client, &session.session_id).await?;
                                                continue;
                                            }
                                            let handled = handle_notification(&notif, &mut renderer).await?;
                                            if notif.method == notifications::TEXT_COMPLETE {
                                                if let Some(full_text) = notif
                                                    .params
                                                    .as_ref()
                                                    .and_then(|p| p.get("full_text"))
                                                    .and_then(|v| v.as_str())
                                                {
                                                    completed_text = full_text.to_string();
                                                }
                                            }
                                            if handled {
                                                if let Some(handoff) = maybe_exit_plan_mode(
                                                    &mut client,
                                                    skills,
                                                    auto_approve_permissions,
                                                    &mut session_in_plan_mode,
                                                    &mut session,
                                                    &completed_text,
                                                )
                                                .await?
                                                {
                                                    eprintln!(
                                                        "Plan complete; started normal session with final plan: {}",
                                                        session.session_id
                                                    );
                                                    renderer.render_text_complete(&handoff.final_plan).await?;
                                                }
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

    // Extract options if present.
    let options: Vec<String> = notif
        .params
        .as_ref()
        .and_then(|p| p.get("options"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("label")
                        .and_then(|l| l.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    let mut stderr = tokio::io::stderr();
    stderr
        .write_all(format!("\n[ask_user] {prompt}\n").as_bytes())
        .await?;

    // Show numbered options if present.
    if !options.is_empty() {
        for (i, opt) in options.iter().enumerate() {
            stderr
                .write_all(format!("  {}. {opt}\n", i + 1).as_bytes())
                .await?;
        }
        stderr
            .write_all(b"Enter number (or text for custom answer): ")
            .await?;
    } else {
        stderr.write_all(b"> ").await?;
    }
    stderr.flush().await?;

    let mut line = String::new();
    let mut stdin = BufReader::new(tokio::io::stdin());
    stdin.read_line(&mut line).await?;
    let raw = line.trim().to_string();

    // If options present, try to parse as number.
    let response = if !options.is_empty() {
        if let Ok(num) = raw.parse::<usize>() {
            if num >= 1 && num <= options.len() {
                options[num - 1].clone()
            } else {
                raw
            }
        } else {
            raw
        }
    } else {
        raw
    };

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
        notifications::REASONING_DELTA => {
            if let Some(params) = &notif.params {
                if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                    renderer.render_reasoning_delta(delta).await?;
                }
            }
            Ok(false)
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_command_plan_switches_to_plan_mode_when_needed() {
        assert_eq!(
            handle_chat_command("/plan do thing", false),
            ChatCommandAction::EnterPlanModeAndSend("do thing".into())
        );
    }

    #[test]
    fn chat_command_plan_reuses_existing_plan_mode_session() {
        assert_eq!(
            handle_chat_command("/plan do thing", true),
            ChatCommandAction::SendMessage("do thing".into())
        );
    }

    #[test]
    fn chat_command_plan_without_arguments_is_local_error() {
        assert_eq!(
            handle_chat_command("/plan", false),
            ChatCommandAction::ShowError("Usage: /plan <request>".into())
        );
    }

    #[test]
    fn chat_command_unknown_slash_command_is_local_error() {
        assert_eq!(
            handle_chat_command("/does-not-exist", false),
            ChatCommandAction::ShowError("Unknown slash command: /does-not-exist".into())
        );
    }

    #[test]
    fn chat_command_preserves_quit_behavior() {
        assert_eq!(handle_chat_command("/quit", false), ChatCommandAction::Quit);
    }
}
