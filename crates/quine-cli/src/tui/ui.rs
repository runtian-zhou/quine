use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{AgentPhase, App, ConversationEntry, DiffLine, ToolStatus};

/// Render the entire TUI frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Dynamic input box height: expand for option selection or multi-line input.
    let max_height = (frame.area().height / 2).min(12);
    let input_height = if let Some(ref select) = app.option_select {
        // 2 for borders + 1 for label + option count, capped at half terminal
        let content_rows = (select.options.len() as u16 + 1).min(frame.area().height / 2);
        content_rows + 2
    } else {
        let content_rows = app.input.line_count().max(1) as u16;
        (content_rows + 2).max(3).min(max_height) // +2 for borders
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
fn draw_conversation(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, entry) in app.messages.iter().enumerate() {
        // Add blank line separator between entries, but keep tool-related
        // entries (ToolCall, WriteDiff) compact — no blank line before them.
        if i > 0 {
            let is_tool_related = matches!(
                entry,
                ConversationEntry::ToolCall { .. } | ConversationEntry::WriteDiff { .. }
            );
            if !is_tool_related {
                lines.push(Line::from(""));
            }
        }
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
                    lines.push(Line::from(Span::raw(format!("  {line}"))));
                }
            }
            ConversationEntry::ToolCall {
                tool_name,
                summary,
                status,
                ..
            } => {
                let (marker, style) = match status {
                    ToolStatus::Running => ("\u{27F3}", Style::default().fg(Color::Yellow)),
                    ToolStatus::Success { .. } => ("\u{2713}", Style::default().fg(Color::Green)),
                    ToolStatus::Error { .. } => ("\u{2717}", Style::default().fg(Color::Red)),
                };
                let duration_str = match status {
                    ToolStatus::Running => String::new(),
                    ToolStatus::Success { duration_us } | ToolStatus::Error { duration_us } => {
                        format!(" ({:.1}s)", *duration_us as f64 / 1_000_000.0)
                    }
                };
                let label = if summary.is_empty() {
                    format!(" {tool_name}{duration_str}")
                } else {
                    format!(" {tool_name}: {summary}{duration_str}")
                };
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(marker, style),
                    Span::styled(label, Style::default().add_modifier(Modifier::DIM)),
                ]));
            }
            ConversationEntry::WriteDiff {
                file_path: _,
                diff_lines,
            } => {
                for dl in diff_lines {
                    match dl {
                        DiffLine::Header(h) => {
                            lines.push(Line::from(Span::styled(
                                format!("    {h}"),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            )));
                        }
                        DiffLine::Add(l) => {
                            lines.push(Line::from(Span::styled(
                                format!("    + {l}"),
                                Style::default().fg(Color::Green),
                            )));
                        }
                        DiffLine::Remove(l) => {
                            lines.push(Line::from(Span::styled(
                                format!("    - {l}"),
                                Style::default().fg(Color::Red),
                            )));
                        }
                    }
                }
            }
            ConversationEntry::Error(text) => {
                for (i, line) in text.lines().enumerate() {
                    let prefix = if i == 0 { "Error: " } else { "       " };
                    lines.push(Line::from(Span::styled(
                        format!("{prefix}{line}"),
                        Style::default().fg(Color::Red),
                    )));
                }
            }
            ConversationEntry::InteractionQuestion { prompt, options } => {
                // Render the question line in yellow/bold.
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        prompt.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                // Render each option as a separate line in cyan.
                for (i, opt) in options.iter().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("     {}. {opt}", i + 1),
                        Style::default().fg(Color::Cyan),
                    )));
                }
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
            ConversationEntry::TurnInfo { duration_us, usage } => {
                let time_str = format!("{:.1}s", *duration_us as f64 / 1_000_000.0);
                let token_str = match usage {
                    Some(u) => {
                        format!(" | {} in / {} out tokens", u.input_tokens, u.output_tokens)
                    }
                    None => String::new(),
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  \u{2500}\u{2500} {time_str}{token_str} \u{2500}\u{2500}"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )]));
            }
        }
    }

    // Show streaming buffer if non-empty, trimming leading blank lines.
    if !app.streaming_buffer.is_empty() {
        let mut started = false;
        for line in app.streaming_buffer.lines() {
            if !started && line.trim().is_empty() {
                continue;
            }
            started = true;
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

    // No trailing blank line — entry separators handle spacing.

    let text = Text::from(lines);

    let conversation = Paragraph::new(text).wrap(Wrap { trim: false });

    // Use ratatui's Paragraph::line_count to get the exact wrapped line count,
    // which accounts for word-boundary wrapping. This fixes scroll calculation
    // that previously undercounted wrapped lines using text.lines.len().
    let content_height = conversation.line_count(area.width) as u32;
    let view_height = area.height as u32;
    // Store view_height in app so PageUp/PageDown can use viewport-proportional steps.
    app.last_view_height = view_height;
    let max_scroll = content_height.saturating_sub(view_height);
    let scroll = if app.user_scrolled {
        max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
    } else {
        max_scroll
    };

    // ratatui scroll takes u16; clamp (content that exceeds u16::MAX lines is
    // extremely unlikely but we handle it gracefully).
    let scroll_u16 = scroll.min(u16::MAX as u32) as u16;
    let conversation = conversation.scroll((scroll_u16, 0));

    frame.render_widget(conversation, area);
}

/// Compute the number of visual rows a single line occupies when wrapped to `area_width`.
///
/// This is a simple ceiling-division approximation. For exact results during
/// rendering we use `Paragraph::line_count`, but this helper is useful for
/// targeted unit tests of the wrapping math.
#[cfg(test)]
fn wrapped_line_count(line: &Line, area_width: u16) -> u16 {
    if area_width == 0 {
        return 1;
    }
    let width = line.width() as u16;
    if width == 0 {
        return 1;
    }
    width.div_ceil(area_width)
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
    // Build display text: label prefixes the first line, subsequent lines have no prefix.
    let content = app.input.content();
    let display_text = format!("{}{}", label, content);

    let input_widget = Paragraph::new(display_text.as_str())
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(input_widget, area);

    // Position cursor for multi-line input.
    let cursor_y = area.y + 1 + app.input.row() as u16; // +1 for border
    let cursor_x = if app.input.row() == 0 {
        (label.len() + app.input.col()) as u16 + 1 // +1 for border
    } else {
        app.input.col() as u16 + 1 // +1 for border
    };
    // Clamp to area bounds.
    let cursor_x = cursor_x.min(area.x + area.width.saturating_sub(2));
    let cursor_y = cursor_y.min(area.y + area.height.saturating_sub(2));
    frame.set_cursor_position((area.x + cursor_x, cursor_y));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draw_does_not_panic_empty_app() {
        // Verify rendering logic doesn't panic with empty state.
        let mut app = App::new("test".into(), false);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_tool_status() {
        let mut app = App::new("test".into(), false);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo running".into(),
            status: ToolStatus::Running,
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "read".into(),
            tool_use_id: "tc2".into(),
            summary: "file.txt".into(),
            status: ToolStatus::Success { duration_us: 150 },
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "write".into(),
            tool_use_id: "tc3".into(),
            summary: "output.txt".into(),
            status: ToolStatus::Error { duration_us: 300 },
        });
        app.messages.push(ConversationEntry::TurnInfo {
            duration_us: 4523,
            usage: Some(quine_llm::TokenUsage {
                input_tokens: 1200,
                output_tokens: 350,
            }),
        });

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_content() {
        let mut app = App::new("test".into(), false);
        app.messages.push(ConversationEntry::User("hello".into()));
        app.messages
            .push(ConversationEntry::AssistantText("hi there".into()));
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo test".into(),
            status: ToolStatus::Running,
        });
        app.messages
            .push(ConversationEntry::Error("something failed".into()));
        app.streaming_buffer = "partial...".into();
        app.phase = AgentPhase::RunningTool("bash".into());

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn test_wrapped_line_count_single_row() {
        // A short line (width < area width) returns 1 row.
        let line = Line::from("hello");
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_wrapped_line_count_multi_row() {
        // A line of width 200 in a 80-column area returns 3 rows.
        let line = Line::from("a".repeat(200));
        assert_eq!(wrapped_line_count(&line, 80), 3);
    }

    #[test]
    fn test_wrapped_line_count_exact_fit() {
        // A line of width 80 in 80-column area returns 1 row.
        let line = Line::from("x".repeat(80));
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_wrapped_line_count_empty() {
        // An empty line returns 1 row.
        let line = Line::from("");
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_content_height_with_wrapping() {
        // Total height for a mix of short and long lines matches expected wrapped row count.
        let lines = [
            Line::from("short"),         // 1 row
            Line::from("a".repeat(200)), // 3 rows in 80-col
            Line::from("x".repeat(80)),  // 1 row (exact fit)
            Line::from(""),              // 1 row (empty)
            Line::from("b".repeat(160)), // 2 rows in 80-col
        ];
        let total: u16 = lines.iter().map(|l| wrapped_line_count(l, 80)).sum();
        assert_eq!(total, 8); // 1 + 3 + 1 + 1 + 2
    }
}
