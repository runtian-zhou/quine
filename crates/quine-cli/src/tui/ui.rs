use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, Instant};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use serde_json::to_string_pretty;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{
    AgentPhase, App, ContextExplorerState, ContextExplorerTab, ConversationEntry,
    ConversationRenderCache, InputBuffer, ToolBatchCall, ToolStatus, UnwindState,
};

/// Format a duration in microseconds to a human-readable string.
fn format_duration_us(us: u64) -> String {
    let secs = us as f64 / 1_000_000.0;
    if secs >= 10.0 {
        format!("{secs:.0}s")
    } else if secs >= 1.0 {
        format!("{secs:.1}s")
    } else {
        let ms = us as f64 / 1_000.0;
        if ms >= 1.0 {
            format!("{ms:.0}ms")
        } else {
            "<1ms".to_string()
        }
    }
}

fn tool_duration_label(status: &ToolStatus) -> Option<String> {
    match status {
        ToolStatus::Running { .. } => None,
        ToolStatus::Success { duration_us } | ToolStatus::Error { duration_us } => {
            Some(format!(" ({})", format_duration_us(*duration_us)))
        }
    }
}

fn format_running_timer(started_at: Instant, timeout: Option<Duration>) -> String {
    let elapsed = started_at.elapsed();
    let elapsed_label = format_duration_us(elapsed.as_micros() as u64);
    match timeout {
        Some(timeout) => {
            let timeout_label = format_duration_us(timeout.as_micros() as u64);
            format!(" ({} / {})", elapsed_label, timeout_label)
        }
        None => format!(" ({elapsed_label})"),
    }
}

/// Format token usage in a compact, TUI-friendly form.
fn format_token_usage(usage: &quine_llm::TokenUsage, max_context_window: u64) -> String {
    let current = usage.input_tokens + usage.output_tokens;
    let percent = current
        .saturating_mul(100)
        .checked_div(max_context_window)
        .unwrap_or(0);
    format!("ctx {percent}%", percent = percent.min(100),)
}

fn format_context_status(
    usage: Option<&quine_llm::TokenUsage>,
    max_context_window: Option<u64>,
) -> String {
    match (usage, max_context_window) {
        (Some(usage), Some(max_context_window)) => format_token_usage(usage, max_context_window),
        (Some(usage), None) => format!("ctx {} used", usage.input_tokens + usage.output_tokens),
        (None, Some(max_context_window)) => format!("ctx limit {max_context_window}"),
        (None, None) => "ctx n/a".to_string(),
    }
}

fn status_report_panel_height(app: &App) -> u16 {
    if app.status_report.is_some() {
        5
    } else {
        0
    }
}

fn status_report_bar(progress_percent: u8, width: usize) -> String {
    let width = width.max(8);
    let filled = usize::from(progress_percent)
        .saturating_mul(width)
        .div_ceil(100);
    let mut bar = String::with_capacity(width + 2);
    bar.push('[');
    for index in 0..width {
        bar.push(if index < filled { '#' } else { '-' });
    }
    bar.push(']');
    bar
}

/// Render the entire TUI frame.
pub fn draw(frame: &mut Frame, app: &mut App) {
    // Dynamic input box height: expand for option selection or multi-line input.
    let max_height = (frame.area().height / 2).min(12);
    let input_height = if let Some(ref select) = app.option_select {
        let label = app.input_label();
        let content_rows =
            input_content_rows(&app.input, &label, frame.area().width.saturating_sub(2))
                .saturating_add(select.options.len() as u16);
        (content_rows + 2).max(3).min(max_height)
    } else {
        let label = app.input_label();
        let mut content_rows =
            input_content_rows(&app.input, &label, frame.area().width.saturating_sub(2));
        if let Some(hints) = app.slash_command_hint() {
            content_rows += hints.len() as u16;
        }
        (content_rows + 2).max(3).min(max_height) // +2 for borders
    };
    let report_height = status_report_panel_height(app);
    let mut constraints = vec![Constraint::Length(1)];
    if report_height > 0 {
        constraints.push(Constraint::Length(report_height));
    }
    constraints.push(Constraint::Min(3));
    constraints.push(Constraint::Length(input_height));
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    draw_status_bar(frame, app, chunks[0]);
    let conversation_index = if report_height > 0 {
        draw_status_report_panel(frame, app, chunks[1]);
        2
    } else {
        1
    };
    draw_conversation(frame, app, chunks[conversation_index]);
    draw_input(frame, app, chunks[conversation_index + 1]);
    if let Some(selector) = app.unwind_selector.as_mut() {
        let view_height = draw_unwind_selector(frame, chunks[conversation_index], selector);
        app.last_context_view_height = u32::from(view_height);
    } else if let Some(explorer) = app.context_explorer.as_mut() {
        let view_height = draw_context_explorer(frame, chunks[conversation_index], explorer);
        app.last_context_view_height = u32::from(view_height);
    } else {
        app.last_context_view_height = 0;
    }
    draw_status_notice_overlay(frame, app);
}

fn wrapped_rows(width: usize, area_width: u16) -> u16 {
    if area_width == 0 {
        return 1;
    }
    let area_width = usize::from(area_width);
    width.max(1).div_ceil(area_width) as u16
}

fn option_window_bounds(total: usize, cursor: usize, visible_rows: usize) -> (usize, usize) {
    if total == 0 || visible_rows == 0 {
        return (0, 0);
    }

    if total <= visible_rows {
        return (0, total);
    }

    let mut start = cursor.saturating_sub(visible_rows.saturating_sub(1));
    let max_start = total.saturating_sub(visible_rows);
    if start > max_start {
        start = max_start;
    }
    let end = (start + visible_rows).min(total);
    (start, end)
}

fn display_width(text: &str) -> usize {
    text.width()
}

fn input_content_rows(input: &InputBuffer, label: &str, area_width: u16) -> u16 {
    let total_rows: u16 = (0..input.line_count())
        .map(|index| {
            let prefix_width = if index == 0 { display_width(label) } else { 0 };
            wrapped_rows(prefix_width + display_width(input.line(index)), area_width)
        })
        .sum();

    let (cursor_row, _) = input_cursor_position(input, label, area_width);
    total_rows.max(cursor_row + 1)
}

fn wrap_input_lines(input: &InputBuffer, label: &str, area_width: u16) -> Vec<Line<'static>> {
    let area_width = usize::from(area_width);
    let mut lines = Vec::new();
    for index in 0..input.line_count() {
        let mut current = if index == 0 {
            label.to_string()
        } else {
            String::new()
        };
        let mut current_width = display_width(&current);

        for ch in input.line(index).chars() {
            let ch_width = ch.width().unwrap_or(0);
            if area_width > 0 && current_width > 0 && current_width + ch_width > area_width {
                lines.push(Line::from(std::mem::take(&mut current)));
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }

        lines.push(Line::from(current));
    }
    lines
}

fn input_cursor_position(input: &InputBuffer, label: &str, area_width: u16) -> (u16, u16) {
    if area_width == 0 {
        return (0, 0);
    }

    let mut row = 0u16;
    for index in 0..input.row() {
        let prefix_width = if index == 0 { display_width(label) } else { 0 };
        row += wrapped_rows(prefix_width + display_width(input.line(index)), area_width);
    }

    let prefix_width = if input.row() == 0 {
        display_width(label)
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

fn wrap_box_lines(text: &str, inner_width: usize) -> Vec<String> {
    let mut wrapped = Vec::new();

    for raw_line in text.lines() {
        let line = if raw_line.is_empty() { " " } else { raw_line };
        let mut current = String::new();
        let mut current_width = 0usize;

        for ch in line.chars() {
            let ch_width = ch.width().unwrap_or(0).max(1);
            if current_width + ch_width > inner_width && !current.is_empty() {
                wrapped.push(current);
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }

        if current.is_empty() {
            wrapped.push(" ".to_string());
        } else {
            wrapped.push(current);
        }
    }

    wrapped
}

fn render_plan_box_with_indent(
    lines: &mut Vec<Line<'static>>,
    plan: &str,
    width: u16,
    indent: usize,
) {
    let reserved = indent + 4;
    let inner_width = usize::from(width).saturating_sub(reserved).max(1);
    let indent_text = " ".repeat(indent);

    lines.push(Line::from(Span::styled(
        format!("{indent_text}┌{}┐", "─".repeat(inner_width + 2)),
        Style::default().fg(Color::Cyan),
    )));

    for line in wrap_box_lines(plan, inner_width) {
        let content_width = line.width();
        let padding = " ".repeat(inner_width.saturating_sub(content_width));
        let style = if line.trim_start().starts_with("Plan:") {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            plan_status_style(line.trim_start())
        };

        lines.push(Line::from(vec![
            Span::styled(format!("{indent_text}│ "), Style::default().fg(Color::Cyan)),
            Span::styled(format!("{line}{padding}"), style),
            Span::styled(" │", Style::default().fg(Color::Cyan)),
        ]));
    }

    lines.push(Line::from(Span::styled(
        format!("{indent_text}└{}┘", "─".repeat(inner_width + 2)),
        Style::default().fg(Color::Cyan),
    )));
}

fn render_plan_box(lines: &mut Vec<Line<'static>>, plan: &str, width: u16) {
    render_plan_box_with_indent(lines, plan, width, 4);
}

fn render_bash_preview_box_with_indent(
    lines: &mut Vec<Line<'static>>,
    preview: &str,
    width: u16,
    indent: usize,
) {
    let reserved = indent + 4;
    let inner_width = usize::from(width).saturating_sub(reserved).max(1);
    let indent_text = " ".repeat(indent);
    let top = format!("{indent_text}┌{}┐", "─".repeat(inner_width + 2));
    let bottom = format!("{indent_text}└{}┘", "─".repeat(inner_width + 2));
    let wrapped = wrap_box_lines(preview, inner_width);

    let clipped = wrapped.len() > 6;
    let visible = if clipped {
        &wrapped[..6]
    } else {
        wrapped.as_slice()
    };
    lines.push(Line::from(Span::styled(
        top,
        Style::default().fg(Color::Cyan),
    )));
    for line in visible {
        let content_width = line.width();
        let padding = " ".repeat(inner_width.saturating_sub(content_width));
        lines.push(Line::from(vec![
            Span::styled(format!("{indent_text}│ "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{line}{padding}"),
                Style::default().fg(Color::White),
            ),
            Span::styled(" │", Style::default().fg(Color::Cyan)),
        ]));
    }
    if clipped {
        let truncation = "… output truncated …";
        let padding = " ".repeat(inner_width.saturating_sub(truncation.width()));
        lines.push(Line::from(vec![
            Span::styled(format!("{indent_text}│ "), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("{truncation}{padding}"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(" │", Style::default().fg(Color::Cyan)),
        ]));
    }
    lines.push(Line::from(Span::styled(
        bottom,
        Style::default().fg(Color::Cyan),
    )));
}

fn render_bash_preview_box(lines: &mut Vec<Line<'static>>, preview: &str, width: u16) {
    render_bash_preview_box_with_indent(lines, preview, width, 6);
}

fn render_grouped_plan_preview(lines: &mut Vec<Line<'static>>, preview: &str, width: u16) {
    render_plan_box_with_indent(lines, preview, width, 6);
}

fn push_tool_batch_entry_lines(
    lines: &mut Vec<Line<'static>>,
    calls: &[ToolBatchCall],
    area_width: u16,
) {
    lines.push(Line::from(vec![
        Span::raw("    "),
        Span::styled(
            format!("▌ Tools ({})", calls.len()),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    for (index, call) in calls.iter().enumerate() {
        let branch = if index + 1 == calls.len() {
            "└"
        } else {
            "├"
        };
        let (marker, marker_style) = match &call.status {
            ToolStatus::Running { .. } => ("⟳", Style::default().fg(Color::Yellow)),
            ToolStatus::Success { .. } => ("✓", Style::default().fg(Color::Green)),
            ToolStatus::Error { .. } => ("✗", Style::default().fg(Color::Red)),
        };
        let label = if call.summary.is_empty() {
            call.tool_name.clone()
        } else {
            format!("{}: {}", call.tool_name, call.summary)
        };
        lines.push(Line::from(vec![
            Span::raw("    "),
            Span::styled(format!("{branch} {marker}"), marker_style),
            Span::raw(format!(" {label}")),
        ]));

        if let Some(preview) = call.result_preview.as_deref() {
            let preview = preview.trim();
            if !preview.is_empty() {
                match call.tool_name.as_str() {
                    "plan" => render_grouped_plan_preview(lines, preview, area_width),
                    "bash" | "web_search" | "web_open" => {
                        render_bash_preview_box(lines, preview, area_width)
                    }
                    _ => {}
                }
            }
        }
    }
}

fn push_conversation_entry_lines(
    lines: &mut Vec<Line<'static>>,
    entry: &ConversationEntry,
    area_width: u16,
    max_context_window: Option<u64>,
) {
    match entry {
        ConversationEntry::User(text) => {
            lines.push(Line::from(vec![
                Span::styled(
                    "You: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(text.clone()),
            ]));
        }
        ConversationEntry::AssistantText(text) => {
            render_markdown_text_lines(lines, text, "  ", Style::default(), area_width);
        }
        ConversationEntry::ToolBatch { calls } => {
            push_tool_batch_entry_lines(lines, calls, area_width);
        }
        ConversationEntry::ToolCall {
            tool_name,
            summary,
            status,
            result_preview,
            ..
        } => {
            let (marker, marker_style) = match status {
                ToolStatus::Running { .. } => ("⟳", Style::default().fg(Color::Yellow)),
                ToolStatus::Success { .. } => ("✓", Style::default().fg(Color::Green)),
                ToolStatus::Error { .. } => ("✗", Style::default().fg(Color::Red)),
            };
            let label = if summary.is_empty() {
                format!(" {tool_name}")
            } else {
                format!(" {tool_name}: {summary}")
            };
            let mut spans = vec![
                Span::raw("    "),
                Span::styled(marker, marker_style),
                Span::styled(label, Style::default().add_modifier(Modifier::DIM)),
            ];
            if let Some(duration_label) = tool_duration_label(status) {
                spans.push(Span::styled(
                    duration_label,
                    Style::default()
                        .fg(Color::DarkGray)
                        .add_modifier(Modifier::DIM),
                ));
            } else if let ToolStatus::Running {
                started_at,
                timeout,
            } = status
            {
                if tool_name == "bash" {
                    spans.push(Span::styled(
                        format_running_timer(*started_at, *timeout),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::DIM),
                    ));
                }
            }
            lines.push(Line::from(spans));
            if tool_name == "plan" {
                if let Some(preview) = result_preview {
                    for line in preview.lines().filter(|line| !line.trim().is_empty()) {
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
            } else if matches!(tool_name.as_str(), "bash" | "web_search" | "web_open") {
                if let Some(preview) = result_preview {
                    render_bash_preview_box(lines, preview, area_width);
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
        ConversationEntry::PlanBox(plan) => {
            render_plan_box(lines, plan, area_width);
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
        ConversationEntry::InteractionPrompt { summary, prompt } => {
            if let Some(summary) = summary.as_deref().filter(|value| !value.trim().is_empty()) {
                lines.push(Line::from(vec![
                    Span::styled(
                        "Summary: ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(summary.to_string()),
                ]));
            }
            lines.push(Line::from(vec![
                Span::styled(
                    "Response: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(prompt.clone()),
            ]));
        }
        ConversationEntry::TurnInfo { duration_us, usage } => {
            let time_str = format_duration_us(*duration_us);
            let token_str = match (usage, max_context_window) {
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

fn append_live_lines(lines: &mut Vec<Line<'static>>, app: &App, area_width: u16) {
    if !app.streaming_buffer.is_empty() {
        let mut started = false;
        let mut visible = Vec::new();
        for line in app.streaming_buffer.lines() {
            if !started && line.trim().is_empty() {
                continue;
            }
            started = true;
            visible.push(line.to_string());
        }
        if !visible.is_empty() {
            render_markdown_text_lines(
                lines,
                &visible.join("\n"),
                "",
                Style::default().add_modifier(Modifier::DIM),
                area_width,
            );
        }
    }

    match &app.phase {
        AgentPhase::Thinking => {
            lines.push(Line::from(Span::styled(
                "Thinking...".to_string(),
                Style::default().fg(Color::Yellow),
            )));
        }
        AgentPhase::RunningTool(name) => {
            lines.push(Line::from(Span::styled(
                format!("Running tool: {name}..."),
                Style::default().fg(Color::Yellow),
            )));
        }
        AgentPhase::Streaming | AgentPhase::Idle => {}
    }
}

fn render_inline_markup_line(prefix: &str, text: &str, base_style: Style) -> Line<'static> {
    let mut spans = Vec::new();
    if !prefix.is_empty() {
        spans.push(Span::styled(prefix.to_string(), base_style));
    }
    spans.extend(render_inline_markup_spans(text, base_style));
    Line::from(spans)
}

fn code_fence_language(line: &str) -> Option<Option<String>> {
    let rest = line.trim_start().strip_prefix("```")?;
    let language = rest.trim();
    Some((!language.is_empty()).then(|| language.to_string()))
}

fn render_markdown_text_lines(
    lines: &mut Vec<Line<'static>>,
    text: &str,
    prefix: &str,
    base_style: Style,
    area_width: u16,
) {
    let mut in_code_block = false;
    let mut code_language = None;
    let mut code_lines = Vec::new();

    for line in text.lines() {
        if let Some(language) = code_fence_language(line) {
            if in_code_block {
                render_code_block_box(
                    lines,
                    &code_lines,
                    code_language.as_deref(),
                    area_width,
                    prefix.width(),
                    base_style,
                );
                in_code_block = false;
                code_language = None;
                code_lines.clear();
            } else {
                in_code_block = true;
                code_language = language;
                code_lines.clear();
            }
            continue;
        }

        if in_code_block {
            code_lines.push(line.to_string());
        } else {
            lines.push(render_inline_markup_line(prefix, line, base_style));
        }
    }

    if in_code_block {
        render_code_block_box(
            lines,
            &code_lines,
            code_language.as_deref(),
            area_width,
            prefix.width(),
            base_style,
        );
    }
}

fn render_code_block_box(
    lines: &mut Vec<Line<'static>>,
    code_lines: &[String],
    language: Option<&str>,
    width: u16,
    indent: usize,
    base_style: Style,
) {
    let reserved = indent + 4;
    let inner_width = usize::from(width).saturating_sub(reserved).max(1);
    let body_width = inner_width + 2;
    let indent_text = " ".repeat(indent);
    let border_style = base_style.fg(Color::Cyan);
    let label_style = base_style.fg(Color::Yellow).add_modifier(Modifier::BOLD);
    let code_style = base_style.fg(Color::White);

    if let Some(language) = language.filter(|value| !value.is_empty()) {
        let label = format!(" {language} ");
        if label.width() < body_width {
            lines.push(Line::from(vec![
                Span::styled(format!("{indent_text}┌"), border_style),
                Span::styled(label.clone(), label_style),
                Span::styled("─".repeat(body_width - label.width()), border_style),
                Span::styled("┐", border_style),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                format!("{indent_text}┌{}┐", "─".repeat(body_width)),
                border_style,
            )));
        }
    } else {
        lines.push(Line::from(Span::styled(
            format!("{indent_text}┌{}┐", "─".repeat(body_width)),
            border_style,
        )));
    }

    let wrapped = if code_lines.is_empty() {
        vec![" ".to_string()]
    } else {
        let code = code_lines.join("\n");
        let wrapped = wrap_box_lines(&code, inner_width);
        if wrapped.is_empty() {
            vec![" ".to_string()]
        } else {
            wrapped
        }
    };
    for line in wrapped {
        let content_width = line.width();
        let padding = " ".repeat(inner_width.saturating_sub(content_width));
        lines.push(Line::from(vec![
            Span::styled(format!("{indent_text}│ "), border_style),
            Span::styled(format!("{line}{padding}"), code_style),
            Span::styled(" │", border_style),
        ]));
    }

    lines.push(Line::from(Span::styled(
        format!("{indent_text}└{}┘", "─".repeat(body_width)),
        border_style,
    )));
}

fn render_inline_markup_spans(text: &str, base_style: Style) -> Vec<Span<'static>> {
    let highlight_style = base_style.fg(Color::White).add_modifier(Modifier::BOLD);
    let quote_style = base_style.fg(Color::Cyan);
    let mut spans = Vec::new();
    let mut plain = String::new();
    let mut index = 0usize;

    let flush_plain = |spans: &mut Vec<Span<'static>>, plain: &mut String| {
        if !plain.is_empty() {
            spans.push(Span::styled(std::mem::take(plain), base_style));
        }
    };

    while index < text.len() {
        let rest = &text[index..];
        if let Some(after_marker) = rest.strip_prefix("**") {
            if let Some(end) = after_marker.find("**").filter(|end| *end > 0) {
                flush_plain(&mut spans, &mut plain);
                spans.push(Span::styled(
                    after_marker[..end].to_string(),
                    highlight_style,
                ));
                index += 2 + end + 2;
                continue;
            }
            plain.push_str("**");
            index += 2;
            continue;
        }

        if let Some(after_marker) = rest.strip_prefix('`') {
            if let Some(end) = after_marker.find('`').filter(|end| *end > 0) {
                flush_plain(&mut spans, &mut plain);
                spans.push(Span::styled(after_marker[..end].to_string(), quote_style));
                index += 1 + end + 1;
                continue;
            }
            plain.push('`');
            index += 1;
            continue;
        }

        let ch = rest
            .chars()
            .next()
            .expect("index is inside a non-empty string");
        plain.push(ch);
        index += ch.len_utf8();
    }

    flush_plain(&mut spans, &mut plain);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base_style));
    }
    spans
}

fn is_tool_group_entry(entry: &ConversationEntry) -> bool {
    matches!(
        entry,
        ConversationEntry::ToolCall { .. }
            | ConversationEntry::PatchPreview(_)
            | ConversationEntry::PlanBox(_)
            | ConversationEntry::PlanProgress { .. }
            | ConversationEntry::TurnInfo { .. }
    )
}

fn should_insert_separator(previous: &ConversationEntry, current: &ConversationEntry) -> bool {
    !(is_tool_group_entry(previous) && is_tool_group_entry(current))
}

fn build_conversation_lines(app: &App, area_width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut previous_entry: Option<ConversationEntry> = None;
    let mut index = 0usize;
    while index < app.messages.len() {
        let entry = if matches!(
            app.messages.get(index),
            Some(ConversationEntry::ToolCall { .. })
        ) {
            let mut calls = Vec::new();
            let mut cursor = index;
            while let Some(ConversationEntry::ToolCall {
                tool_name,
                summary,
                status,
                result_preview,
                ..
            }) = app.messages.get(cursor)
            {
                calls.push(ToolBatchCall {
                    tool_name: tool_name.clone(),
                    summary: summary.clone(),
                    status: status.clone(),
                    result_preview: result_preview.clone(),
                });
                cursor += 1;
            }
            index = cursor;
            if calls.len() > 1 {
                ConversationEntry::ToolBatch { calls }
            } else {
                app.messages[index - 1].clone()
            }
        } else {
            let entry = app.messages[index].clone();
            index += 1;
            entry
        };

        if let Some(previous) = previous_entry.as_ref() {
            if should_insert_separator(previous, &entry) {
                lines.push(Line::from(""));
            }
        }
        push_conversation_entry_lines(&mut lines, &entry, area_width, app.max_context_window);
        previous_entry = Some(entry);
    }
    append_live_lines(&mut lines, app, area_width);
    lines
}

fn wrap_plain_line(text: &str, area_width: u16) -> Vec<String> {
    if area_width == 0 {
        return Vec::new();
    }

    let max_width = usize::from(area_width);
    if text.is_empty() {
        return vec![String::new()];
    }

    let mut wrapped = Vec::new();
    let mut current = String::new();
    let mut current_width = 0usize;

    for ch in text.chars() {
        let ch_width = ch.width().unwrap_or(0).max(1);
        if current_width + ch_width > max_width && !current.is_empty() {
            wrapped.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(ch);
        current_width += ch_width;
    }

    if current.is_empty() {
        wrapped.push(String::new());
    } else {
        wrapped.push(current);
    }

    wrapped
}

fn wrap_styled_line(line: &Line<'static>, area_width: u16) -> Vec<Line<'static>> {
    if area_width == 0 {
        return Vec::new();
    }

    let max_width = usize::from(area_width);
    if line.spans.is_empty() {
        return vec![Line::from("")];
    }

    let mut wrapped = Vec::new();
    let mut current_line_spans: Vec<Span<'static>> = Vec::new();
    let mut current_segment = String::new();
    let mut current_style = Style::default();
    let mut has_segment = false;
    let mut current_width = 0usize;

    let flush_segment = |current_line_spans: &mut Vec<Span<'static>>,
                         current_segment: &mut String,
                         current_style: Style,
                         has_segment: &mut bool| {
        if !current_segment.is_empty() {
            current_line_spans.push(Span::styled(std::mem::take(current_segment), current_style));
        }
        *has_segment = false;
    };

    let flush_line = |wrapped: &mut Vec<Line<'static>>,
                      current_line_spans: &mut Vec<Span<'static>>| {
        if current_line_spans.is_empty() {
            wrapped.push(Line::from(""));
        } else {
            wrapped.push(Line::from(std::mem::take(current_line_spans)));
        }
    };

    for span in &line.spans {
        let style = span.style;
        for ch in span.content.chars() {
            let ch_width = ch.width().unwrap_or(0).max(1);
            if current_width + ch_width > max_width && current_width > 0 {
                flush_segment(
                    &mut current_line_spans,
                    &mut current_segment,
                    current_style,
                    &mut has_segment,
                );
                flush_line(&mut wrapped, &mut current_line_spans);
                current_width = 0;
            }

            if !has_segment {
                current_style = style;
                has_segment = true;
            } else if current_style != style {
                flush_segment(
                    &mut current_line_spans,
                    &mut current_segment,
                    current_style,
                    &mut has_segment,
                );
                current_style = style;
                has_segment = true;
            }

            current_segment.push(ch);
            current_width += ch_width;
        }
    }

    flush_segment(
        &mut current_line_spans,
        &mut current_segment,
        current_style,
        &mut has_segment,
    );
    flush_line(&mut wrapped, &mut current_line_spans);
    wrapped
}

fn build_conversation_wrapped_lines(
    lines: &[Line<'static>],
    area_width: u16,
) -> Vec<Line<'static>> {
    let mut wrapped = Vec::new();
    for line in lines {
        wrapped.extend(wrap_styled_line(line, area_width));
    }
    if wrapped.is_empty() {
        wrapped.push(Line::from(""));
    }
    wrapped
}

fn build_conversation_visual_lines(lines: &[Line<'static>], area_width: u16) -> Vec<String> {
    let mut visual_lines = Vec::new();
    for line in lines {
        visual_lines.extend(wrap_plain_line(&line.to_string(), area_width));
    }
    if visual_lines.is_empty() {
        visual_lines.push(String::new());
    }
    visual_lines
}

fn ensure_conversation_cache(app: &mut App, area_width: u16) -> &ConversationRenderCache {
    let revision = app.conversation_revision();
    let should_rebuild = app
        .conversation_cache
        .as_ref()
        .is_none_or(|cache| cache.width != area_width || cache.revision != revision);

    if should_rebuild {
        let logical_lines = build_conversation_lines(app, area_width);
        let lines = build_conversation_wrapped_lines(&logical_lines, area_width);
        let visual_lines = build_conversation_visual_lines(&lines, area_width);
        let content_height = visual_lines.len() as u32;
        app.conversation_cache = Some(ConversationRenderCache {
            width: area_width,
            revision,
            lines,
            visual_lines,
            content_height,
        });
    }

    app.conversation_cache
        .as_ref()
        .expect("conversation cache initialized")
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mode = if app.plan_mode { "plan" } else { "chat" };
    let phase = app.phase_status_text().to_lowercase();
    let usage = format_context_status(app.last_turn_usage.as_ref(), app.max_context_window);
    let left = format!(" session:{} | {} | {} ", app.session_id, phase, mode);
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

fn draw_status_report_panel(frame: &mut Frame, app: &App, area: Rect) {
    let Some(report) = app.status_report.as_ref() else {
        return;
    };

    let inner_width = usize::from(area.width.saturating_sub(4)).max(12);
    let progress = status_report_bar(report.progress_percent, inner_width.min(24));
    let heading = if report.active {
        format!(
            "Status report · {}% progress · {}% confidence · {} rounds",
            report.progress_percent, report.confidence_percent, report.tool_rounds_observed
        )
    } else {
        format!(
            "Last status report · {}% progress · {}% confidence · {} rounds",
            report.progress_percent, report.confidence_percent, report.tool_rounds_observed
        )
    };
    let text = Text::from(vec![
        Line::from(Span::styled(
            heading,
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(progress, Style::default().fg(Color::Green)),
            Span::raw(format!(" {}%", report.progress_percent)),
        ]),
        Line::from(vec![
            Span::styled(
                "Confidence: ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "{}% chance of fully completing the current request",
                report.confidence_percent
            )),
        ]),
        Line::from(vec![
            Span::styled(
                "Done: ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(report.completed_summary.clone()),
        ]),
        Line::from(vec![
            Span::styled(
                "Next: ",
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(report.remaining_summary.clone()),
        ]),
    ]);
    let widget = Paragraph::new(text)
        .block(Block::default().borders(Borders::ALL).title(" Status "))
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn draw_status_notice_overlay(frame: &mut Frame, app: &App) {
    let Some(notice) = app.current_status_notice() else {
        return;
    };
    let width = (notice.width() as u16 + 4).min(frame.area().width.saturating_sub(2));
    if width < 8 {
        return;
    }
    let area = Rect {
        x: frame.area().width.saturating_sub(width).saturating_sub(1),
        y: 1,
        width,
        height: 3,
    };
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(notice)
            .style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL).title(" Notice ")),
        area,
    );
}

/// Render the scrollable conversation view.
fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    frame.render_widget(Clear, area);

    let (content_height, lines, visual_lines) = {
        let cache = ensure_conversation_cache(app, area.width);
        (
            cache.content_height,
            cache.lines.clone(),
            cache.visual_lines.clone(),
        )
    };
    let view_height = area.height as u32;
    app.last_view_height = view_height;
    let max_scroll = content_height.saturating_sub(view_height);
    let scroll = if app.user_scrolled {
        max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
    } else {
        max_scroll
    };
    app.set_conversation_viewport(area.x, area.y, area.width, area.height, scroll);

    let text = if app.has_conversation_selection() {
        let mut selected_lines = Vec::with_capacity(lines.len());
        for (row, (base_line, line_text)) in lines.iter().zip(visual_lines.iter()).enumerate() {
            if let Some((start_col, end_col)) =
                app.conversation_selection_columns_for_visual_row(row as u32, line_text)
            {
                let mut spans = Vec::new();
                let mut display_col = 0usize;
                for span in &base_line.spans {
                    let mut segment = String::new();
                    let mut segment_selected = None;
                    for ch in span.content.chars() {
                        let ch_width = ch.width().unwrap_or(0).max(1);
                        let ch_start = display_col;
                        let ch_end = display_col + ch_width;
                        let selected = !(ch_end <= start_col || ch_start >= end_col);
                        if segment_selected != Some(selected) && !segment.is_empty() {
                            let style = if segment_selected == Some(true) {
                                span.style.bg(Color::Blue).fg(Color::Black)
                            } else {
                                span.style
                            };
                            spans.push(Span::styled(std::mem::take(&mut segment), style));
                        }
                        segment_selected = Some(selected);
                        segment.push(ch);
                        display_col = ch_end;
                    }
                    if !segment.is_empty() {
                        let style = if segment_selected == Some(true) {
                            span.style.bg(Color::Blue).fg(Color::Black)
                        } else {
                            span.style
                        };
                        spans.push(Span::styled(segment, style));
                    }
                }
                selected_lines.push(if spans.is_empty() {
                    base_line.clone()
                } else {
                    Line::from(spans)
                });
            } else {
                selected_lines.push(base_line.clone());
            }
        }
        Text::from(selected_lines)
    } else {
        Text::from(lines)
    };

    let conversation = Paragraph::new(text).scroll((scroll.min(u16::MAX as u32) as u16, 0));

    frame.render_widget(conversation, area);
}

/// Compute the number of visual rows a single line occupies when wrapped to `area_width`.
fn format_context_entry_label(index: usize, explorer: &ContextExplorerState) -> String {
    match explorer.snapshot.history.get(index) {
        Some(entry) => format_history_entry_label(index, entry),
        None => format!("{:>3}. <missing>", index + 1),
    }
}

fn format_history_entry_label(index: usize, entry: &crate::context_debug::HistoryEntry) -> String {
    let entry_number = index + 1;
    match entry {
        crate::context_debug::HistoryEntry::Text { role, text } => {
            let first_line = text.lines().next().unwrap_or("").trim();
            if first_line.is_empty() {
                format!("{entry_number:>3}. {role}: <blank>")
            } else {
                format!("{entry_number:>3}. {role}: {first_line}")
            }
        }
        crate::context_debug::HistoryEntry::ToolUse {
            role,
            text,
            tool_calls,
        } => {
            let suffix = text
                .as_deref()
                .and_then(|value| value.lines().next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("");
            if tool_calls.len() > 1 {
                let names = tool_calls
                    .iter()
                    .map(|call| call.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                if suffix.is_empty() {
                    format!(
                        "{entry_number:>3}. {role}: tool batch ({}) [{names}]",
                        tool_calls.len()
                    )
                } else {
                    format!(
                        "{entry_number:>3}. {role}: batch ({}) {suffix}",
                        tool_calls.len()
                    )
                }
            } else {
                let tool_summary = tool_calls
                    .first()
                    .map(|call| call.tool_name.as_str())
                    .unwrap_or("tool");
                if suffix.is_empty() {
                    format!("{entry_number:>3}. {role}: tool {tool_summary}")
                } else {
                    format!("{entry_number:>3}. {role}: {suffix}")
                }
            }
        }
        crate::context_debug::HistoryEntry::ToolResult {
            tool_use_id,
            is_error,
            ..
        } => {
            let status = if *is_error { "error" } else { "ok" };
            format!("{entry_number:>3}. tool result {tool_use_id} ({status})")
        }
    }
}

fn format_tool_label(index: usize, explorer: &ContextExplorerState) -> String {
    let item_number = index + 1;
    match explorer.snapshot.available_tools.get(index) {
        Some(tool) => format!("{item_number:>3}. {}", tool.name),
        None => format!("{item_number:>3}. <missing>"),
    }
}

fn format_tool_detail(explorer: &ContextExplorerState) -> String {
    match explorer.selected_tool() {
        Some(tool) => {
            let parameters = serde_json::to_string_pretty(&tool.parameters)
                .unwrap_or_else(|_| tool.parameters.to_string());
            format!(
                "name: {}\n\ndescription:\n{}\n\nparameters:\n{}",
                tool.name, tool.description, parameters
            )
        }
        None => "No tool selected.".to_string(),
    }
}

fn format_skill_label(index: usize, explorer: &ContextExplorerState) -> String {
    let item_number = index + 1;
    match explorer.snapshot.loaded_skills.get(index) {
        Some(skill) => format!("{item_number:>3}. {}", skill.name),
        None => format!("{item_number:>3}. <missing>"),
    }
}

fn format_skill_prompt_preview(skill: &crate::context_debug::SkillSnapshot) -> String {
    match skill.system_prompt.as_deref() {
        Some(system_prompt) if skill.system_prompt_truncated => format!(
            "{system_prompt}\n\n[preview truncated to {} chars; full prompt has {} chars]",
            system_prompt.chars().count(),
            skill.system_prompt_char_count
        ),
        Some(system_prompt) => system_prompt.to_string(),
        None => "<none>".to_string(),
    }
}

fn format_skill_detail(explorer: &ContextExplorerState) -> String {
    match explorer.selected_skill() {
        Some(skill) => {
            let tool_names = if skill.tool_names.is_empty() {
                "<none>".to_string()
            } else {
                skill.tool_names.join(", ")
            };
            let system_prompt = format_skill_prompt_preview(skill);
            format!(
                "name: {}\nversion: {}\nsource: {}\n\ndescription:\n{}\n\nsystem_prompt:\n{}\n\nprompt_chars: {}\nprompt_truncated: {}\n\ntools:\n{}",
                skill.name,
                skill.version,
                skill.source_path.display(),
                skill.description,
                system_prompt,
                skill.system_prompt_char_count,
                skill.system_prompt_truncated,
                tool_names
            )
        }
        None => "No skill selected.".to_string(),
    }
}

fn tool_usage_summary(explorer: &ContextExplorerState) -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for entry in &explorer.snapshot.history {
        if let crate::context_debug::HistoryEntry::ToolUse { tool_calls, .. } = entry {
            for call in tool_calls {
                *counts.entry(call.tool_name.clone()).or_default() += 1;
            }
        }
    }

    if counts.is_empty() {
        return "tools [none]".to_string();
    }

    let tools = counts
        .into_iter()
        .map(|(name, count)| {
            if count == 1 {
                name
            } else {
                format!("{name} x{count}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("tools [{tools}]")
}

fn tool_name_by_use_id(explorer: &ContextExplorerState) -> HashMap<&str, &str> {
    let mut names = HashMap::new();
    for entry in &explorer.snapshot.history {
        if let crate::context_debug::HistoryEntry::ToolUse { tool_calls, .. } = entry {
            for call in tool_calls {
                names.insert(call.tool_use_id.as_str(), call.tool_name.as_str());
            }
        }
    }
    names
}

fn format_context_entry_detail(explorer: &ContextExplorerState) -> String {
    let tool_names = tool_name_by_use_id(explorer);
    match explorer.selected_entry() {
        Some(crate::context_debug::HistoryEntry::Text { role, text }) => {
            format!("kind: text\nrole: {role}\n\n{text}")
        }
        Some(crate::context_debug::HistoryEntry::ToolUse {
            role,
            text,
            tool_calls,
        }) => {
            let mut detail = format!("kind: tool_use\nrole: {role}\n");
            if tool_calls.len() > 1 {
                detail.push_str(&format!("tool_batch_size: {}\n", tool_calls.len()));
                let tool_names = tool_calls
                    .iter()
                    .map(|call| call.tool_name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                detail.push_str(&format!("tool_batch: [{tool_names}]\n"));
            }
            if let Some(text) = text {
                detail.push_str("\ntext:\n");
                detail.push_str(text);
                detail.push('\n');
            }
            for (index, call) in tool_calls.iter().enumerate() {
                detail.push_str(&format!(
                    "\ntool_call {}\n- id: {}\n- name: {}\n- arguments:\n{}\n",
                    index + 1,
                    call.tool_use_id,
                    call.tool_name,
                    to_string_pretty(&call.arguments)
                        .unwrap_or_else(|_| call.arguments.to_string())
                ));
            }
            detail
        }
        Some(crate::context_debug::HistoryEntry::ToolResult {
            role,
            tool_use_id,
            output,
            is_error,
        }) => {
            let status = if *is_error { "error" } else { "ok" };
            let tool_name = tool_names
                .get(tool_use_id.as_str())
                .copied()
                .unwrap_or("<unknown>");
            format!(
                "kind: tool_result\nrole: {role}\ntool_use_id: {tool_use_id}\ntool_name: {tool_name}\nstatus: {status}\n\n{output}"
            )
        }
        None => "No entry selected.".to_string(),
    }
}

fn format_plan_status(status: &crate::context_debug::PlanActionStatusSnapshot) -> &str {
    status.label()
}

fn format_plans_tab_lines(explorer: &ContextExplorerState) -> Vec<Line<'static>> {
    if explorer.snapshot.plans.is_empty() {
        return vec![Line::from("No plans recorded.")];
    }

    let mut lines = Vec::new();
    for plan in &explorer.snapshot.plans {
        lines.push(Line::from(vec![
            Span::styled(
                plan.title.clone(),
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                " — {} remaining / {} actions",
                plan.actions
                    .iter()
                    .filter(|action| {
                        !matches!(
                            action.status,
                            crate::context_debug::PlanActionStatusSnapshot::Completed
                                | crate::context_debug::PlanActionStatusSnapshot::Failed { .. }
                                | crate::context_debug::PlanActionStatusSnapshot::Skipped { .. }
                        )
                    })
                    .count(),
                plan.actions.len()
            )),
        ]));
        lines.push(Line::from(format!("  id: {}", plan.plan_id)));
        for action in &plan.actions {
            lines.push(Line::from(format!(
                "  - {} [{}] {}",
                action.action_id,
                format_plan_status(&action.status),
                action.title
            )));
            if !action.description.is_empty() {
                lines.push(Line::from(format!("      {}", action.description)));
            }
            if !action.depends_on.is_empty() {
                let deps = action
                    .depends_on
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(", ");
                lines.push(Line::from(format!("      depends_on: {deps}")));
            }
            if let Some(result) = &action.result {
                for line in result.lines() {
                    lines.push(Line::from(format!("      result: {line}")));
                }
            }
        }
        lines.push(Line::from(""));
    }
    lines
}

fn lineage_summary(snapshot: &crate::context_debug::SessionContextSnapshot) -> String {
    let parent = snapshot.lineage.parent_id.as_deref().unwrap_or("<root>");
    format!(
        "lineage root {} | parent {} | depth {} | children {}",
        snapshot.lineage.root_id,
        parent,
        snapshot.lineage.depth,
        snapshot.lineage.child_ids.len()
    )
}

fn compact_summary_lines(
    snapshot: &crate::context_debug::SessionContextSnapshot,
) -> Vec<Line<'static>> {
    let Some(summary) = snapshot.compact_memory_summary_markdown.as_deref() else {
        return vec![Line::from("session summary <none>")];
    };

    let mut lines = vec![Line::from("session summary")];
    for line in summary
        .lines()
        .filter(|line| {
            let trimmed = line.trim();
            !trimmed.is_empty() && trimmed != "# Session Summary"
        })
        .take(2)
    {
        lines.push(Line::from(format!("  {line}")));
    }
    lines
}

fn draw_context_explorer(
    frame: &mut Frame,
    area: Rect,
    explorer: &mut ContextExplorerState,
) -> u16 {
    let popup = centered_rect(90, 85, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Context Explorer ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Clear, inner);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let summary = vec![
        Line::from(format!(
            "session {} | state {} | {} entries",
            explorer.snapshot.session_id,
            explorer.snapshot.state,
            explorer.snapshot.history.len()
        )),
        Line::from(format!(
            "skills [{}] | {} | plan_mode {}",
            explorer.snapshot.skills.join(", "),
            tool_usage_summary(explorer),
            explorer.snapshot.plan_mode,
        )),
        Line::from(format!(
            "cwd {}",
            explorer.snapshot.working_directory.display()
        )),
        Line::from(format!(
            "created {} | system_prompt {}",
            explorer.snapshot.created_at,
            explorer
                .snapshot
                .system_prompt
                .as_deref()
                .filter(|value| !value.is_empty())
                .unwrap_or("<none>")
        )),
        Line::from(lineage_summary(&explorer.snapshot)),
    ];
    let mut summary = summary;
    summary.extend(compact_summary_lines(&explorer.snapshot));
    frame.render_widget(Clear, sections[0]);
    frame.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: false }),
        sections[0],
    );

    let tab_titles = ["History", "Tools", "Skills", "Plans"];
    let tab_index = match explorer.active_tab {
        ContextExplorerTab::History => 0,
        ContextExplorerTab::Tools => 1,
        ContextExplorerTab::Skills => 2,
        ContextExplorerTab::Plans => 3,
    };
    let tabs = Tabs::new(tab_titles)
        .select(tab_index)
        .style(Style::default().fg(Color::DarkGray))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(Clear, sections[1]);
    frame.render_widget(tabs, sections[1]);

    let scrollable_height = match explorer.active_tab {
        ContextExplorerTab::History => {
            frame.render_widget(Clear, sections[2]);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);
            paint_blank_area(frame, columns[1]);

            let list_items: Vec<ListItem> = explorer
                .snapshot
                .history
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let label = format!(
                        "{}{}",
                        if index == explorer.selected_index {
                            "› "
                        } else {
                            "  "
                        },
                        format_context_entry_label(index, explorer)
                    );
                    ListItem::new(Line::from(label))
                })
                .collect();
            let list_height = columns[0].height.saturating_sub(2) as usize;
            let list_scroll = context_list_scroll(
                explorer.selected_index,
                explorer.snapshot.history.len(),
                list_height,
            );
            let list = List::new(list_items)
                .block(Block::default().title(" Entries ").borders(Borders::ALL))
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("");
            let mut list_state = ListState::default().with_selected(Some(explorer.selected_index));
            *list_state.offset_mut() = usize::from(list_scroll);
            frame.render_stateful_widget(list, columns[0], &mut list_state);

            let detail_block = Block::default().title(" Detail ").borders(Borders::ALL);
            let detail_inner = detail_block.inner(columns[1]);
            frame.render_widget(detail_block, columns[1]);
            let detail = format_context_entry_detail(explorer);
            render_text_area(frame, detail_inner, &detail, &mut explorer.scroll_offset);
            detail_inner.height
        }
        ContextExplorerTab::Tools => {
            frame.render_widget(Clear, sections[2]);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);
            paint_blank_area(frame, columns[1]);

            let list_items: Vec<ListItem> = explorer
                .snapshot
                .available_tools
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let label = format!(
                        "{}{}",
                        if index == explorer.selected_index {
                            "› "
                        } else {
                            "  "
                        },
                        format_tool_label(index, explorer)
                    );
                    ListItem::new(Line::from(label))
                })
                .collect();
            let list_height = columns[0].height.saturating_sub(2) as usize;
            let list_scroll = context_list_scroll(
                explorer.selected_index,
                explorer.snapshot.available_tools.len(),
                list_height,
            );
            let list = List::new(list_items)
                .block(Block::default().title(" Tools ").borders(Borders::ALL))
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("");
            let mut list_state = ListState::default().with_selected(Some(explorer.selected_index));
            *list_state.offset_mut() = usize::from(list_scroll);
            frame.render_stateful_widget(list, columns[0], &mut list_state);

            let detail_block = Block::default()
                .title(" Tool Detail ")
                .borders(Borders::ALL);
            let detail_inner = detail_block.inner(columns[1]);
            frame.render_widget(detail_block, columns[1]);
            let detail = format_tool_detail(explorer);
            render_text_area(frame, detail_inner, &detail, &mut explorer.scroll_offset);
            detail_inner.height
        }
        ContextExplorerTab::Skills => {
            frame.render_widget(Clear, sections[2]);
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);
            paint_blank_area(frame, columns[1]);

            let list_items: Vec<ListItem> = explorer
                .snapshot
                .loaded_skills
                .iter()
                .enumerate()
                .map(|(index, _)| {
                    let label = format!(
                        "{}{}",
                        if index == explorer.selected_index {
                            "› "
                        } else {
                            "  "
                        },
                        format_skill_label(index, explorer)
                    );
                    ListItem::new(Line::from(label))
                })
                .collect();
            let list_height = columns[0].height.saturating_sub(2) as usize;
            let list_scroll = context_list_scroll(
                explorer.selected_index,
                explorer.snapshot.loaded_skills.len(),
                list_height,
            );
            let list = List::new(list_items)
                .block(Block::default().title(" Skills ").borders(Borders::ALL))
                .highlight_style(
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("");
            let mut list_state = ListState::default().with_selected(Some(explorer.selected_index));
            *list_state.offset_mut() = usize::from(list_scroll);
            frame.render_stateful_widget(list, columns[0], &mut list_state);

            let detail_block = Block::default()
                .title(" Skill Detail ")
                .borders(Borders::ALL);
            let detail_inner = detail_block.inner(columns[1]);
            frame.render_widget(detail_block, columns[1]);
            let detail = format_skill_detail(explorer);
            render_text_area(frame, detail_inner, &detail, &mut explorer.scroll_offset);
            detail_inner.height
        }
        ContextExplorerTab::Plans => {
            frame.render_widget(Clear, sections[2]);
            let plans_block = Block::default().title(" Plans ").borders(Borders::ALL);
            let plans_inner = plans_block.inner(sections[2]);
            frame.render_widget(plans_block, sections[2]);
            let lines = format_plans_tab_lines(explorer);
            render_lines_area(frame, plans_inner, &lines, &mut explorer.scroll_offset);
            plans_inner.height
        }
    };

    let footer = Paragraph::new("Esc close • ←→/h l tabs • ↑↓/j k navigate • PgUp/PgDn scroll")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        );
    frame.render_widget(Clear, sections[3]);
    frame.render_widget(footer, sections[3]);
    scrollable_height
}

fn draw_unwind_selector(frame: &mut Frame, area: Rect, selector: &mut UnwindState) -> u16 {
    let popup = centered_rect(86, 75, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Unwind Context ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    frame.render_widget(Clear, inner);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let selected_entry = selector
        .selected_history_index()
        .map(|index| index + 1)
        .unwrap_or(0);
    let unwind_target_entry = selected_entry.saturating_sub(1);
    let summary = vec![
        Line::from(format!(
            "session {} | state {} | {} user inputs | {} entries",
            selector.snapshot.session_id,
            selector.snapshot.state,
            selector.entry_count(),
            selector.snapshot.history.len()
        )),
        Line::from(format!(
            "rewind to before entry {selected_entry}; model context keeps entries through {unwind_target_entry}"
        )),
    ];
    frame.render_widget(Clear, sections[0]);
    frame.render_widget(
        Paragraph::new(summary).wrap(Wrap { trim: false }),
        sections[0],
    );

    frame.render_widget(Clear, sections[1]);
    let entry_count = selector.entry_count();
    if entry_count == 0 {
        frame.render_widget(
            Paragraph::new("No user input history available.")
                .alignment(Alignment::Center)
                .block(Block::default().title(" Entries ").borders(Borders::ALL)),
            sections[1],
        );
    } else {
        if selector.selected_index >= entry_count {
            selector.selected_index = entry_count - 1;
        }
        let list_items: Vec<ListItem> = selector
            .user_history_entries()
            .enumerate()
            .map(|(index, (history_index, entry))| {
                let marker = if index == selector.selected_index {
                    "> "
                } else {
                    "  "
                };
                ListItem::new(Line::from(format!(
                    "{marker}{}",
                    format_history_entry_label(history_index, entry)
                )))
            })
            .collect();
        let list_height = sections[1].height.saturating_sub(2) as usize;
        let list_scroll = context_list_scroll(selector.selected_index, entry_count, list_height);
        selector.scroll_offset = list_scroll;
        let list = List::new(list_items)
            .block(Block::default().title(" Entries ").borders(Borders::ALL))
            .highlight_style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("");
        let mut list_state = ListState::default().with_selected(Some(selector.selected_index));
        *list_state.offset_mut() = usize::from(selector.scroll_offset);
        frame.render_stateful_widget(list, sections[1], &mut list_state);
    }

    let footer = Paragraph::new("Esc close • ↑↓/j k choose • Enter unwind")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        );
    frame.render_widget(Clear, sections[2]);
    frame.render_widget(footer, sections[2]);
    sections[1].height.saturating_sub(2)
}

fn paint_blank_area(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let blank_line = " ".repeat(area.width as usize);
    let style = Style::default().fg(Color::White).bg(Color::Reset);
    let buffer = frame.buffer_mut();
    for y in area.top()..area.bottom() {
        buffer.set_string(area.left(), y, &blank_line, style);
    }
}

fn render_text_area(frame: &mut Frame, area: Rect, text: &str, scroll_offset: &mut u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    paint_blank_area(frame, area);
    let wrapped = wrap_box_lines(text, area.width as usize);
    render_wrapped_lines(frame, area, &wrapped, scroll_offset);
}

fn render_lines_area(
    frame: &mut Frame,
    area: Rect,
    lines: &[Line<'static>],
    scroll_offset: &mut u16,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    paint_blank_area(frame, area);
    let text = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    let wrapped = wrap_box_lines(&text, area.width as usize);
    render_wrapped_lines(frame, area, &wrapped, scroll_offset);
}

fn render_wrapped_lines(
    frame: &mut Frame,
    area: Rect,
    wrapped: &[String],
    scroll_offset: &mut u16,
) {
    let style = Style::default().fg(Color::White).bg(Color::Reset);
    let buffer = frame.buffer_mut();
    let visible_rows = area.height as usize;
    let max_scroll = wrapped
        .len()
        .saturating_sub(visible_rows)
        .min(usize::from(u16::MAX)) as u16;
    *scroll_offset = (*scroll_offset).min(max_scroll);
    let start = usize::from(*scroll_offset);

    for row in 0..visible_rows {
        let y = area.top() + row as u16;
        let line = wrapped.get(start + row).map_or("", String::as_str);
        let visible: String = line.chars().take(area.width as usize).collect();
        buffer.set_stringn(area.left(), y, &visible, area.width as usize, style);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn context_list_scroll(selected_index: usize, entry_count: usize, visible_rows: usize) -> u16 {
    if visible_rows == 0 || entry_count <= visible_rows {
        return 0;
    }
    let max_scroll = entry_count.saturating_sub(visible_rows);
    let preferred = selected_index.saturating_sub(visible_rows / 2);
    preferred.min(max_scroll) as u16
}

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
    frame.render_widget(Clear, area);

    if let Some(ref select) = app.option_select {
        if app.slash_select_active {
            let label = app.input_label();
            let mut lines = wrap_input_lines(&app.input, &label, area.width.saturating_sub(2));
            let input_rows = lines.len();
            let visible_rows = area.height.saturating_sub(2 + input_rows as u16) as usize;
            let (start, end) =
                option_window_bounds(select.options.len(), select.cursor, visible_rows);
            for (index, option) in select.options[start..end].iter().enumerate() {
                let absolute_index = start + index;
                let is_cursor = absolute_index == select.cursor;
                let style = if is_cursor {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let (command, help) = option.split_once('\t').unwrap_or((option.as_str(), ""));
                lines.push(Line::from(vec![
                    Span::styled(format!("  {:<10}", command), style),
                    Span::styled(help.to_string(), Style::default().fg(Color::DarkGray)),
                ]));
            }
            let widget =
                Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::ALL));
            frame.render_widget(widget, area);

            let (cursor_row, cursor_col) =
                input_cursor_position(&app.input, &label, area.width.saturating_sub(2));
            let cursor_y = (area.y + 1 + cursor_row).min(area.y + area.height.saturating_sub(2));
            let cursor_x = (area.x + 1 + cursor_col).min(area.x + area.width.saturating_sub(2));
            frame.set_cursor_position((cursor_x, cursor_y));
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        let label = app.input_label();
        lines.extend(wrap_input_lines(
            &app.input,
            &label,
            area.width.saturating_sub(2),
        ));
        let input_rows = lines.len();
        let visible_rows = area.height.saturating_sub(2 + input_rows as u16) as usize;
        let (start, end) = option_window_bounds(select.options.len(), select.cursor, visible_rows);
        for (index, opt) in select.options[start..end].iter().enumerate() {
            let absolute_index = start + index;
            let is_cursor = absolute_index == select.cursor;
            let prefix = if select.multi_select {
                let check = if select.selected.contains(&absolute_index) {
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
            let (primary, secondary) = opt.split_once('\t').unwrap_or((opt.as_str(), ""));
            let mut spans = vec![Span::styled(format!("{prefix}{primary}"), style)];
            if !secondary.is_empty() {
                spans.push(Span::styled(
                    format!("  {secondary}"),
                    Style::default().fg(Color::DarkGray),
                ));
            }
            lines.push(Line::from(spans));
        }
        let widget = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
        frame.render_widget(widget, area);

        let (cursor_row, cursor_col) =
            input_cursor_position(&app.input, &label, area.width.saturating_sub(2));
        let cursor_y = (area.y + 1 + cursor_row).min(area.y + area.height.saturating_sub(2));
        let cursor_x = (area.x + 1 + cursor_col).min(area.x + area.width.saturating_sub(2));
        frame.set_cursor_position((cursor_x, cursor_y));
        return;
    }

    let label = app.input_label();
    let mut lines = wrap_input_lines(&app.input, &label, area.width.saturating_sub(2));
    if let Some(hints) = app.slash_command_hint() {
        for (command, help) in hints {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("  {:<10}", command),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(help.to_string(), Style::default().fg(Color::DarkGray)),
            ]));
        }
    }
    let input_widget =
        Paragraph::new(Text::from(lines)).block(Block::default().borders(Borders::ALL));

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
    use crate::context_debug::{HistoryEntry, SessionContextSnapshot};
    use crate::tui::app::ConversationEntry;
    use ratatui::layout::{Position, Rect};
    use std::time::{Duration, Instant};

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

    fn non_input_conversation_lines(
        backend: &ratatui::backend::TestBackend,
        app: &App,
        terminal_width: u16,
        terminal_height: u16,
    ) -> Vec<String> {
        let max_height = (terminal_height / 2).min(12);
        let label = app.input_label();
        let content_rows = input_content_rows(&app.input, &label, terminal_width.saturating_sub(2));
        let input_height = (content_rows + 2).max(3).min(max_height);
        let conversation_height = terminal_height.saturating_sub(1 + input_height);
        let buffer = backend.buffer();
        let conversation_area = Rect::new(0, 1, terminal_width, conversation_height);

        (conversation_area.y..conversation_area.y + conversation_area.height)
            .map(|y| {
                (conversation_area.x..conversation_area.x + conversation_area.width)
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
            input_tokens: 120_000,
            output_tokens: 30_000,
        };

        assert_eq!(format_token_usage(&usage, 200_000), "ctx 75%");
    }

    #[test]
    fn format_token_usage_clamps_to_full_context() {
        let usage = quine_llm::TokenUsage {
            input_tokens: 180_000,
            output_tokens: 40_000,
        };

        assert_eq!(format_token_usage(&usage, 200_000), "ctx 100%");
    }

    #[test]
    fn format_context_status_avoids_placeholder_dashes() {
        assert_eq!(
            format_context_status(None, Some(200_000)),
            "ctx limit 200000"
        );
        assert_eq!(format_context_status(None, None), "ctx n/a");
    }

    #[test]
    fn render_inline_markup_styles_highlights_and_quotes() {
        let line =
            render_inline_markup_line("  ", "fix **cache** with `reqwest`", Style::default());

        assert_eq!(line.to_string(), "  fix cache with reqwest");
        assert_eq!(line.spans[2].content.as_ref(), "cache");
        assert!(line.spans[2].style.add_modifier.contains(Modifier::BOLD));
        assert_eq!(line.spans[2].style.fg, Some(Color::White));
        assert_eq!(line.spans[4].content.as_ref(), "reqwest");
        assert_eq!(line.spans[4].style.fg, Some(Color::Cyan));
    }

    #[test]
    fn render_inline_markup_preserves_unmatched_delimiters() {
        let line = render_inline_markup_line("", "keep **open and `dangling", Style::default());

        assert_eq!(line.to_string(), "keep **open and `dangling");
        assert!(line
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn assistant_text_renders_fenced_code_block_without_delimiters() {
        let mut lines = Vec::new();

        push_conversation_entry_lines(
            &mut lines,
            &ConversationEntry::AssistantText(
                "Before\n```rust\nlet value = 42;\nprintln!(\"{value}\");\n```\nAfter".into(),
            ),
            80,
            None,
        );

        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Before"));
        assert!(rendered.contains("After"));
        assert!(rendered.contains("let value = 42;"));
        assert!(rendered.contains("println!(\"{value}\");"));
        assert!(rendered.contains("rust"));
        assert!(!rendered.contains("```"));

        let code_line = lines
            .iter()
            .find(|line| line.to_string().contains("let value = 42;"))
            .expect("code line should render");
        assert!(code_line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Color::White)));
        assert!(code_line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(Color::Cyan)));
    }

    #[test]
    fn assistant_text_renders_unclosed_fenced_code_block() {
        let mut lines = Vec::new();

        push_conversation_entry_lines(
            &mut lines,
            &ConversationEntry::AssistantText("```bash\necho running".into()),
            80,
            None,
        );

        let rendered = lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("bash"));
        assert!(rendered.contains("echo running"));
        assert!(!rendered.contains("```"));
    }

    #[test]
    fn draw_context_explorer_marks_selected_entry() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::Text {
                    role: "user".into(),
                    text: "first".into(),
                },
                HistoryEntry::Text {
                    role: "assistant".into(),
                    text: "second".into(),
                },
            ],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_move_down();

        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let selected = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "›")
            .count();
        assert!(selected >= 1);
    }

    #[test]
    fn draw_unwind_selector_lists_history_entries() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::Text {
                    role: "user".into(),
                    text: "first".into(),
                },
                HistoryEntry::Text {
                    role: "assistant".into(),
                    text: "second".into(),
                },
                HistoryEntry::ToolUse {
                    role: "assistant".into(),
                    text: None,
                    tool_calls: vec![crate::context_debug::ToolCallEntry {
                        tool_use_id: "toolu_hidden".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "echo hidden"}),
                    }],
                },
                HistoryEntry::ToolResult {
                    role: "tool".into(),
                    tool_use_id: "toolu_hidden".into(),
                    output: "hidden".into(),
                    is_error: false,
                },
                HistoryEntry::Text {
                    role: "user".into(),
                    text: "third".into(),
                },
            ],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_unwind_selector(snapshot);

        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let rendered = buffer_lines(terminal.backend()).join("\n");
        assert!(rendered.contains("Unwind Context"));
        assert!(rendered.contains("user: first"));
        assert!(rendered.contains("user: third"));
        assert!(!rendered.contains("assistant: second"));
        assert!(!rendered.contains("toolu_hidden"));
        assert!(app.last_context_view_height > 0);
    }

    #[test]
    fn draw_context_explorer_keeps_selected_marker_after_detail_scroll() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::Text {
                    role: "user".into(),
                    text: "first".into(),
                },
                HistoryEntry::Text {
                    role: "assistant".into(),
                    text: (0..80)
                        .map(|index| format!("line {index}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                },
            ],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_move_down();
        app.context_explorer_scroll_down(20);

        let backend = ratatui::backend::TestBackend::new(100, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let buffer = terminal.backend().buffer();
        let selected = buffer
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "›" && cell.bg == Color::Yellow)
            .count();
        assert!(selected >= 1);
    }

    #[test]
    fn draw_context_explorer_records_detail_height_and_clamps_scroll() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![HistoryEntry::Text {
                role: "user".into(),
                text: "short context entry".into(),
            }],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_scroll_down(200);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert!(app.last_context_view_height > 0);
        assert!(app.last_context_view_height < app.last_view_height);
        assert_eq!(
            app.context_explorer
                .as_ref()
                .map(|explorer| explorer.scroll_offset),
            Some(0)
        );
    }

    #[test]
    fn closing_context_explorer_clears_overlay_content() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: Some("system prompt".into()),
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![HistoryEntry::Text {
                role: "user".into(),
                text: "hello world".into(),
            }],
        };
        let mut app = App::new("test".into(), false, None);
        app.messages
            .push(ConversationEntry::AssistantText("base conversation".into()));
        app.open_context_explorer(snapshot);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        app.close_context_explorer();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(!lines.iter().any(|line| line.contains("Context Explorer")));
        assert!(lines.iter().any(|line| line.contains("base conversation")));
    }

    #[test]
    fn context_list_scroll_keeps_selected_item_visible() {
        assert_eq!(context_list_scroll(0, 20, 5), 0);
        assert_eq!(context_list_scroll(4, 20, 5), 2);
        assert_eq!(context_list_scroll(19, 20, 5), 15);
        assert_eq!(context_list_scroll(2, 3, 5), 0);
    }

    #[test]
    fn draw_renders_context_explorer_overlay() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: Some("system prompt".into()),
            skills: vec!["review".into(), "qa".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::Text {
                    role: "user".into(),
                    text: "hello world".into(),
                },
                HistoryEntry::ToolResult {
                    role: "tool".into(),
                    tool_use_id: "call_1".into(),
                    output: "done".into(),
                    is_error: false,
                },
            ],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("Context Explorer")));
        assert!(lines
            .iter()
            .any(|line| line.contains("session session-1 | state idle | 2 entries")));
        assert!(lines.iter().any(|line| line.contains("tools [none]")));
        assert!(lines.iter().any(|line| line.contains("History")));
        assert!(lines.iter().any(|line| line.contains("user: hello world")));
        assert!(lines.iter().any(|line| line.contains("kind: text")));
    }

    #[test]
    fn draw_context_explorer_shows_tool_info() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![quine_llm::ToolDefinition {
                name: "bash".into(),
                description: "Execute a shell command in the workspace.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
                read_only: false,
                idempotent: false,
            }],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::ToolUse {
                    role: "assistant".into(),
                    text: Some("running tool".into()),
                    tool_calls: vec![crate::context_debug::ToolCallEntry {
                        tool_use_id: "call_1".into(),
                        tool_name: "bash".into(),
                        arguments: serde_json::json!({"command": "pwd"}),
                    }],
                },
                HistoryEntry::ToolResult {
                    role: "tool".into(),
                    tool_use_id: "call_1".into(),
                    output: "/tmp/project".into(),
                    is_error: false,
                },
            ],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_move_down();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("tools [bash]")));
        assert!(lines.iter().any(|line| line.contains("tool_name: bash")));
    }

    #[test]
    fn draw_context_explorer_renders_tools_tab() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![quine_llm::ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
                read_only: true,
                idempotent: true,
            }],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_next_tab();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("Tool Detail")));
        assert!(lines.iter().any(|line| line.contains("read_file")));
        assert!(lines.iter().any(|line| line.contains("description:")));
    }

    #[test]
    fn draw_context_explorer_renders_skills_tab() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![crate::context_debug::SkillSnapshot {
                name: "review".into(),
                description: "Review changes".into(),
                version: "1.0".into(),
                system_prompt: Some("Review carefully".into()),
                system_prompt_char_count: 16,
                system_prompt_truncated: false,
                source_path: std::path::PathBuf::from("/tmp/project/.quine/skills/review.md"),
                tool_names: vec!["read_file".into(), "bash".into()],
            }],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_next_tab();
        app.context_explorer_next_tab();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("Skill Detail")));
        assert!(lines.iter().any(|line| line.contains("review")));
        assert!(lines.iter().any(|line| line.contains("Review changes")));
    }

    #[test]
    fn format_skill_detail_marks_truncated_prompt_previews() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["feature-planning".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![crate::context_debug::SkillSnapshot {
                name: "feature-planning".into(),
                description: "Plan features".into(),
                version: "1.0".into(),
                system_prompt: Some("preview body".into()),
                system_prompt_char_count: 2_048,
                system_prompt_truncated: true,
                source_path: std::path::PathBuf::from(
                    "/tmp/project/.claude/commands/feature-planning.md",
                ),
                tool_names: vec![],
            }],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        let explorer = app.context_explorer.as_ref().expect("explorer open");
        let detail = format_skill_detail(explorer);

        assert!(detail.contains("preview body"));
        assert!(detail.contains("prompt_chars: 2048"));
        assert!(detail.contains("prompt_truncated: true"));
        assert!(detail.contains("[preview truncated"));
    }

    #[test]
    fn draw_context_explorer_renders_plans_tab() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![crate::context_debug::PlanSnapshot {
                plan_id: "plan-1".into(),
                title: "Fix explorer".into(),
                actions: vec![crate::context_debug::PlanActionSnapshot {
                    action_id: "patch".into(),
                    title: "Patch renderer".into(),
                    description: "Update the context explorer rendering".into(),
                    depends_on: vec![],
                    status: crate::context_debug::PlanActionStatusSnapshot::InProgress,
                    result: Some("Wiring tabs".into()),
                }],
            }],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);
        app.context_explorer_next_tab();
        app.context_explorer_next_tab();
        app.context_explorer_next_tab();

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("Plans")));
        assert!(lines.iter().any(|line| line.contains("Fix explorer")));
        assert!(lines
            .iter()
            .any(|line| line.contains("patch [in-progress] Patch renderer")));
    }

    #[test]
    fn draw_renders_bash_preview_in_box() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "pwd".into(),
            status: ToolStatus::Success { duration_us: 150 },
            result_preview: Some(
                (0..10)
                    .map(|index| format!("line {index}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
        });

        let backend = ratatui::backend::TestBackend::new(80, 24);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = non_input_conversation_lines(terminal.backend(), &app, 80, 24);
        assert!(lines.iter().any(|line| line.contains("┌")));
        assert!(lines.iter().any(|line| line.contains("line 0")));
        assert!(lines
            .iter()
            .any(|line| line.contains("… output truncated …")));
    }

    #[test]
    fn draw_renders_web_search_preview_in_box() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "web_search".into(),
            tool_use_id: "tc1".into(),
            summary: "mlx rust support".into(),
            status: ToolStatus::Success { duration_us: 150 },
            result_preview: Some(
                "Answer with citations\nSources:\n- Example: https://example.com".into(),
            ),
        });

        let lines = build_conversation_lines(&app, 80);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        assert!(rendered.iter().any(|line| line.contains("web_search")));
        assert!(rendered
            .iter()
            .any(|line| line.contains("Answer with citations")));
        assert!(rendered.iter().any(|line| line.contains("┌")));
    }

    #[test]
    fn draw_grouped_bash_preview_box_stays_aligned_with_wide_glyphs() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "printf demo".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: Some("wide ✅ output\nsecond line".into()),
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "read_file".into(),
            tool_use_id: "tc2".into(),
            summary: "src/main.rs".into(),
            status: ToolStatus::Success { duration_us: 24 },
            result_preview: None,
        });

        let lines = build_conversation_lines(&app, 60);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let top_index = rendered.iter().position(|line| line.contains("┌")).unwrap();
        let bottom_index = rendered.iter().position(|line| line.contains("└")).unwrap();

        assert!(rendered[top_index].starts_with("      ┌"));
        assert!(rendered[bottom_index].starts_with("      └"));
        for line in &rendered[top_index..=bottom_index] {
            assert_eq!(line.width(), rendered[top_index].width());
        }
    }

    #[test]
    fn draw_context_explorer_renders_lineage_and_compact_summary() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-2".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot {
                parent_id: Some("session-1".into()),
                root_id: "session-1".into(),
                depth: 1,
                child_ids: vec!["session-3".into()],
            },
            prompt_memory: None,
            compact_memory_summary_markdown: Some(
                "# Session Summary\n\nCompact body line.\nSecond line.\n".into(),
            ),
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![],
        };
        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines
            .iter()
            .any(|line| line.contains("lineage root session-1")));
        let summary_lines = compact_summary_lines(&app.context_explorer.as_ref().unwrap().snapshot);
        assert!(summary_lines
            .iter()
            .any(|line| line.to_string().contains("session summary")));
        assert!(summary_lines
            .iter()
            .any(|line| line.to_string().contains("Compact body line.")));
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
            status: ToolStatus::Running {
                started_at: Instant::now(),
                timeout: Some(Duration::from_secs(120)),
            },
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
            status: ToolStatus::Running {
                started_at: Instant::now(),
                timeout: Some(Duration::from_secs(120)),
            },
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
    fn draw_renders_single_blank_line_before_tool_entries_too() {
        let mut app = App::new("test".into(), false, None);
        app.messages
            .push(ConversationEntry::AssistantText("hi there".into()));
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo test".into(),
            status: ToolStatus::Running {
                started_at: Instant::now() - Duration::from_secs(3),
                timeout: Some(Duration::from_secs(120)),
            },
            result_preview: None,
        });

        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        let assistant_index = lines
            .iter()
            .position(|line| line.contains("  hi there"))
            .unwrap();
        let tool_index = lines
            .iter()
            .position(|line| line.contains("bash: echo test"))
            .unwrap();

        assert_eq!(tool_index - assistant_index, 2);
        assert!(lines[assistant_index + 1].is_empty());
    }

    #[test]
    fn draw_groups_adjacent_tool_entries_without_blank_lines() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo test".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: None,
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "read_file".into(),
            tool_use_id: "tc2".into(),
            summary: "src/main.rs".into(),
            status: ToolStatus::Success { duration_us: 24 },
            result_preview: None,
        });

        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        let first_tool_index = lines
            .iter()
            .position(|line| line.contains("bash: echo test"))
            .unwrap();
        let second_tool_index = lines
            .iter()
            .position(|line| line.contains("read_file: src/main.rs"))
            .unwrap();

        assert_eq!(second_tool_index - first_tool_index, 1);
    }

    #[test]
    fn draw_groups_tool_call_and_turn_info_without_blank_lines() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "echo test".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: None,
        });
        app.messages.push(ConversationEntry::TurnInfo {
            duration_us: 5_000,
            usage: None,
        });

        let lines = build_conversation_lines(&app, 60);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let tool_index = rendered
            .iter()
            .position(|line| line.contains("bash: echo test"))
            .unwrap();
        let turn_info_index = rendered
            .iter()
            .position(|line| line.contains("──"))
            .unwrap();

        assert_eq!(turn_info_index - tool_index, 1);
        assert!(!rendered[tool_index + 1].is_empty());
    }

    #[test]
    fn draw_groups_tool_call_and_plan_box_without_blank_lines() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "plan".into(),
            tool_use_id: "tc1".into(),
            summary: "create_plan: Demo".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: None,
        });
        app.messages.push(ConversationEntry::PlanBox(
            "Plan: Demo\n🟢 [a1] First task\n✅ [a2] Done task".into(),
        ));

        let lines = build_conversation_lines(&app, 60);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let tool_index = rendered
            .iter()
            .position(|line| line.contains("plan: create_plan: Demo"))
            .unwrap();
        let plan_top_index = rendered.iter().position(|line| line.contains("┌")).unwrap();

        assert_eq!(plan_top_index - tool_index, 1);
        assert!(!rendered[tool_index + 1].is_empty());
    }

    #[test]
    fn draw_skips_blank_line_in_plan_tool_preview() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "plan".into(),
            tool_use_id: "tc1".into(),
            summary: "create_plan: Demo".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: Some(
                "Plan created (ID: 123)\n\nPlan: Branch, commit, and attempt PR flow".into(),
            ),
        });

        let lines = build_conversation_lines(&app, 100);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();
        let created_index = rendered
            .iter()
            .position(|line| line.contains("Plan created (ID: 123)"))
            .unwrap();
        let plan_index = rendered
            .iter()
            .position(|line| line.contains("Plan: Branch, commit, and attempt PR flow"))
            .unwrap();

        assert_eq!(plan_index - created_index, 1);
        assert!(!rendered[created_index + 1].trim().is_empty());
    }

    #[test]
    fn draw_grouped_plan_tool_preview_renders_plan_box() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "plan".into(),
            tool_use_id: "tc1".into(),
            summary: "create_plan: Demo".into(),
            status: ToolStatus::Success { duration_us: 42 },
            result_preview: Some("Plan: Demo\n🟢 [a1] First task\n✅ [a2] Done task".into()),
        });
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "read_file".into(),
            tool_use_id: "tc2".into(),
            summary: "src/main.rs".into(),
            status: ToolStatus::Success { duration_us: 24 },
            result_preview: None,
        });

        let lines = build_conversation_lines(&app, 60);
        let rendered: Vec<String> = lines.iter().map(|line| line.to_string()).collect();

        let header_index = rendered
            .iter()
            .position(|line| line.contains("▌ Tools (2)"))
            .unwrap();
        let plan_box_top_index = rendered.iter().position(|line| line.contains("┌")).unwrap();
        let plan_box_bottom_index = rendered.iter().position(|line| line.contains("└")).unwrap();

        assert!(rendered.iter().any(|line| line.contains("Plan: Demo")));
        assert!(rendered.iter().any(|line| line.contains("[a1] First task")));
        assert_eq!(plan_box_top_index, header_index + 2);
        assert!(rendered[plan_box_top_index].starts_with("      ┌"));
        assert!(rendered[plan_box_bottom_index].starts_with("      └"));
        for line in &rendered[plan_box_top_index..=plan_box_bottom_index] {
            assert_eq!(line.width(), rendered[plan_box_top_index].width());
        }
    }

    #[test]
    fn draw_renders_bash_running_timer_with_elapsed_and_timeout() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "tc1".into(),
            summary: "sleep 10".into(),
            status: ToolStatus::Running {
                started_at: Instant::now() - Duration::from_secs(3),
                timeout: Some(Duration::from_secs(120)),
            },
            result_preview: None,
        });

        let backend = ratatui::backend::TestBackend::new(80, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines
            .iter()
            .any(|line| line.contains("bash: sleep 10") && line.contains("3.0s / 120s")));
    }

    #[test]
    fn draw_renders_plan_box_entry() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::PlanBox(
            "Plan: Demo\n🟢 [a1] First task\n✅ [a2] Done task".into(),
        ));

        let backend = ratatui::backend::TestBackend::new(60, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("┌")));
        assert!(lines.iter().any(|line| line.contains("Plan: Demo")));
        assert!(lines.iter().any(|line| line.contains("[a1] First task")));
        assert!(lines.iter().any(|line| line.contains("└")));
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

    #[test]
    fn draw_auto_follow_keeps_latest_long_assistant_output_visible_with_tall_input() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::User("prompt".into()));
        app.messages.push(ConversationEntry::AssistantText(
            (1..=20)
                .map(|index| format!("line {index:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.input
            .set_from_string("this input is intentionally long enough to wrap twice");

        let terminal_width = 24;
        let terminal_height = 10;
        let backend = ratatui::backend::TestBackend::new(terminal_width, terminal_height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines =
            non_input_conversation_lines(terminal.backend(), &app, terminal_width, terminal_height);

        assert!(lines.iter().any(|line| line.contains("line 20")));
        assert!(lines.iter().all(|line| !line.contains("line 01")));
    }

    #[test]
    fn draw_auto_follow_keeps_final_turn_info_visible() {
        let mut app = App::new("test".into(), false, Some(200_000));
        app.messages.push(ConversationEntry::User("prompt".into()));
        app.messages.push(ConversationEntry::AssistantText(
            (1..=14)
                .map(|index| format!("assistant line {index:02}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.messages.push(ConversationEntry::TurnInfo {
            duration_us: 4_523_000,
            usage: Some(quine_llm::TokenUsage {
                input_tokens: 120_000,
                output_tokens: 30_000,
            }),
        });
        app.input
            .set_from_string("this input is intentionally long enough to wrap twice");

        let terminal_width = 24;
        let terminal_height = 10;
        let backend = ratatui::backend::TestBackend::new(terminal_width, terminal_height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines =
            non_input_conversation_lines(terminal.backend(), &app, terminal_width, terminal_height);

        assert!(lines.iter().any(|line| line.contains("4.5s | ctx 75%")));
    }

    #[test]
    fn draw_places_cursor_at_end_of_wrapped_ascii_input_without_duplicate_text() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("123456789");

        let backend = ratatui::backend::TestBackend::new(10, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("> 123456")));
        assert!(lines.iter().any(|line| line.contains("789")));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("123456789"))
                .count(),
            0
        );
        terminal
            .backend_mut()
            .assert_cursor_position(Position::new(4, 6));
    }

    #[test]
    fn context_detail_repaint_overwrites_old_wrapped_content() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
            status_report: None,
            history: vec![
                HistoryEntry::Text {
                    role: "assistant".into(),
                    text: "this is a very long detail entry that should wrap across multiple lines and then disappear completely after we move the selection to a shorter entry\nthis leftover line must not remain visible".into(),
                },
                HistoryEntry::Text {
                    role: "assistant".into(),
                    text: "short detail".into(),
                },
            ],
        };

        let mut app = App::new("test".into(), false, None);
        app.open_context_explorer(snapshot);

        let backend = ratatui::backend::TestBackend::new(100, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        app.context_explorer_move_down();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("short detail")));
        assert!(!lines
            .iter()
            .any(|line| line.contains("this leftover line must not remain visible")));
    }

    #[test]
    fn draw_interaction_prompt_renders_summary_and_response() {
        let mut app = App::new("test".into(), false, None);
        app.messages.push(ConversationEntry::InteractionPrompt {
            summary: Some("Need confirmation".into()),
            prompt: "Please answer yes or no".into(),
        });

        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines
            .iter()
            .any(|line| line.contains("Summary: Need confirmation")));
        assert!(lines
            .iter()
            .any(|line| line.contains("Response: Please answer yes or no")));
    }

    #[test]
    fn draw_switch_selector_renders_input_and_session_summary() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/switch al");
        app.set_switch_session_candidates(vec![
            crate::tui::app::SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: Some("Alpha summary".into()),
            },
            crate::tui::app::SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
        ]);

        let backend = ratatui::backend::TestBackend::new(100, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("/switch al")));
        assert!(lines.iter().any(|line| line.contains("alpha")));
        assert!(lines.iter().any(|line| line.contains("Alpha summary")));
    }

    #[test]
    fn draw_switch_selector_scrolls_to_keep_selected_option_visible() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/switch s");
        app.set_switch_session_candidates(
            (1..=8)
                .map(|index| crate::tui::app::SwitchSessionCandidate {
                    session_id: format!("session-{index}"),
                    summary: Some(format!("Summary {index}")),
                })
                .collect(),
        );
        for _ in 0..6 {
            app.option_cursor_down();
        }

        let backend = ratatui::backend::TestBackend::new(80, 8);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let lines = buffer_lines(terminal.backend());
        assert!(lines.iter().any(|line| line.contains("session-7")));
        assert!(lines.iter().any(|line| line.contains("Summary 7")));
        assert!(!lines.iter().any(|line| line.contains("session-1")));
    }
}
