mod app;
mod ui;

use std::path::Path;
use std::time::Duration;

use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::client::IpcClient;
use crate::run::fetch_available_skills;
use crate::session::{
    create_session, create_session_with_initial_messages, create_slash_skill_session,
};
use app::{AgentPhase, AppAction, PendingPlanExit};
use quine_harness::protocol::{methods, notifications};
use quine_llm::Message;

/// Run the TUI chat interface.
///
/// Sets up the terminal in raw/alternate-screen mode, runs the main event loop,
/// and restores the terminal on exit (including panics).
pub async fn run_tui_chat(
    socket_path: &Path,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;
    let available_skills = fetch_available_skills(&mut client).await?;

    // Create session.
    let session = create_session(&mut client, skills, plan_mode, auto_approve_permissions).await?;

    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Install panic hook to restore terminal.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = std::io::stdout().execute(LeaveAlternateScreen);
        original_hook(info);
    }));

    let mut app = app::App::new(session.session_id, plan_mode, session.max_context_window);
    let mut event_stream = EventStream::new();
    let mut spinner_interval = tokio::time::interval(Duration::from_millis(80));

    let result = run_event_loop(
        &mut terminal,
        &mut app,
        &mut client,
        skills,
        &available_skills,
        auto_approve_permissions,
        &mut event_stream,
        &mut spinner_interval,
    )
    .await;

    // Restore terminal.
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Shutdown daemon if we spawned it.
    if daemon_spawned {
        let _ = client.call(methods::SHUTDOWN, None).await;
    }

    result
}

/// The main event loop: select over terminal events, daemon notifications, and spinner ticks.
#[allow(clippy::too_many_arguments)]
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut app::App,
    client: &mut IpcClient,
    skills: &[String],
    available_skills: &[String],
    auto_approve_permissions: bool,
    event_stream: &mut EventStream,
    spinner_interval: &mut tokio::time::Interval,
) -> anyhow::Result<()> {
    loop {
        // Draw.
        terminal.draw(|frame| ui::draw(frame, app))?;

        if app.should_quit {
            break;
        }

        // Select over event sources.
        tokio::select! {
            maybe_event = event_stream.next() => {
                match maybe_event {
                    Some(Ok(event)) => {
                        if let Some(action) = handle_terminal_event(app, event) {
                            execute_action(app, client, skills, available_skills, auto_approve_permissions, action).await?;
                        }
                    }
                    Some(Err(_)) | None => {
                        app.should_quit = true;
                    }
                }
            }
            maybe_notif = client.recv_notification() => {
                match maybe_notif {
                    Some(notif) => {
                        let should_check_plan_exit =
                            app.plan_mode && notif.method == notifications::TURN_COMPLETE;
                        app.apply_notification(&notif);
                        if should_check_plan_exit {
                            request_plan_exit_confirmation_after_turn_complete(app);
                        }
                    }
                    None => {
                        app.messages.push(app::ConversationEntry::Error(
                            "Connection to daemon lost.".into(),
                        ));
                        app.phase = AgentPhase::Idle;
                    }
                }
            }
            _ = spinner_interval.tick() => {
                app.tick_spinner();
            }
        }
    }

    Ok(())
}

fn request_plan_exit_confirmation_after_turn_complete(app: &mut app::App) {
    let pending_plan_exit =
        app.current_turn_assistant_text
            .as_ref()
            .map(|text| PendingPlanExit::FinalPlan {
                final_plan: text.clone(),
            });
    if let Some(pending_exit) = pending_plan_exit {
        app.request_plan_exit_confirmation(pending_exit);
    }
}

/// Map a terminal event to an AppAction.
fn handle_terminal_event(app: &mut app::App, event: Event) -> Option<AppAction> {
    match event {
        Event::Key(KeyEvent {
            code, modifiers, ..
        }) => {
            // Ctrl-C or Ctrl-D: quit.
            if code == KeyCode::Char('c') && modifiers.contains(KeyModifiers::CONTROL) {
                app.should_quit = true;
                return Some(AppAction::Quit);
            }
            if code == KeyCode::Char('d') && modifiers.contains(KeyModifiers::CONTROL) {
                app.should_quit = true;
                return Some(AppAction::Quit);
            }

            // In option-select mode, route keys differently.
            if app.is_selecting_options() {
                return match code {
                    KeyCode::Enter => app.submit_input(),
                    KeyCode::Up => {
                        app.option_cursor_up();
                        None
                    }
                    KeyCode::Down => {
                        app.option_cursor_down();
                        None
                    }
                    KeyCode::Char(' ') => {
                        app.option_toggle();
                        None
                    }
                    KeyCode::Esc => {
                        app.option_select = None;
                        None
                    }
                    _ => None,
                };
            }

            // Ctrl+Up/Down: always scroll conversation.
            if code == KeyCode::Up && modifiers.contains(KeyModifiers::CONTROL) {
                app.scroll_up(3);
                return None;
            }
            if code == KeyCode::Down && modifiers.contains(KeyModifiers::CONTROL) {
                app.scroll_down(3);
                return None;
            }

            // Enter submits input.
            if code == KeyCode::Enter {
                return app.submit_input();
            }

            match code {
                KeyCode::Backspace => {
                    app.input.delete_char_before();
                    None
                }
                KeyCode::Left => {
                    app.input.cursor_left();
                    None
                }
                KeyCode::Right => {
                    app.input.cursor_right();
                    None
                }
                KeyCode::Up => {
                    if app.input.is_multiline() && app.input.row() > 0 {
                        app.input.cursor_up();
                    } else {
                        app.history_prev();
                    }
                    None
                }
                KeyCode::Down => {
                    if app.input.is_multiline() && app.input.row() < app.input.line_count() - 1 {
                        app.input.cursor_down();
                    } else {
                        app.history_next();
                    }
                    None
                }
                KeyCode::PageUp => {
                    let step = if app.last_view_height > 2 {
                        app.last_view_height.saturating_sub(2)
                    } else {
                        10
                    };
                    app.scroll_up(step);
                    None
                }
                KeyCode::PageDown => {
                    let step = if app.last_view_height > 2 {
                        app.last_view_height.saturating_sub(2)
                    } else {
                        10
                    };
                    app.scroll_down(step);
                    None
                }
                KeyCode::Home => {
                    app.scroll_up(u32::MAX);
                    None
                }
                KeyCode::End => {
                    app.scroll_down(u32::MAX);
                    None
                }
                KeyCode::Esc => {
                    if app.phase != AgentPhase::Idle {
                        // Agent is busy — cancel in-flight work.
                        Some(AppAction::Cancel)
                    } else {
                        // Agent is idle — clear input buffer.
                        app.input.clear();
                        None
                    }
                }
                KeyCode::Char(c) => {
                    app.input.insert_char(c);
                    None
                }
                _ => None,
            }
        }
        Event::Mouse(_) => None,
        Event::Resize(_, _) => None, // ratatui redraws on next frame.
        _ => None,
    }
}

/// Execute an AppAction by calling the daemon.
async fn execute_action(
    app: &mut app::App,
    client: &mut IpcClient,
    skills: &[String],
    available_skills: &[String],
    auto_approve_permissions: bool,
    action: AppAction,
) -> anyhow::Result<()> {
    match action {
        AppAction::SendMessage(msg) => {
            app.begin_turn();
            let params = serde_json::json!({
                "session_id": app.session_id,
                "content": msg,
            });
            if let Err(e) = client.call(methods::SEND_MESSAGE, Some(params)).await {
                app.messages
                    .push(app::ConversationEntry::Error(e.to_string()));
                app.phase = AgentPhase::Idle;
            }
        }
        AppAction::CompactSession => {
            let params = serde_json::json!({
                "session_id": app.session_id,
            });
            match client.call(methods::COMPACT_SESSION, Some(params)).await {
                Ok(Ok(_)) => {
                    app.messages.push(app::ConversationEntry::AssistantText(
                        "Context compacted.".into(),
                    ));
                }
                Ok(Err(error)) => {
                    app.messages.push(app::ConversationEntry::Error(error));
                }
                Err(error) => {
                    app.messages
                        .push(app::ConversationEntry::Error(error.to_string()));
                }
            }
            app.phase = AgentPhase::Idle;
            app.auto_scroll();
        }
        AppAction::SendSlashSkillMessage {
            skill_name,
            request,
        } => {
            if !available_skills
                .iter()
                .any(|candidate| candidate == &skill_name)
            {
                app.messages.push(app::ConversationEntry::Error(format!(
                    "Unknown slash command: /{skill_name}"
                )));
                app.phase = AgentPhase::Idle;
                app.auto_scroll();
            } else {
                match create_slash_skill_session(
                    client,
                    &skill_name,
                    &request,
                    auto_approve_permissions,
                )
                .await
                {
                    Ok(session) => {
                        app.reset_for_new_session(
                            session.session_id,
                            false,
                            session.max_context_window,
                        );
                        app.messages
                            .push(app::ConversationEntry::AssistantText(format!(
                                "Started skill session: /{skill_name}"
                            )));
                        app.messages
                            .push(app::ConversationEntry::User(if request.is_empty() {
                                format!("/{skill_name}")
                            } else {
                                format!("/{skill_name} {request}")
                            }));
                        app.phase = AgentPhase::Thinking;
                        app.auto_scroll();
                        app.begin_turn();
                        let params = serde_json::json!({
                            "session_id": app.session_id,
                            "content": request,
                        });
                        if let Err(e) = client.call(methods::SEND_MESSAGE, Some(params)).await {
                            app.messages
                                .push(app::ConversationEntry::Error(e.to_string()));
                            app.phase = AgentPhase::Idle;
                        }
                    }
                    Err(e) => {
                        app.messages
                            .push(app::ConversationEntry::Error(e.to_string()));
                        app.phase = AgentPhase::Idle;
                    }
                }
            }
        }
        AppAction::ScheduleLoop {
            request,
            delay,
            cadence,
        } => {
            let params = serde_json::json!({
                "parent_id": app.session_id,
                "task": request,
                "delay_secs": delay.as_secs(),
                "cadence_secs": cadence.map(|value| value.as_secs()),
            });
            if let Err(e) = client.call(methods::SCHEDULE_AGENT, Some(params)).await {
                app.messages
                    .push(app::ConversationEntry::Error(e.to_string()));
            }
            app.phase = AgentPhase::Idle;
        }
        AppAction::EnterPlanMode {
            request,
            was_plan_mode,
        } => match create_session(client, skills, true, auto_approve_permissions).await {
            Ok(session) => {
                app.reset_for_new_session(session.session_id, true, session.max_context_window);
                app.messages
                    .push(app::ConversationEntry::User(request.clone()));
                app.phase = AgentPhase::Thinking;
                app.auto_scroll();
                app.begin_turn();
                let params = serde_json::json!({
                    "session_id": app.session_id,
                    "content": request,
                });
                if let Err(e) = client.call(methods::SEND_MESSAGE, Some(params)).await {
                    app.messages
                        .push(app::ConversationEntry::Error(e.to_string()));
                    app.phase = AgentPhase::Idle;
                }
            }
            Err(e) => {
                app.plan_mode = was_plan_mode;
                app.messages
                    .push(app::ConversationEntry::Error(e.to_string()));
                app.phase = AgentPhase::Idle;
            }
        },
        AppAction::ExitPlanMode { final_plan } => {
            match create_session_with_initial_messages(
                client,
                skills,
                false,
                auto_approve_permissions,
                &[Message::assistant(final_plan.clone())],
            )
            .await
            {
                Ok(session) => {
                    app.reset_for_new_session(
                        session.session_id,
                        false,
                        session.max_context_window,
                    );
                    app.messages.push(app::ConversationEntry::AssistantText(
                        "Plan complete. Started a fresh normal session with the final plan carried over."
                            .into(),
                    ));
                    app.messages
                        .push(app::ConversationEntry::PlanBox(final_plan));
                    app.auto_scroll();
                }
                Err(e) => {
                    app.messages
                        .push(app::ConversationEntry::Error(e.to_string()));
                    app.phase = AgentPhase::Idle;
                }
            }
        }
        AppAction::SubmitInteraction(response) => {
            let params = serde_json::json!({
                "session_id": app.session_id,
                "response": response,
            });
            if let Err(e) = client
                .call(methods::SUBMIT_INTERACTION_RESPONSE, Some(params))
                .await
            {
                app.messages
                    .push(app::ConversationEntry::Error(e.to_string()));
            }
        }
        AppAction::Cancel => {
            let params = serde_json::json!({
                "session_id": app.session_id,
            });
            if let Err(e) = client.call(methods::CANCEL, Some(params)).await {
                app.messages
                    .push(app::ConversationEntry::Error(e.to_string()));
            }
            app.cancel_active_turn();
            app.messages
                .push(app::ConversationEntry::Error("(cancelled)".into()));
        }
        AppAction::Quit => {
            app.should_quit = true;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_harness::protocol::JsonRpcNotification;

    fn make_notif(method: &str, params: serde_json::Value) -> JsonRpcNotification {
        JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
        }
    }

    #[test]
    fn turn_complete_exit_confirmation_uses_text_flushed_from_streaming_buffer() {
        let mut app = app::App::new("test".into(), true, None);
        let stream = make_notif(
            notifications::STREAM_DELTA,
            serde_json::json!({ "delta": "Final plan from stream" }),
        );
        let turn_complete = make_notif(
            notifications::TURN_COMPLETE,
            serde_json::json!({ "duration_us": 42 }),
        );

        app.apply_notification(&stream);
        assert!(app.pending_plan_exit.is_none());

        app.apply_notification(&turn_complete);
        request_plan_exit_confirmation_after_turn_complete(&mut app);

        assert!(matches!(
            app.pending_plan_exit,
            Some(PendingPlanExit::FinalPlan { ref final_plan }) if final_plan == "Final plan from stream"
        ));
        assert!(matches!(
            app.messages.last(),
            Some(app::ConversationEntry::InteractionQuestion { prompt, options })
                if prompt.contains("start a normal session with this final plan")
                && options == &vec!["Yes".to_string(), "No".to_string()]
        ));
    }

    #[test]
    fn turn_complete_does_not_reuse_stale_assistant_text() {
        let mut app = app::App::new("test".into(), true, None);
        app.messages.push(app::ConversationEntry::AssistantText(
            "Old plan text".into(),
        ));
        let turn_complete = make_notif(
            notifications::TURN_COMPLETE,
            serde_json::json!({ "duration_us": 42 }),
        );

        app.begin_turn();
        app.apply_notification(&turn_complete);
        request_plan_exit_confirmation_after_turn_complete(&mut app);

        assert!(app.pending_plan_exit.is_none());
        assert!(matches!(
            app.messages.last(),
            Some(app::ConversationEntry::TurnInfo { .. })
        ));
    }
}
