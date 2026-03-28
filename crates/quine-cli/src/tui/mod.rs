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
use crate::session::create_session;
use app::{AgentPhase, AppAction};
use quine_harness::protocol::methods;

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
async fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut app::App,
    client: &mut IpcClient,
    skills: &[String],
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
                            execute_action(app, client, skills, auto_approve_permissions, action).await?;
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
                        app.apply_notification(&notif);
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
    auto_approve_permissions: bool,
    action: AppAction,
) -> anyhow::Result<()> {
    match action {
        AppAction::SendMessage(msg) => {
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
        AppAction::EnterPlanMode {
            request,
            was_plan_mode,
        } => match create_session(client, skills, true, auto_approve_permissions).await {
            Ok(session) => {
                app.session_id = session.session_id;
                app.max_context_window = session.max_context_window;
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
