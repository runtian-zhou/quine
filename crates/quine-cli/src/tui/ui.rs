use ratatui::layout::{Alignment, Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{AgentPhase, App, ConversationEntry, InputBuffer, ToolStatus};

/// Format a duration in microseconds to a human-readable string.
fn format_duration_us(us: u64) -> String {
    let secs = us as f64 / 1_000_000.0;
    if secs >= 10.0 {
        format!("{:.0}s", secs)
    } else if secs >= 1.0 {
        format!("{:.1}s", secs)
    } else {
        let ms = us as f64 / 1_000.0;
        if ms >= 1.0 {
            format!("{:.0}ms", ms)
        } else {
            "<1ms".to_string()
        }
    }
}

/// Format token usage in a compact, TUI-friendly form.
fn format_token_usage(usage: &quine_llm::TokenUsage, max_context_window: u64) -> String {
    format!(
        "ctx {current}/{max}",
        current = usage.input_tokens + usage.output_tokens,
        max = max_context_window,
    )
}

/// Render the entire TUI frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Dynamic input box height: expand for option selection or multi-line input.
    let max_height = (frame.area().height / 2).min(12);
    let input_height = if let Some(ref select) = app.option_select {
        // 2 for borders + 1 for label + option count, capped at half terminal
        let content_rows = (select.options.len() as u16 + 1).min(frame.area().height / 2);
        content_rows + 2
    } else {
        let label = app.input_label();
        let content_rows =
            input_content_rows(&app.input, &label, frame.area().width.saturating_sub(2));
        (content_rows + 2).max(3).min(max_height) // +2 for borders
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // status bar
            Constraint::Min(3),               // conversation view
            Constraint::Length(input_height), // input box
        ])
        .split(frame.area());

    draw_status_bar(frame, app, chunks[0]);
    draw_conversation(frame, app, chunks[1]);
    draw_input(frame, app, chunks[2]);
}

fn wrapped_rows(width: usize, area_width: u16) -> u16 {
    if area_width == 0 {
        return 1;
    }
    let area_width = usize::from(area_width);
    width.max(1).div_ceil(area_width) as u16
}

fn input_content_rows(input: &InputBuffer, label: &str, area_width: u16) -> u16 {
    let total_rows: u16 = (0..input.line_count())
        .map(|index| {
            let prefix_width = if index == 0 { label.chars().count() } else { 0 };
            wrapped_rows(prefix_width + input.line(index).chars().count(), area_width)
        })
        .sum();

    let (cursor_row, _) = input_cursor_position(input, label, area_width);
    total_rows.max(cursor_row + 1)
}

fn input_lines(input: &InputBuffer, label: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::with_capacity(input.line_count());
    for index in 0..input.line_count() {
        let prefix = if index == 0 { label } else { "" };
        lines.push(Line::from(format!("{prefix}{}", input.line(index))));
    }
    lines
}

fn input_cursor_position(input: &InputBuffer, label: &str, area_width: u16) -> (u16, u16) {
    if area_width == 0 {
        return (0, 0);
    }

    let mut row = 0u16;
    for index in 0..input.row() {
        let prefix_width = if index == 0 { label.chars().count() } else { 0 };
        row += wrapped_rows(prefix_width + input.line(index).chars().count(), area_width);
    }

    let prefix_width = if input.row() == 0 {
        label.chars().count()
    } else {
        0
    };
    let cursor_width = input.line_prefix_width(input.row(), input.col()) + prefix_width;
    row += (cursor_width / usize::from(area_width)) as u16;
    let col = (cursor_width % usize::from(area_width)) as u16;
    (row, col)
}

fn plan_status_style(status_line: &str) -> Style {
    if status_line.starts_with("✅") {
        Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD)
    } else if status_line.starts_with("🔄") {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else if status_line.starts_with("🟢") {
        Style::default()
            .fg(Color::LightGreen)
            .add_modifier(Modifier::BOLD)
    } else if status_line.starts_with("🟡") {
        Style::default()
            .fg(Color::LightYellow)
            .add_modifier(Modifier::BOLD)
    } else if status_line.starts_with("❌") {
        Style::default()
            .fg(Color::LightRed)
            .add_modifier(Modifier::BOLD)
    } else if status_line.starts_with("⏭️") {
        Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White)
    }
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mode = if app.plan_mode { "plan" } else { "chat" };
    let phase = match &app.phase {
        AgentPhase::Idle => "idle".to_string(),
        AgentPhase::Thinking => format!("{} thinking", app.spinner_char()),
        AgentPhase::Streaming => format!("{} streaming", app.spinner_char()),
        AgentPhase::RunningTool(name) => format!("{} tool:{name}", app.spinner_char()),
    };
    let usage = match (&app.last_turn_usage, app.max_context_window) {
        (Some(usage), Some(max_context_window)) => format_token_usage(usage, max_context_window),
        (Some(usage), None) => format!("ctx {} used", usage.input_tokens + usage.output_tokens),
        (None, Some(max_context_window)) => format!("ctx --/{max_context_window}"),
        (None, None) => "ctx --".to_string(),
    };
    let left = format!(" session:{} | {} | {} ", app.session_id, mode, phase);
    let right = format!(" {} ", usage);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(right.len() as u16)])
        .split(area);

    let left_widget = Paragraph::new(left).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    );
    let right_widget = Paragraph::new(right)
        .style(Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM))
        .alignment(Alignment::Right);

    frame.render_widget(left_widget, chunks[0]);
    frame.render_widget(right_widget, chunks[1]);
}

/// Render the scrollable conversation view.
fn draw_conversation(frame: &mut Frame, app: &mut App, area: ratatui::layout::Rect) {
    let mut lines: Vec<Line<'_>> = Vec::new();

    for (i, entry) in app.messages.iter().enumerate() {
        // Add blank line separator between entries, but keep tool-related
        // entries compact — no blank line before them.
        if i > 0 {
            let is_tool_related = matches!(entry, ConversationEntry::ToolCall { .. });
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
                result_preview,
                ..
            } => {
                let (marker, style) = match status {
                    ToolStatus::Running => ("⟳", Style::default().fg(Color::Yellow)),
                    ToolStatus::Success { .. } => ("✓", Style::default().fg(Color::Green)),
                    ToolStatus::Error { .. } => ("✗", Style::default().fg(Color::Red)),
                };
                let duration_str = match status {
                    ToolStatus::Running => String::new(),
                    ToolStatus::Success { duration_us } | ToolStatus::Error { duration_us } => {
                        format!(" ({})", format_duration_us(*duration_us))
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
                if tool_name == "plan" {
                    if let Some(preview) = result_preview {
                        for line in preview.lines() {
                            let style = if line.starts_with("Plan:") {
                                Style::default()
                                    .fg(Color::White)
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                plan_status_style(line)
                            };
                            lines.push(Line::from(vec![
                                Span::raw("      "),
                                Span::styled(line.to_string(), style),
                            ]));
                        }
                    }
                }
            }
            ConversationEntry::PatchPreview(preview) => {
                for line in preview.lines() {
                    let style = if line.starts_with("+ ") {
                        Style::default().fg(Color::Green)
                    } else if line.starts_with("- ") {
                        Style::default().fg(Color::Red)
                    } else {
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM)
                    };
                    lines.push(Line::from(vec![
                        Span::raw("      "),
                        Span::styled(line.to_string(), style),
                    ]));
                }
            }
            ConversationEntry::PlanProgress {
                action_id,
                status,
                remaining,
                total,
            } => {
                lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("plan", Style::default().fg(Color::Cyan)),
                    Span::styled(
                        format!(" {action_id} -> {status} ({remaining} remaining / {total} total)"),
                        Style::default().fg(Color::Cyan).add_modifier(Modifier::DIM),
                    ),
                ]));
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
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default().fg(Color::Yellow)),
                    Span::styled(
                        prompt.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
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
                let time_str = format_duration_us(*duration_us);
                let token_str = match (usage, app.max_context_window) {
                    (Some(u), Some(max_context_window)) => {
                        format!(" | {}", format_token_usage(u, max_context_window))
                    }
                    (Some(u), None) => {
                        format!(" | ctx {} used", u.input_tokens + u.output_tokens)
                    }
                    (None, _) => String::new(),
                };
                lines.push(Line::from(vec![Span::styled(
                    format!("  ── {time_str}{token_str} ──"),
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                )]));
            }
        }
    }

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

    let text = Text::from(lines);
    let conversation = Paragraph::new(text).wrap(Wrap { trim: false });

    let content_height = conversation.line_count(area.width) as u32;
    let view_height = area.height as u32;
    app.last_view_height = view_height;
    let max_scroll = content_height.saturating_sub(view_height);
    let scroll = if app.user_scrolled {
        max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
    } else {
        max_scroll
    };

    let scroll_u16 = scroll.min(u16::MAX as u32) as u16;
    let conversation = conversation.scroll((scroll_u16, 0));

    frame.render_widget(conversation, area);
}

/// Compute the number of visual rows a single line occupies when wrapped to `area_width`.
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
    let input_widget = Paragraph::new(Text::from(input_lines(&app.input, &label)))
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });

    frame.render_widget(input_widget, area);

    let (cursor_row, cursor_col) =
        input_cursor_position(&app.input, &label, area.width.saturating_sub(2));
    let cursor_y = (area.y + 1 + cursor_row).min(area.y + area.height.saturating_sub(2));
    let cursor_x = (area.x + 1 + cursor_col).min(area.x + area.width.saturating_sub(2));
    frame.set_cursor_position((cursor_x, cursor_y));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer_lines(backend: &ratatui::backend::TestBackend) -> Vec<String> {
        let buffer = backend.buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn format_token_usage_compacts_values() {
        let usage = quine_llm::TokenUsage {
            input_tokens: 1200,
            output_tokens: 350,
        };

        assert_eq!(format_token_usage(&usage, 200_000), "ctx 1550/200000");
    }

    #[test]
    fn draw_does_not_panic_empty_app() {
        let mut app = App::new("test".into(), false, None);
        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
    }

    #[test]
    fn draw_does_not_panic_with_tool_status() {
        let mut app = App::new("test".into(), false, Some(200_000));
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo running".into(),
            status: ToolStatus::Running,
            result_preview: None,
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "read".into(),
            tool_use_id: "tc2".into(),
            summary: "file.txt".into(),
            status: ToolStatus::Success { duration_us: 150 },
            result_preview: None,
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "write".into(),
            tool_use_id: "tc3".into(),
            summary: "output.txt".into(),
            status: ToolStatus::Error { duration_us: 300 },
            result_preview: None,
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
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::User("hello".into()));
        app.messages
            .push(ConversationEntry::AssistantText("hi there".into()));
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo test".into(),
            status: ToolStatus::Running,
            result_preview: None,
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
    fn draw_renders_single_blank_line_between_entries() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::User("hello".into()));
        app.messages
            .push(ConversationEntry::AssistantText("hi there".into()));

        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        let hello_index = lines
            .iter()
            .position(|line| line.contains("You: hello"))
            .unwrap();
        let reply_index = lines
            .iter()
            .position(|line| line.contains("  hi there"))
            .unwrap();

        assert_eq!(reply_index - hello_index, 2);
        assert!(lines[hello_index + 1].is_empty());
    }

    #[test]
    fn draw_trims_leading_blank_lines_from_streaming_buffer() {
        let mut app = App::new("test".into(), false, None);
        app.streaming_buffer = "\n\npartial output".into();
        app.phase = AgentPhase::Streaming;

        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        let partial_index = lines
            .iter()
            .position(|line| line.contains("partial output"))
            .unwrap();

        assert_eq!(partial_index, 1);
        assert!(!lines[partial_index - 1].contains("partial output"));
    }

    #[test]
    fn test_wrapped_line_count_single_row() {
        let line = Line::from("hello");
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_wrapped_line_count_multi_row() {
        let line = Line::from("a".repeat(200));
        assert_eq!(wrapped_line_count(&line, 80), 3);
    }

    #[test]
    fn test_wrapped_line_count_exact_fit() {
        let line = Line::from("x".repeat(80));
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_wrapped_line_count_empty() {
        let line = Line::from("");
        assert_eq!(wrapped_line_count(&line, 80), 1);
    }

    #[test]
    fn test_content_height_with_wrapping() {
        let lines = [
            Line::from("short"),
            Line::from("a".repeat(200)),
            Line::from("x".repeat(80)),
            Line::from(""),
            Line::from("b".repeat(160)),
        ];
        let total: u16 = lines.iter().map(|l| wrapped_line_count(l, 80)).sum();
        assert_eq!(total, 8);
    }

    #[test]
    fn input_height_counts_soft_wrapped_rows() {
        let mut input = InputBuffer::new();
        input.set_from_string("abcdefghijklmnopqrstuvwxyz");

        assert_eq!(input_content_rows(&input, "> ", 10), 3);
    }

    #[test]
    fn input_height_keeps_cursor_visible_at_wrap_boundary() {
        let mut input = InputBuffer::new();
        input.set_from_string("12345678");

        assert_eq!(input_cursor_position(&input, "> ", 10), (1, 0));
        assert_eq!(input_content_rows(&input, "> ", 10), 2);
    }

    #[test]
    fn input_cursor_position_tracks_wrapped_second_line() {
        let mut input = InputBuffer::new();
        input.set_from_string("abc\ndefghijklmnop");

        assert_eq!(input_cursor_position(&input, "> ", 8), (2, 5));
    }
}
