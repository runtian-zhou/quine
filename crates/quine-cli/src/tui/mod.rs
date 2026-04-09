mod app;
mod ui;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use base64::Engine;
use crossterm::cursor::MoveTo;
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyModifiers,
    MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use futures::StreamExt;
use ratatui::prelude::CrosstermBackend;
use ratatui::widgets::Clear as TuiClear;
use ratatui::Terminal;
use serde::Deserialize;

use crate::client::IpcClient;
use crate::context_debug::fetch_session_context;
use crate::interaction::{maybe_auto_approve, prompt as interaction_prompt};
use crate::ps::{format_session_summary, prepend_summary};
use crate::run::fetch_available_skills;
use crate::session::{
    create_session, create_slash_skill_session, exit_plan_mode, resolve_resume_target,
    set_session_model_profile,
};
use app::{AgentPhase, AppAction, PendingPlanExit, SwitchSessionCandidate};
use quine_harness::protocol::{methods, notifications};

#[derive(Debug, Deserialize, Default)]
struct TuiModelProfilesDocument {
    #[serde(default)]
    profiles: BTreeMap<String, TuiModelProfileDefinition>,
}

#[derive(Debug, Deserialize)]
struct TuiModelProfileDefinition {
    #[allow(dead_code)]
    provider: String,
}

#[derive(Debug, Deserialize)]
struct TuiSessionSummary {
    session_id: String,
    #[serde(default)]
    parent_id: Option<String>,
    status: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    plan_mode: bool,
    #[serde(default)]
    _depth: usize,
}

fn format_tui_ps_table(mut sessions: Vec<TuiSessionSummary>) -> String {
    let summary = format_tui_status_summary(&sessions);
    if sessions.is_empty() {
        return summary;
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
    let mode_width = sessions
        .iter()
        .map(|session| {
            if session.plan_mode {
                "plan".len()
            } else {
                "chat".len()
            }
        })
        .max()
        .unwrap_or(4)
        .max("MODE".len());
    let summary_width = sessions
        .iter()
        .map(|session| tui_session_summary_text(session).len())
        .max()
        .unwrap_or(7)
        .max("SUMMARY".len());

    let mut lines = Vec::with_capacity(sessions.len() + 1);
    lines.push(format!(
        "{:<session_width$}  {:<parent_width$}  {:<state_width$}  {:<mode_width$}  {:<summary_width$}",
        "SESSION",
        "PARENT",
        "STATUS",
        "MODE",
        "SUMMARY",
        session_width = session_width,
        parent_width = parent_width,
        state_width = state_width,
        mode_width = mode_width,
        summary_width = summary_width,
    ));

    for session in sessions {
        lines.push(format!(
            "{:<session_width$}  {:<parent_width$}  {:<state_width$}  {:<mode_width$}  {:<summary_width$}",
            session.session_id,
            session.parent_id.as_deref().unwrap_or("-"),
            session.status,
            if session.plan_mode { "plan" } else { "chat" },
            tui_session_summary_text(&session),
            session_width = session_width,
            parent_width = parent_width,
            state_width = state_width,
            mode_width = mode_width,
            summary_width = summary_width,
        ));
    }

    prepend_summary(&summary, &lines.join("\n"))
}

fn model_profiles_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| home.join(".quine").join("model-profiles.yaml"))
}

fn load_model_profile_names() -> anyhow::Result<Vec<String>> {
    let Some(path) = model_profiles_path() else {
        return Ok(Vec::new());
    };
    if !path.exists() {
        return Ok(Vec::new());
    }

    let contents = std::fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    let document: TuiModelProfilesDocument = serde_yaml::from_str(&contents)
        .map_err(|error| anyhow::anyhow!("failed to parse {}: {error}", path.display()))?;
    Ok(document.profiles.into_keys().collect())
}

fn format_tui_ps_tree(sessions: Vec<TuiSessionSummary>) -> String {
    let summary = format_tui_status_summary(&sessions);
    if sessions.is_empty() {
        return summary;
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

        let summary = tui_session_summary_text(session);
        output.push(format!(
            "{}{}{} [{}]{}{}",
            prefix,
            branch,
            session.session_id,
            session.status,
            if summary.is_empty() {
                String::new()
            } else {
                format!(" — {summary}")
            },
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
    prepend_summary(&summary, &output.join("\n"))
}

fn format_tui_status_summary(sessions: &[TuiSessionSummary]) -> String {
    format_session_summary(sessions.iter().map(|session| session.status.as_str()))
}

fn tui_session_summary_text(session: &TuiSessionSummary) -> &str {
    session
        .summary
        .as_deref()
        .or(session.title.as_deref())
        .unwrap_or("")
}

async fn fetch_tui_ps_output(client: &mut IpcClient, tree: bool) -> anyhow::Result<String> {
    let response = client
        .call(methods::LIST_SESSIONS, Some(serde_json::json!({})))
        .await?;
    let response = response.map_err(anyhow::Error::msg)?;
    let sessions: Vec<TuiSessionSummary> = serde_json::from_value(response)?;
    Ok(if tree {
        format_tui_ps_tree(sessions)
    } else {
        format_tui_ps_table(sessions)
    })
}

async fn list_sessions(client: &mut IpcClient) -> anyhow::Result<Vec<TuiSessionSummary>> {
    let response = client
        .call(methods::LIST_SESSIONS, Some(serde_json::json!({})))
        .await?;
    let response = response.map_err(anyhow::Error::msg)?;
    let sessions: Vec<TuiSessionSummary> = serde_json::from_value(response)?;
    Ok(sessions)
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
    model_profile: Option<&str>,
    capture_mouse: bool,
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
        None => create_session(&mut client, skills, plan_mode, model_profile).await?,
    };

    // Setup terminal.
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    if capture_mouse {
        stdout.execute(EnableMouseCapture)?;
    }
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // Install panic hook to restore terminal.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut stdout = std::io::stdout();
        let _ = disable_raw_mode();
        if capture_mouse {
            let _ = stdout.execute(DisableMouseCapture);
        }
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
    if let Ok(profiles) = load_model_profile_names() {
        app.set_model_profile_candidates(profiles);
    }
    if let Ok(sessions) = list_sessions(&mut client).await {
        app.set_switch_session_candidates(
            sessions
                .into_iter()
                .map(|session| SwitchSessionCandidate {
                    session_id: session.session_id,
                    summary: session.summary.or(session.title),
                })
                .collect(),
        );
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
    if capture_mouse {
        terminal.backend_mut().execute(DisableMouseCapture)?;
    }
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
    let mut last_context_visible = app.context_explorer_active();
    loop {
        // Draw.
        terminal.draw(|frame| {
            if app.context_explorer_active() != last_context_visible {
                frame.render_widget(TuiClear, frame.area());
            }
            ui::draw(frame, app)
        })?;
        last_context_visible = app.context_explorer_active();

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

fn mouse_scroll_step(view_height: u32) -> u32 {
    if view_height > 6 {
        (view_height / 3).max(3)
    } else {
        3
    }
}

#[cfg(not(target_os = "macos"))]
fn try_arboard_copy(text: &str) -> Result<&'static str, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("arboard init failed: {error}"))?;
    clipboard
        .set_text(text.to_string())
        .map_err(|error| format!("arboard copy failed: {error}"))?;
    Ok("arboard")
}

#[cfg(target_os = "macos")]
fn try_pbcopy(text: &str) -> Result<&'static str, String> {
    let mut child = Command::new("/usr/bin/pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to launch pbcopy: {error}"))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| "pbcopy stdin unavailable".to_string())?;
    stdin
        .write_all(text.as_bytes())
        .map_err(|error| format!("failed to write to pbcopy: {error}"))?;
    drop(stdin);
    let status = child
        .wait()
        .map_err(|error| format!("failed waiting for pbcopy: {error}"))?;
    if status.success() {
        Ok("pbcopy")
    } else {
        Err(format!("pbcopy exited with status {status}"))
    }
}

fn try_osc52(text: &str) -> Result<&'static str, String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut stdout = std::io::stdout();
    stdout
        .write_all(format!("\u{1b}]52;c;{encoded}\u{07}").as_bytes())
        .map_err(|error| format!("failed to write OSC 52 clipboard sequence: {error}"))?;
    stdout
        .flush()
        .map_err(|error| format!("failed to flush OSC 52 clipboard sequence: {error}"))?;
    Ok("osc52")
}

fn copy_selection_to_pasteboard(text: &str) -> Result<String, String> {
    let mut errors = Vec::new();

    #[cfg(target_os = "macos")]
    {
        match try_pbcopy(text) {
            Ok(backend) => return Ok(backend.to_string()),
            Err(error) => errors.push(error),
        }

        match try_osc52(text) {
            Ok(backend) => Ok(format!("{backend} ({})", errors.join(" | "))),
            Err(error) => {
                errors.push(error);
                Err(errors.join(" | "))
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    match try_arboard_copy(text) {
        Ok(backend) => return Ok(backend.to_string()),
        Err(error) => errors.push(error),
    }

    #[cfg(not(target_os = "macos"))]
    match try_osc52(text) {
        Ok(backend) => {
            if errors.is_empty() {
                Ok(backend.to_string())
            } else {
                Ok(format!("{backend} ({})", errors.join(" | ")))
            }
        }
        Err(error) => {
            errors.push(error);
            Err(errors.join(" | "))
        }
    }
}

fn copy_current_conversation_selection(app: &mut app::App) {
    let Some(text) = app.selected_conversation_text() else {
        return;
    };
    match copy_selection_to_pasteboard(&text) {
        Ok(backend) => {
            app.set_status_notice(format!(
                "copied {} chars via {backend}",
                text.chars().count()
            ));
        }
        Err(error) => {
            app.set_status_notice(format!("copy failed: {error}"));
        }
    }
}

fn handle_mouse_event(app: &mut app::App, event: MouseEvent) -> Option<AppAction> {
    let step = mouse_scroll_step(app.last_view_height);
    match event.kind {
        MouseEventKind::ScrollUp => {
            if app.context_explorer_active() {
                app.context_explorer_scroll_up(step.min(u16::MAX as u32) as u16);
            } else {
                app.scroll_up(step);
            }
            None
        }
        MouseEventKind::ScrollDown => {
            if app.context_explorer_active() {
                app.context_explorer_scroll_down(step.min(u16::MAX as u32) as u16);
            } else {
                app.scroll_down(step);
            }
            None
        }
        MouseEventKind::Down(MouseButton::Left) => {
            if !app.context_explorer_active()
                && app.begin_conversation_drag_at_mouse(event.column, event.row)
            {
                app.set_status_notice("selection started");
            }
            None
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            if !app.context_explorer_active()
                && app.update_conversation_drag_at_mouse(event.column, event.row)
            {
                if let Some(text) = app.selected_conversation_text() {
                    app.set_status_notice(format!("selecting {} chars", text.chars().count()));
                } else {
                    app.set_status_notice("drag received");
                }
                copy_current_conversation_selection(app);
            }
            None
        }
        MouseEventKind::Up(MouseButton::Left) => {
            if !app.context_explorer_active() {
                if app
                    .finish_conversation_drag_at_mouse(event.column, event.row)
                    .is_some()
                {
                    app.set_status_notice("selection released");
                    copy_current_conversation_selection(app);
                } else {
                    app.set_status_notice("mouse up received");
                }
            }
            None
        }
        _ => None,
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
            if app.is_selecting_options()
                && !app.slash_select_active
                && !app.model_select_active
                && !app.switch_select_active
            {
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

            // Enter accepts the active completion before submitting input.
            if code == KeyCode::Enter {
                if app.switch_select_active {
                    app.accept_switch_session_option();
                    return None;
                }
                if app.model_select_active {
                    app.accept_model_profile_option();
                    return None;
                }
                if app.accept_slash_command_option() {
                    return None;
                }
                return app.submit_input();
            }

            match code {
                KeyCode::Backspace if app.switch_select_active => {
                    app.input.delete_char_before();
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
                    None
                }
                KeyCode::Backspace if app.model_select_active => {
                    app.input.delete_char_before();
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
                    None
                }
                KeyCode::Backspace => {
                    app.input.delete_char_before();
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
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
                    } else if app.input.is_multiline()
                        && app.input.row() < app.input.line_count() - 1
                    {
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
                        app.refresh_model_profile_options();
                        app.refresh_switch_session_options();
                        None
                    }
                }
                KeyCode::Tab => {
                    if app.switch_select_active {
                        app.accept_switch_session_option();
                    } else if app.model_select_active {
                        app.accept_model_profile_option();
                    } else {
                        app.finalize_slash_command_selection();
                    }
                    None
                }
                KeyCode::Char(c) if app.model_select_active => {
                    app.input.insert_char(c);
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
                    None
                }
                KeyCode::Char(c) if app.switch_select_active => {
                    app.input.insert_char(c);
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
                    None
                }
                KeyCode::Char(c) => {
                    app.input.insert_char(c);
                    app.refresh_slash_command_options();
                    app.refresh_model_profile_options();
                    app.refresh_switch_session_options();
                    None
                }
                _ => None,
            }
        }
        Event::Mouse(event) => handle_mouse_event(app, event),
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
                    app.push_message(app::ConversationEntry::Error(format!(
                        "/ps failed: {error}"
                    )));
                }
            }
            match list_sessions(client).await {
                Ok(sessions) => {
                    let session_ids = sessions
                        .into_iter()
                        .map(|session| SwitchSessionCandidate {
                            session_id: session.session_id,
                            summary: session.summary.or(session.title),
                        })
                        .collect();
                    app.set_switch_session_candidates(session_ids);
                }
                Err(error) => {
                    app.push_message(app::ConversationEntry::Error(format!(
                        "failed to refresh session completions: {error}"
                    )));
                }
            }
            app.set_phase(AgentPhase::Idle);
            app.auto_scroll();
        }
        AppAction::ClearSession => {
            let created = create_session(client, available_skills, false, None).await?;
            let new_session_id = created.session_id;
            app.reset_for_new_session(new_session_id.clone(), false, None);
            app.push_message(app::ConversationEntry::AssistantText(format!(
                "Started new session {new_session_id}."
            )));
            app.set_phase(AgentPhase::Idle);
            app.auto_scroll();
        }
        AppAction::SwitchSession { session_id } => {
            match fetch_session_context(client, &session_id).await {
                Ok(context) => {
                    let plan_mode = context.plan_mode;
                    app.reset_for_new_session(session_id.clone(), plan_mode, None);
                    app.load_session_context(context);
                    app.push_message(app::ConversationEntry::AssistantText(format!(
                        "Switched to session {session_id}."
                    )));
                }
                Err(error) => {
                    app.push_message(app::ConversationEntry::Error(format!(
                        "/switch failed: {error}"
                    )));
                }
            }
            app.set_phase(AgentPhase::Idle);
            app.auto_scroll();
        }
        AppAction::SetModelProfile { model_profile } => {
            match set_session_model_profile(client, &app.session_id, &model_profile).await {
                Ok(()) => app.push_message(app::ConversationEntry::AssistantText(format!(
                    "Switched model profile to `{model_profile}`."
                ))),
                Err(error) => app.push_message(app::ConversationEntry::Error(error.to_string())),
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
        } => match create_session(client, skills, true, None).await {
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

    fn sample_tui_session(
        session_id: &str,
        parent_id: Option<&str>,
        status: &str,
    ) -> TuiSessionSummary {
        TuiSessionSummary {
            session_id: session_id.to_string(),
            parent_id: parent_id.map(str::to_string),
            status: status.to_string(),
            title: None,
            summary: None,
            plan_mode: false,
            _depth: 0,
        }
    }

    #[test]
    fn format_tui_ps_table_includes_summary() {
        let output = format_tui_ps_table(vec![sample_tui_session("s1", None, "running")]);
        assert!(output.starts_with("1 sessions · 1 running\n\nSESSION"));
    }

    #[test]
    fn format_tui_ps_tree_includes_summary() {
        let output = format_tui_ps_tree(vec![
            sample_tui_session("parent", None, "waiting"),
            sample_tui_session("child", Some("parent"), "running"),
        ]);
        assert!(output.starts_with("2 sessions · 1 running · 1 waiting\n\n"));
        assert!(output.contains("parent [waiting]"));
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

    #[test]
    fn switch_selector_allows_typing_through_terminal_handler() {
        let mut app = app::App::new("test".into(), false, None);
        app.input.set_from_string("/switch a");
        app.set_switch_session_candidates(vec![
            app::SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: Some("Alpha summary".into()),
            },
            app::SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
        ]);

        let action = handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE)),
        );

        assert!(action.is_none());
        assert_eq!(app.input.content(), "/switch al");
        let options = app
            .option_select
            .as_ref()
            .map(|select| select.options.clone());
        assert_eq!(
            options,
            Some(vec![
                "alpha\tAlpha summary".into(),
                "alpine\tAlpine summary".into()
            ])
        );
    }

    #[test]
    fn switch_selector_enter_submits_switch_action() {
        let mut app = app::App::new("test".into(), false, None);
        app.input.set_from_string("/switch a");
        app.set_switch_session_candidates(vec![
            app::SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: Some("Alpha summary".into()),
            },
            app::SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
        ]);

        let action = handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );

        assert!(action.is_none());
        assert_eq!(app.input.content(), "/switch alpha");
        assert!(!app.switch_select_active);
    }

    #[test]
    fn model_selector_allows_typing_through_terminal_handler() {
        let mut app = app::App::new("test".into(), false, None);
        app.input.set_from_string("/model loc");
        app.set_model_profile_candidates(vec!["local-qwen-coder".into(), "local-fast".into()]);

        let action = handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
        );

        assert!(action.is_none());
        assert_eq!(app.input.content(), "/model loca");
        let options = app
            .option_select
            .as_ref()
            .map(|select| select.options.clone());
        assert_eq!(
            options,
            Some(vec!["local-qwen-coder".into(), "local-fast".into()])
        );
    }

    #[test]
    fn model_selector_enter_applies_selected_profile() {
        let mut app = app::App::new("test".into(), false, None);
        app.input.set_from_string("/model loc");
        app.set_model_profile_candidates(vec!["local-qwen-coder".into(), "local-fast".into()]);

        let action = handle_terminal_event(
            &mut app,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );

        assert!(action.is_none());
        assert_eq!(app.input.content(), "/model local-qwen-coder");
        assert!(!app.model_select_active);
    }

    #[test]
    fn mouse_scroll_up_moves_conversation_scroll() {
        let mut app = app::App::new("test".into(), false, None);
        app.last_view_height = 12;

        let action = handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(action.is_none());
        assert_eq!(app.scroll_offset, 4);
        assert!(app.user_scrolled);
    }

    #[test]
    fn mouse_scroll_down_moves_conversation_toward_bottom() {
        let mut app = app::App::new("test".into(), false, None);
        app.last_view_height = 12;
        app.scroll_up(10);

        let action = handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(action.is_none());
        assert_eq!(app.scroll_offset, 6);
        assert!(app.user_scrolled);
    }

    #[test]
    fn mouse_scroll_targets_context_explorer_when_open() {
        let mut app = app::App::new("test".into(), false, None);
        app.last_view_height = 15;
        app.open_context_explorer(crate::context_debug::SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: Vec::new(),
            working_directory: std::path::PathBuf::from("."),
            plan_mode: false,
            available_tools: Vec::new(),
            loaded_skills: Vec::new(),
            plans: Vec::new(),
            lineage: crate::context_debug::SessionLineageSnapshot {
                parent_id: None,
                root_id: "session-1".into(),
                depth: 0,
                child_ids: Vec::new(),
            },
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: Vec::new(),
        });

        let action = handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(action.is_none());
        assert_eq!(
            app.context_explorer
                .as_ref()
                .map(|explorer| explorer.scroll_offset),
            Some(5)
        );
        assert_eq!(app.scroll_offset, 0);
    }

    #[test]
    fn mouse_drag_updates_conversation_selection() {
        let mut app = app::App::new("test".into(), false, None);
        app.set_conversation_viewport(0, 0, 40, 10, 0);

        let down = handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 2,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
        );
        let drag = handle_terminal_event(
            &mut app,
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: 6,
                row: 1,
                modifiers: KeyModifiers::NONE,
            }),
        );

        assert!(down.is_none());
        assert!(drag.is_none());
        assert_eq!(
            app.conversation_selection,
            Some(app::ConversationSelection {
                anchor: app::ConversationSelectionPoint { row: 1, col: 2 },
                focus: app::ConversationSelectionPoint { row: 1, col: 6 },
            })
        );
    }

    #[test]
    fn load_model_profile_names_reads_yaml_keys() {
        let temp_home = tempfile::TempDir::new().unwrap();
        let quine_dir = temp_home.path().join(".quine");
        std::fs::create_dir_all(&quine_dir).unwrap();
        std::fs::write(
            quine_dir.join("model-profiles.yaml"),
            "profiles:\n  local-qwen-coder:\n    provider: openai\n  local-fast:\n    provider: openai\n",
        )
        .unwrap();
        let previous_home = std::env::var_os("HOME");
        unsafe {
            std::env::set_var("HOME", temp_home.path());
        }

        let names = load_model_profile_names().unwrap();

        match previous_home {
            Some(value) => unsafe { std::env::set_var("HOME", value) },
            None => unsafe { std::env::remove_var("HOME") },
        }

        assert_eq!(
            names,
            vec!["local-fast".to_string(), "local-qwen-coder".to_string()]
        );
    }
}
