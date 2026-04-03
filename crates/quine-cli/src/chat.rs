use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::client::IpcClient;
use crate::context_debug::render_session_context;
use crate::render::{Renderer, TerminalRenderer};
use crate::session::{
    create_session, create_slash_skill_session, exit_plan_mode, resolve_resume_target,
};
use crate::slash_command::{parse_slash_command, SlashCommand};
use quine_harness::protocol::{methods, notifications};

/// Shut down the daemon if we spawned it.
async fn shutdown_if_spawned(client: &mut IpcClient, daemon_spawned: bool) {
    if daemon_spawned {
        let _ = client.call(methods::SHUTDOWN, None).await;
    }
}

fn is_confirmed_plan_exit(response: &str) -> Option<bool> {
    match response.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => Some(true),
        "n" | "no" => Some(false),
        _ => None,
    }
}

async fn confirm_plan_exit(
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    prompt: &str,
) -> anyhow::Result<bool> {
    loop {
        eprint!("{prompt} ");
        match lines.next_line().await? {
            Some(line) => {
                if let Some(confirmed) = is_confirmed_plan_exit(&line) {
                    return Ok(confirmed);
                }
                eprintln!("Please answer yes or no.");
            }
            None => return Ok(false),
        }
    }
}

fn print_resume_command(socket_path: &Path, session_id: &str) {
    eprintln!(
        "Resume from this checkpoint with: `quine run --session {} --socket {} \"<message>\"`",
        session_id,
        socket_path.display()
    );
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ChatCommandAction {
    Quit,
    ShowError(String),
    ShowContext,
    SendMessage(String),
    CompactSession,
    EnterPlanModeAndSend(String),
    StartSkillSession { skill_name: String, request: String },
}

async fn maybe_exit_plan_mode(
    client: &mut IpcClient,
    lines: &mut tokio::io::Lines<BufReader<tokio::io::Stdin>>,
    session_in_plan_mode: &mut bool,
    session_id: &str,
    completed_text: &str,
) -> anyhow::Result<bool> {
    if !*session_in_plan_mode {
        return Ok(false);
    }

    let final_plan = completed_text.trim();
    if final_plan.is_empty() {
        return Ok(false);
    }

    if !confirm_plan_exit(
        lines,
        "Leave plan mode and start a normal session with this final plan? (y/n)",
    )
    .await?
    {
        eprintln!("Stayed in plan mode.");
        return Ok(false);
    }

    exit_plan_mode(client, session_id).await?;
    *session_in_plan_mode = false;

    Ok(true)
}

fn handle_chat_command(input: &str, plan_mode: bool) -> ChatCommandAction {
    let trimmed = input.trim();
    if let Some(command) = parse_slash_command(trimmed) {
        match command {
            SlashCommand::BuiltIn { name, arguments } => match name.as_str() {
                "quit" => ChatCommandAction::Quit,
                "compact" => {
                    if arguments.is_empty() {
                        ChatCommandAction::CompactSession
                    } else {
                        ChatCommandAction::ShowError("Usage: /compact".to_string())
                    }
                }
                "context" => {
                    if arguments.is_empty() {
                        ChatCommandAction::ShowContext
                    } else {
                        ChatCommandAction::ShowError("Usage: /context".to_string())
                    }
                }
                "plan" => {
                    if arguments.is_empty() {
                        ChatCommandAction::ShowError("Usage: /plan <request>".to_string())
                    } else if plan_mode {
                        ChatCommandAction::SendMessage(arguments)
                    } else {
                        ChatCommandAction::EnterPlanModeAndSend(arguments)
                    }
                }
                "loop" => ChatCommandAction::ShowError(
                    "`/loop` is only supported in the TUI right now".to_string(),
                ),
                other => ChatCommandAction::ShowError(format!("Unknown slash command: /{other}")),
            },
            SlashCommand::Skill { name, arguments } => ChatCommandAction::StartSkillSession {
                skill_name: name,
                request: arguments,
            },
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
    resume_checkpoint: Option<&str>,
) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;
    let mut renderer = TerminalRenderer::new();

    let resumed = resolve_resume_target(&mut client, resume_checkpoint).await?;

    // Create or resume a session.
    let mut session = match resumed {
        Some(target) => crate::session::CreatedSession {
            session_id: target.session_id,
            max_context_window: None,
        },
        None => create_session(&mut client, skills, plan_mode, auto_approve_permissions).await?,
    };
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
                            ChatCommandAction::ShowContext => {
                                render_session_context(&mut renderer, &mut client, &session.session_id)
                                    .await?;
                                continue;
                            }
                            ChatCommandAction::CompactSession => {
                                renderer.render_info("Compacting context...").await?;
                                let params = serde_json::json!({
                                    "session_id": session.session_id,
                                });
                                let result = client.call(methods::COMPACT_SESSION, Some(params)).await?;
                                match result {
                                    Ok(_) => renderer.render_info("Context compacted.").await?,
                                    Err(error) => renderer.render_error(&error).await?,
                                }
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
                            ChatCommandAction::StartSkillSession { skill_name, request } => {
                                if session_in_plan_mode
                                    && !confirm_plan_exit(
                                        &mut lines,
                                        &format!(
                                            "Leave plan mode and start /{skill_name}? (y/n)"
                                        ),
                                    )
                                    .await?
                                {
                                    eprintln!("Stayed in plan mode.");
                                    continue;
                                }
                                session = create_slash_skill_session(
                                    &mut client,
                                    &skill_name,
                                    &request,
                                    auto_approve_permissions,
                                )
                                .await?;
                                session_in_plan_mode = false;
                                renderer
                                    .render_info(&format!("Started skill session: /{skill_name}"))
                                    .await?;
                                if request.is_empty() {
                                    renderer
                                        .render_info("Enter a prompt to continue.")
                                        .await?;
                                    continue;
                                }
                                request
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
                                                if maybe_exit_plan_mode(
                                                    &mut client,
                                                    &mut lines,
                                                    &mut session_in_plan_mode,
                                                    &session.session_id,
                                                    &completed_text,
                                                )
                                                .await?
                                                {
                                                    eprintln!("Plan complete; session left plan mode.");
                                                    renderer
                                                        .render_info(
                                                            "Session left plan mode; the final plan remains in the transcript.",
                                                        )
                                                        .await?;
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
                                    print_resume_command(socket_path, &session.session_id);
                                    shutdown_if_spawned(&mut client, daemon_spawned).await;
                                    return Ok(());
                                }
                            }
                        }
                    }
                    None => {
                        // EOF (Ctrl-D).
                        eprintln!("\nGoodbye!");
                        print_resume_command(socket_path, &session.session_id);
                        break;
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                eprintln!("\nInterrupted.");
                print_resume_command(socket_path, &session.session_id);
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
    fn chat_command_compact_uses_manual_compaction_path() {
        assert_eq!(
            handle_chat_command("/compact", false),
            ChatCommandAction::CompactSession
        );
    }

    #[test]
    fn chat_command_skill_starts_new_session() {
        assert_eq!(
            handle_chat_command("/review audit this", false),
            ChatCommandAction::StartSkillSession {
                skill_name: "review".into(),
                request: "audit this".into(),
            }
        );
    }

    #[test]
    fn chat_command_loop_is_tui_only_error() {
        assert_eq!(
            handle_chat_command("/loop every 5m check logs", false),
            ChatCommandAction::ShowError("`/loop` is only supported in the TUI right now".into())
        );
    }

    #[test]
    fn plan_exit_confirmation_accepts_yes_and_no() {
        assert_eq!(is_confirmed_plan_exit("yes"), Some(true));
        assert_eq!(is_confirmed_plan_exit("Y"), Some(true));
        assert_eq!(is_confirmed_plan_exit("no"), Some(false));
        assert_eq!(is_confirmed_plan_exit("n"), Some(false));
        assert_eq!(is_confirmed_plan_exit("maybe"), None);
    }
}
