mod app;
mod ui;

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use crossterm::cursor::MoveTo;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::prelude::CrosstermBackend;
use ratatui::Terminal;
use serde::Deserialize;

use crate::client::IpcClient;
use crate::context_debug::fetch_session_context;
use crate::interaction::{maybe_auto_approve, prompt as interaction_prompt};
use crate::run::fetch_available_skills;
use crate::session::{
    create_session, create_slash_skill_session, exit_plan_mode, resolve_resume_target,
};
use app::{AgentPhase, AppAction, PendingPlanExit};
use quine_harness::protocol::{methods, notifications};

#[derive(Debug, Deserialize)]
struct TuiSessionSummary {
    session_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    status: String,
    #[serde(default)]
    plan_mode: bool,
    #[serde(default)]
    _depth: usize,
}

fn format_tui_ps_table(mut sessions: Vec<TuiSessionSummary>) -> String {
    if sessions.is_empty() {
        return "No sessions found.".to_string();
    }

    sessions.sort_by(|left, right| {
        left.parent_id
            .cmp(&right.parent_id)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    let session_width = sessions
        .iter()
        .map(|session| session.session_id.len())
        .max()
        .unwrap_or(7)
        .max("SESSION".len());
    let parent_width = sessions
        .iter()
        .map(|session| session.parent_id.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(6)
        .max("PARENT".len());
    let state_width = sessions
        .iter()
        .map(|session| session.status.len())
        .max()
        .unwrap_or(5)
        .max("STATUS".len());

    let mut lines = Vec::with_capacity(sessions.len() + 1);
    lines.push(format!(
        "{:<session_width$}  {:<parent_width$}  {:<state_width$}  MODE",
        "SESSION",
        "PARENT",
        "STATUS",
        session_width = session_width,
        parent_width = parent_width,
        state_width = state_width,
    ));

    for session in sessions {
        lines.push(format!(
            "{:<session_width$}  {:<parent_width$}  {:<state_width$}  {}",
            session.session_id,
            session.parent_id.as_deref().unwrap_or("-"),
            session.status,
            if session.plan_mode { "plan" } else { "chat" },
            session_width = session_width,
            parent_width = parent_width,
            state_width = state_width,
        ));
    }

    lines.join("\n")
}

fn format_tui_ps_tree(sessions: Vec<TuiSessionSummary>) -> String {
    if sessions.is_empty() {
        return "No sessions found.".to_string();
    }

    let mut sessions_by_id: BTreeMap<String, TuiSessionSummary> = BTreeMap::new();
    let mut children_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut roots = Vec::new();

    for session in sessions {
        let session_id = session.session_id.clone();
        if let Some(parent_id) = session.parent_id.clone() {
            children_by_parent
                .entry(parent_id)
                .or_default()
                .push(session_id.clone());
        } else {
            roots.push(session_id.clone());
        }
        sessions_by_id.insert(session_id, session);
    }

    roots.sort();
    for children in children_by_parent.values_mut() {
        children.sort();
    }

    fn walk(
        output: &mut Vec<String>,
        sessions_by_id: &BTreeMap<String, TuiSessionSummary>,
        children_by_parent: &BTreeMap<String, Vec<String>>,
        session_id: &str,
        prefix: &str,
        is_last: bool,
    ) {
        let Some(session) = sessions_by_id.get(session_id) else {
            return;
        };

        let branch = if prefix.is_empty() {
            ""
        } else if is_last {
            "└─ "
        } else {
            "├─ "
        };

        output.push(format!(
            "{}{}{} [{}]{}",
            prefix,
            branch,
            session.session_id,
            session.status,
            if session.plan_mode { " [plan]" } else { "" },
        ));

        let next_prefix = if prefix.is_empty() {
            String::new()
        } else if is_last {
            format!("{prefix}   ")
        } else {
            format!("{prefix}│  ")
        };

        if let Some(children) = children_by_parent.get(session_id) {
            for (index, child_id) in children.iter().enumerate() {
                walk(
                    output,
                    sessions_by_id,
                    children_by_parent,
                    child_id,
                    &next_prefix,
                    index + 1 == children.len(),
                );
            }
        }
    }

    let mut output = Vec::new();
    for (index, root_id) in roots.iter().enumerate() {
        walk(
            &mut output,
            &sessions_by_id,
            &children_by_parent,
            root_id,
            "",
            index + 1 == roots.len(),
        );
    }
    output.join("\n")
}

async fn fetch_tui_ps_output(client: &mut IpcClient, tree: bool) -> anyhow::Result<String> {
    let response = client.call(methods::LIST_SESSIONS, Some(serde_json::json!({}))).await?;
    let response = response.map_err(anyhow::Error::msg)?;
    let sessions: Vec<TuiSessionSummary> = serde_json::from_value(response)?;
    Ok(if tree {
        format_tui_ps_tree(sessions)
    } else {
        format_tui_ps_table(sessions)
    })
}

fn print_resume_command(socket_path: &Path, session_id: &str) {
    eprintln!(
        "Resume from this checkpoint with: `quine run --session {} --socket {} \"<message>\"`",
        session_id,
        socket_path.display()
    );
}

/// Run the TUI chat interface.
///
/// Sets up the terminal in raw/alternate-screen mode, runs the main event loop,
/// and restores the terminal on exit (including panics).
pub async fn run_tui_chat(
    socket_path: &Path,
    skills: &[String],
    plan_mode: bool,
    auto_approve: bool,
    resume_checkpoint: Option<&str>,
) -> anyhow::Result<()> {
    let (mut client, daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;
    let available_skills = fetch_available_skills(&mut client).await?;

    let resumed = resolve_resume_target(&mut client, resume_checkpoint).await?;

    // Create or resume session.
    let session_plan_mode = resumed.as_ref().map(|target| target.plan_mode);
    let session = match resumed {
        Some(target) => crate::session::CreatedSession {
            session_id: target.session_id,
            max_context_window: None,
        },
        None => create_session(&mut client, skills, plan_mode).await?,
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
        let mut stdout = std::io::stdout();
        let _ = disable_raw_mode();
        let _ = stdout.execute(LeaveAlternateScreen);
        let _ = stdout.execute(Clear(ClearType::All));
        let _ = stdout.execute(MoveTo(0, 0));
        original_hook(info);
    }));

    let mut app = app::App::new(
        session.session_id,
        session_plan_mode.unwrap_or(plan_mode),
        session.max_context_window,
    );
    if let Ok(snapshot) = fetch_session_context(&mut client, &app.session_id).await {
        app.loaded_skill_commands = snapshot
            .loaded_skills
            .into_iter()
            .map(|skill| skill.name)
            .collect();
    }
    let mut event_stream = EventStream::new();
    let mut spinner_interval = tokio::time::interval(Duration::from_millis(80));

    let result = run_event_loop(
        &mut terminal,
        &mut app,
        &mut client,
        skills,
        &available_skills,
        &mut event_stream,
        &mut spinner_interval,
        socket_path,
        auto_approve,
    )
    .await;

    // Restore terminal.
    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.backend_mut().execute(Clear(ClearType::All))?;
    terminal.backend_mut().execute(MoveTo(0, 0))?;
    terminal.show_cursor()?;

    // Shutdown daemon if we spawned it.
    if daemon_spawned {
        let _ = client.call(methods::SHUTDOWN, None).await;
    }

    if let Some(session_id) = result.as_ref().ok().map(|_| app.session_id.clone()) {
        print_resume_command(socket_path, &session_id);
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
    event_stream: &mut EventStream,
    spinner_interval: &mut tokio::time::Interval,
    socket_path: &Path,
    auto_approve: bool,
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
                            execute_action(app, client, skills, available_skills, socket_path, action).await?;
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
                        if notif.method == notifications::INTERACTION_NEEDED
                            && maybe_auto_approve(client, &app.session_id, &notif, auto_approve).await?
                        {
                            app.push_message(app::ConversationEntry::AssistantText(format!(
                                "Auto-approved permission request: {}",
                                interaction_prompt(&notif)
                            )));
                            app.auto_scroll();
                            continue;
                        }
                        let should_check_plan_exit =
                            app.plan_mode && notif.method == notifications::TURN_COMPLETE;
                        app.apply_notification(&notif);
                        if should_check_plan_exit {
                            request_plan_exit_confirmation_after_turn_complete(app);
                        }
                    }
                    None => {
                        app.push_message(app::ConversationEntry::Error(
                            "Connection to daemon lost.".into(),
                        ));
                        app.set_phase(AgentPhase::Idle);
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
            if app.context_explorer_active() {
                return match code {
                    KeyCode::Esc => {
                        app.close_context_explorer();
                        None
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        app.context_explorer_prev_tab();
                        None
                    }
                    KeyCode::Right | KeyCode::Char('l') => {
                        app.context_explorer_next_tab();
                        None
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.context_explorer_move_up();
                        None
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.context_explorer_move_down();
                        None
                    }
                    KeyCode::PageUp => {
                        let step = if app.last_view_height > 2 {
                            app.last_view_height.saturating_sub(2) as u16
                        } else {
                            10
                        };
                        app.context_explorer_scroll_up(step);
                        None
                    }
                    KeyCode::PageDown => {
                        let step = if app.last_view_height > 2 {
                            app.last_view_height.saturating_sub(2) as u16
                        } else {
                            10
                        };
                        app.context_explorer_scroll_down(step);
                        None
                    }
                    KeyCode::Home => {
                        app.context_explorer_move_to_first();
                        None
                    }
                    KeyCode::End => {
                        app.context_explorer_move_to_last();
                        None
                    }
                    _ => None,
                };
            }

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
            if app.is_selecting_options() && !app.slash_select_active {
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

            // Enter submits input, unless a slash suggestion is active.
            if code == KeyCode::Enter {
                if app.accept_slash_command_option() {
                    return None;
                }
                return app.submit_input();
            }

            match code {
                KeyCode::Backspace => {
                    app.input.delete_char_before();
                    app.refresh_slash_command_options();
                    None
                }
                KeyCode::Left => {
                    app.input.cursor_left();
                    app.refresh_slash_command_options();
                    None
                }
                KeyCode::Right => {
                    app.input.cursor_right();
                    app.refresh_slash_command_options();
                    None
                }
                KeyCode::Up => {
                    if app.is_selecting_options() {
                        app.option_cursor_up();
                        if app.slash_select_active {
                            app.preview_slash_command_option();
                        }
                    } else if app.input.is_multiline() && app.input.row() > 0 {
                        app.input.cursor_up();
                    } else {
                        app.history_prev();
                    }
                    None
                }
                KeyCode::Down => {
                    if app.is_selecting_options() {
                        app.option_cursor_down();
                        if app.slash_select_active {
                            app.preview_slash_command_option();
                        }
                    } else if app.input.is_multiline() && app.input.row() < app.input.line_count() - 1 {
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
                        app.refresh_slash_command_options();
                        None
                    }
                }
                KeyCode::Tab => {
                    app.finalize_slash_command_selection();
                    None
                }
                KeyCode::Char(c) => {
                    app.input.insert_char(c);
                    app.refresh_slash_command_options();
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
    _socket_path: &Path,
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
                app.push_message(app::ConversationEntry::Error(e.to_string()));
                app.set_phase(AgentPhase::Idle);
            }
        }
        AppAction::ShowContext => {
            match fetch_session_context(client, &app.session_id).await {
                Ok(snapshot) => app.open_context_explorer(snapshot),
                Err(error) => {
                    app.push_message(app::ConversationEntry::Error(error.to_string()));
                    app.auto_scroll();
                }
            }
            app.set_phase(AgentPhase::Idle);
        }
        AppAction::CompactSession => {
            let params = serde_json::json!({
                "session_id": app.session_id,
            });
            match client.call(methods::COMPACT_SESSION, Some(params)).await {
                Ok(Ok(_)) => {
                    app.push_message(app::ConversationEntry::AssistantText(
                        "Context compacted.".into(),
                    ));
                }
                Ok(Err(error)) => {
                    app.push_message(app::ConversationEntry::Error(error));
                }
                Err(error) => {
                    app.push_message(app::ConversationEntry::Error(error.to_string()));
                }
            }
            app.set_phase(AgentPhase::Idle);
            app.auto_scroll();
        }
        AppAction::ListSessions { tree } => {
            match fetch_tui_ps_output(client, tree).await {
                Ok(output) => app.push_message(app::ConversationEntry::AssistantText(output)),
                Err(error) => {
                    app.push_message(app::ConversationEntry::Error(format!("/ps failed: {error}")));
                }
            }
            app.set_phase(AgentPhase::Idle);
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
                app.push_message(app::ConversationEntry::Error(format!(
                    "Unknown slash command: /{skill_name}"
                )));
                app.set_phase(AgentPhase::Idle);
                app.auto_scroll();
            } else {
                match create_slash_skill_session(client, &skill_name, &request).await {
                    Ok(session) => {
                        app.reset_for_new_session(
                            session.session_id,
                            false,
                            session.max_context_window,
                        );
                        if let Ok(snapshot) = fetch_session_context(client, &app.session_id).await {
                            app.loaded_skill_commands = snapshot
                                .loaded_skills
                                .into_iter()
                                .map(|skill| skill.name)
                                .collect();
                        }
                        app.push_message(app::ConversationEntry::AssistantText(format!(
                            "Started skill session: /{skill_name}"
                        )));
                        app.push_message(app::ConversationEntry::User(if request.is_empty() {
                            format!("/{skill_name}")
                        } else {
                            format!("/{skill_name} {request}")
                        }));
                        app.set_phase(AgentPhase::Thinking);
                        app.auto_scroll();
                        app.begin_turn();
                        let params = serde_json::json!({
                            "session_id": app.session_id,
                            "content": request,
                        });
                        if let Err(e) = client.call(methods::SEND_MESSAGE, Some(params)).await {
                            app.push_message(app::ConversationEntry::Error(e.to_string()));
                            app.set_phase(AgentPhase::Idle);
                        }
                    }
                    Err(e) => {
                        app.push_message(app::ConversationEntry::Error(e.to_string()));
                        app.set_phase(AgentPhase::Idle);
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
                app.push_message(app::ConversationEntry::Error(e.to_string()));
            }
            app.set_phase(AgentPhase::Idle);
        }
        AppAction::EnterPlanMode {
            request,
            was_plan_mode,
        } => match create_session(client, skills, true).await {
            Ok(session) => {
                app.reset_for_new_session(session.session_id, true, session.max_context_window);
                if let Ok(snapshot) = fetch_session_context(client, &app.session_id).await {
                    app.loaded_skill_commands = snapshot
                        .loaded_skills
                        .into_iter()
                        .map(|skill| skill.name)
                        .collect();
                }
                app.push_message(app::ConversationEntry::User(request.clone()));
                app.set_phase(AgentPhase::Thinking);
                app.auto_scroll();
                app.begin_turn();
                let params = serde_json::json!({
                    "session_id": app.session_id,
                    "content": request,
                });
                if let Err(e) = client.call(methods::SEND_MESSAGE, Some(params)).await {
                    app.push_message(app::ConversationEntry::Error(e.to_string()));
                    app.set_phase(AgentPhase::Idle);
                }
            }
            Err(e) => {
                app.plan_mode = was_plan_mode;
                app.push_message(app::ConversationEntry::Error(e.to_string()));
                app.set_phase(AgentPhase::Idle);
            }
        },
        AppAction::ExitPlanMode { final_plan } => {
            match exit_plan_mode(client, &app.session_id).await {
                Ok(()) => {
                    app.exit_plan_mode();
                    app.push_message(app::ConversationEntry::AssistantText(
                        "Plan complete. Continued in the same session with plan mode disabled."
                            .into(),
                    ));
                    app.push_message(app::ConversationEntry::PlanBox(final_plan));
                    app.auto_scroll();
                }
                Err(e) => {
                    app.push_message(app::ConversationEntry::Error(e.to_string()));
                    app.set_phase(AgentPhase::Idle);
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
                app.push_message(app::ConversationEntry::Error(e.to_string()));
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
