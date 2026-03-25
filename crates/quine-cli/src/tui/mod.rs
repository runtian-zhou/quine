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
use app::{AgentPhase, AppAction};
use quine_harness::protocol::methods;

/// Run the TUI chat interface.
///
/// Sets up the terminal in raw/alternate-screen mode, runs the main event loop,
/// and restores the terminal on exit (including panics).
pub async fn run_tui_chat(socket_path: &Path) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;

    // Create session.
    let result = client.call(methods::CREATE_SESSION, None).await?;
    let session_id = match result {
        Ok(value) => value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?
            .to_string(),
        Err(e) => anyhow::bail!("failed to create session: {e}"),
    };

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

    let mut app = app::App::new(session_id.clone());
    let mut event_stream = EventStream::new();
    let mut spinner_interval = tokio::time::interval(Duration::from_millis(80));

    let result = run_event_loop(
        &mut terminal,
        &mut app,
        &mut client,
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
                            execute_action(app, client, action).await?;
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

            // Ctrl+S submits input.
            if code == KeyCode::Char('s') && modifiers.contains(KeyModifiers::CONTROL) {
                return app.submit_input();
            }

            // Shift+Enter or Alt+Enter inserts a newline.
            if code == KeyCode::Enter
                && (modifiers.contains(KeyModifiers::SHIFT)
                    || modifiers.contains(KeyModifiers::ALT))
            {
                app.input.insert_newline();
                return None;
            }

            match code {
                KeyCode::Enter => app.submit_input(),
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
                    app.scroll_up(10);
                    None
                }
                KeyCode::PageDown => {
                    app.scroll_down(10);
                    None
                }
                KeyCode::Home => {
                    app.scroll_up(u16::MAX);
                    None
                }
                KeyCode::End => {
                    app.scroll_down(u16::MAX);
                    None
                }
                KeyCode::Esc => {
                    app.input.clear();
                    None
                }
                KeyCode::Char(c) => {
                    app.input.insert_char(c);
                    None
                }
                _ => None,
            }
        }
        Event::Resize(_, _) => None, // ratatui redraws on next frame.
        _ => None,
    }
}

/// Execute an AppAction by calling the daemon.
async fn execute_action(
    app: &mut app::App,
    client: &mut IpcClient,
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
        AppAction::Quit => {
            app.should_quit = true;
        }
    }
    Ok(())
}
