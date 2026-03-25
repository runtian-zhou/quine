use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{AgentPhase, App, ConversationEntry, DiffLine};

/// Render the entire TUI frame.
pub fn draw(frame: &mut Frame, app: &App) {
    // Dynamic input box height: expand for option selection.
    let input_height = if let Some(ref select) = app.option_select {
        // 2 for borders + 1 for label + option count, capped at half terminal
        let content_rows = (select.options.len() as u16 + 1).min(frame.area().height / 2);
        content_rows + 2
    } else {
        3
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),               // conversation view
            Constraint::Length(input_height), // input box
        ])
        .split(frame.area());

    draw_conversation(frame, app, chunks[0]);
    draw_input(frame, app, chunks[1]);
}

/// Render the scrollable conversation view.
fn draw_conversation(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, entry) in app.messages.iter().enumerate() {
        match entry {
            ConversationEntry::User(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "You: ",
                        Style::default()
                            .fg(Color::Green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
            }
            ConversationEntry::AssistantText(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::raw(line.to_string())));
                }
            }
            ConversationEntry::ToolCall { tool_name, summary } => {
                let indicator = if app.expanded_tools.contains(&i) {
                    "▼"
                } else {
                    "▶"
                };
                let label = if summary.is_empty() {
                    format!("{indicator} {tool_name}")
                } else {
                    format!("{indicator} {tool_name}: {summary}")
                };
                lines.push(Line::from(Span::styled(
                    label,
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                )));
            }
            ConversationEntry::WriteDiff {
                file_path: _,
                diff_lines,
            } => {
                for dl in diff_lines {
                    match dl {
                        DiffLine::Header(h) => {
                            lines.push(Line::from(Span::styled(
                                h.clone(),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            )));
                        }
                        DiffLine::Add(l) => {
                            lines.push(Line::from(Span::styled(
                                format!("+ {l}"),
                                Style::default().fg(Color::Green),
                            )));
                        }
                        DiffLine::Remove(l) => {
                            lines.push(Line::from(Span::styled(
                                format!("- {l}"),
                                Style::default().fg(Color::Red),
                            )));
                        }
                    }
                }
            }
            ConversationEntry::Error(text) => {
                lines.push(Line::from(Span::styled(
                    format!("Error: {text}"),
                    Style::default().fg(Color::Red),
                )));
            }
            ConversationEntry::InteractionPrompt(text) => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Response: ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(text),
                ]));
            }
        }
    }

    // Show streaming buffer if non-empty.
    if !app.streaming_buffer.is_empty() {
        for line in app.streaming_buffer.lines() {
            lines.push(Line::from(Span::styled(
                line.to_string(),
                Style::default().add_modifier(Modifier::DIM),
            )));
        }
    }

    // Show spinner line based on phase.
    match &app.phase {
        AgentPhase::Thinking => {
            lines.push(Line::from(Span::styled(
                format!("{} Thinking...", app.spinner_char()),
                Style::default().fg(Color::Yellow),
            )));
        }
        AgentPhase::RunningTool(name) => {
            lines.push(Line::from(Span::styled(
                format!("{} Running tool: {}...", app.spinner_char(), name),
                Style::default().fg(Color::Yellow),
            )));
        }
        AgentPhase::Streaming | AgentPhase::Idle => {}
    }

    // Add empty line at the end for spacing.
    if !lines.is_empty() {
        lines.push(Line::from(""));
    }

    let text = Text::from(lines);

    // Calculate scroll: we want to show the bottom unless user scrolled up.
    let content_height = text.lines.len() as u16;
    let view_height = area.height;
    let max_scroll = content_height.saturating_sub(view_height);
    let scroll = if app.user_scrolled {
        max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
    } else {
        max_scroll
    };

    let conversation = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(conversation, area);
}

/// Render the input box at the bottom.
fn draw_input(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    // If in option-select mode, render option list instead of text input.
    if let Some(ref select) = app.option_select {
        let mut lines: Vec<Line> = Vec::new();
        let label = app.input_label();
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(Color::Cyan),
        )));
        for (i, opt) in select.options.iter().enumerate() {
            let is_cursor = i == select.cursor;
            let prefix = if select.multi_select {
                let check = if select.selected.contains(&i) {
                    "x"
                } else {
                    " "
                };
                if is_cursor {
                    format!("  › [{check}] ")
                } else {
                    format!("    [{check}] ")
                }
            } else if is_cursor {
                "  › ".to_string()
            } else {
                "    ".to_string()
            };
            let style = if is_cursor {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(format!("{prefix}{opt}"), style)));
        }
        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
        frame.render_widget(widget, area);
        return;
    }

    let label = app.input_label();
    let display_text = format!("{}{}", label, app.input);

    let input_widget = Paragraph::new(display_text.as_str())
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(input_widget, area);

    // Position cursor after the label + input text.
    let cursor_x = (label.len() + app.cursor_pos) as u16 + 1; // +1 for border
    let cursor_y = area.y + 1; // +1 for border
                               // Clamp to area width.
    let cursor_x = cursor_x.min(area.x + area.width - 2);
    frame.set_cursor_position((area.x + cursor_x, cursor_y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_does_not_panic_empty_app() {
        // Verify rendering logic doesn't panic with empty state.
        let app = App::new("test".into());
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_content() {
        let mut app = App::new("test".into());
        app.messages.push(ConversationEntry::User("hello".into()));
        app.messages
            .push(ConversationEntry::AssistantText("hi there".into()));
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            summary: "echo test".into(),
        });
        app.messages
            .push(ConversationEntry::Error("something failed".into()));
        app.streaming_buffer = "partial...".into();
        app.phase = AgentPhase::RunningTool("bash".into());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &app)).unwrap();
    }
}
