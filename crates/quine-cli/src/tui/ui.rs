use std::collections::{BTreeMap, HashMap};

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};
use ratatui::Frame;
use serde_json::to_string_pretty;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::app::{
    AgentPhase, App, ContextExplorerState, ContextExplorerTab, ConversationEntry,
    ConversationRenderCache, InputBuffer, ToolStatus,
};

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
    let current = usage.input_tokens + usage.output_tokens;
    let percent = if max_context_window == 0 {
        0
    } else {
        current.saturating_mul(100) / max_context_window
    };
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
    if let Some(explorer) = app.context_explorer.as_ref() {
        draw_context_explorer(frame, chunks[1], explorer);
    }
}

fn wrapped_rows(width: usize, area_width: u16) -> u16 {
    if area_width == 0 {
        return 1;
    }
    let area_width = usize::from(area_width);
    width.max(1).div_ceil(area_width) as u16
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

fn render_plan_box(lines: &mut Vec<Line<'_>>, plan: &str, width: u16) {
    let inner_width = usize::from(width.saturating_sub(8)).max(1);
    let top = format!("    ┌{}┐", "─".repeat(inner_width + 2));
    let bottom = format!("    └{}┘", "─".repeat(inner_width + 2));
    lines.push(Line::from(Span::styled(
        top,
        Style::default().fg(Color::Cyan),
    )));

    for raw_line in plan.lines() {
        let line = if raw_line.is_empty() { " " } else { raw_line };
        let mut current = String::new();
        let mut current_width = 0usize;

        for ch in line.chars() {
            if current_width >= inner_width {
                let padded = format!("    │ {:<width$} │", current, width = inner_width);
                let style = if current.trim_start().starts_with("Plan:") {
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD)
                } else {
                    plan_status_style(current.trim_start())
                };
                lines.push(Line::from(Span::styled(padded, style)));
                current.clear();
                current_width = 0;
            }
            current.push(ch);
            current_width += 1;
        }

        let padded = format!("    │ {:<width$} │", current, width = inner_width);
        let style = if current.trim_start().starts_with("Plan:") {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            plan_status_style(current.trim_start())
        };
        lines.push(Line::from(Span::styled(padded, style)));
    }

    lines.push(Line::from(Span::styled(
        bottom,
        Style::default().fg(Color::Cyan),
    )));
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
        ConversationEntry::InteractionPrompt(text) => {
            lines.push(Line::from(vec![
                Span::styled(
                    "Response: ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(text.clone()),
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

fn append_live_lines(lines: &mut Vec<Line<'static>>, app: &App) {
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
}

fn build_conversation_lines(app: &App, area_width: u16) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    for (i, entry) in app.messages.iter().enumerate() {
        if i > 0 {
            let is_tool_related = matches!(entry, ConversationEntry::ToolCall { .. });
            if !is_tool_related {
                lines.push(Line::from(""));
            }
        }
        push_conversation_entry_lines(&mut lines, entry, area_width, app.max_context_window);
    }
    append_live_lines(&mut lines, app);
    lines
}

fn conversation_content_height(lines: &[Line<'static>], area_width: u16) -> u32 {
    if area_width == 0 {
        return 0;
    }

    Paragraph::new(Text::from(lines.to_vec()))
        .wrap(Wrap { trim: false })
        .line_count(area_width) as u32
}

fn ensure_conversation_cache(app: &mut App, area_width: u16) -> &ConversationRenderCache {
    let revision = app.conversation_revision();
    let should_rebuild = app
        .conversation_cache
        .as_ref()
        .is_none_or(|cache| cache.width != area_width || cache.revision != revision);

    if should_rebuild {
        let lines = build_conversation_lines(app, area_width);
        let content_height = conversation_content_height(&lines, area_width);
        app.conversation_cache = Some(ConversationRenderCache {
            width: area_width,
            revision,
            lines,
            content_height,
        });
    }

    app.conversation_cache
        .as_ref()
        .expect("conversation cache initialized")
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: ratatui::layout::Rect) {
    let mode = if app.plan_mode { "plan" } else { "chat" };
    let phase = match &app.phase {
        AgentPhase::Idle => "idle".to_string(),
        AgentPhase::Thinking => format!("{} thinking", app.spinner_char()),
        AgentPhase::Streaming => format!("{} streaming", app.spinner_char()),
        AgentPhase::RunningTool(name) => format!("{} tool:{name}", app.spinner_char()),
    };
    let usage = format_context_status(app.last_turn_usage.as_ref(), app.max_context_window);
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
fn draw_conversation(frame: &mut Frame, app: &mut App, area: Rect) {
    frame.render_widget(Clear, area);

    let (content_height, lines) = {
        let cache = ensure_conversation_cache(app, area.width);
        (cache.content_height, cache.lines.clone())
    };
    let view_height = area.height as u32;
    app.last_view_height = view_height;
    let max_scroll = content_height.saturating_sub(view_height);
    let scroll = if app.user_scrolled {
        max_scroll.saturating_sub(app.scroll_offset.min(max_scroll))
    } else {
        max_scroll
    };

    let text = Text::from(lines);
    let conversation = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((scroll.min(u16::MAX as u32) as u16, 0));

    frame.render_widget(conversation, area);
}

/// Compute the number of visual rows a single line occupies when wrapped to `area_width`.
fn format_context_entry_label(index: usize, explorer: &ContextExplorerState) -> String {
    let entry_number = index + 1;
    match explorer.snapshot.history.get(index) {
        Some(crate::context_debug::HistoryEntry::Text { role, text }) => {
            let first_line = text.lines().next().unwrap_or("").trim();
            if first_line.is_empty() {
                format!("{entry_number:>3}. {role}: <blank>")
            } else {
                format!("{entry_number:>3}. {role}: {first_line}")
            }
        }
        Some(crate::context_debug::HistoryEntry::ToolUse {
            role,
            text,
            tool_calls,
        }) => {
            let tool_summary = tool_calls
                .first()
                .map(|call| call.tool_name.as_str())
                .unwrap_or("tool");
            let suffix = text
                .as_deref()
                .and_then(|value| value.lines().next())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("");
            if suffix.is_empty() {
                format!("{entry_number:>3}. {role}: tool {tool_summary}")
            } else {
                format!("{entry_number:>3}. {role}: {suffix}")
            }
        }
        Some(crate::context_debug::HistoryEntry::ToolResult {
            tool_use_id,
            is_error,
            ..
        }) => {
            let status = if *is_error { "error" } else { "ok" };
            format!("{entry_number:>3}. tool result {tool_use_id} ({status})")
        }
        None => format!("{entry_number:>3}. <missing>"),
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

fn format_skill_detail(explorer: &ContextExplorerState) -> String {
    match explorer.selected_skill() {
        Some(skill) => {
            let tool_names = if skill.tool_names.is_empty() {
                "<none>".to_string()
            } else {
                skill.tool_names.join(", ")
            };
            let system_prompt = skill.system_prompt.as_deref().unwrap_or("<none>");
            format!(
                "name: {}\nversion: {}\nsource: {}\n\ndescription:\n{}\n\nsystem_prompt:\n{}\n\ntools:\n{}",
                skill.name,
                skill.version,
                skill.source_path.display(),
                skill.description,
                system_prompt,
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

fn draw_context_explorer(frame: &mut Frame, area: Rect, explorer: &ContextExplorerState) {
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
            Constraint::Length(4),
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
            "skills [{}] | {} | plan_mode {} | auto_approve {}",
            explorer.snapshot.skills.join(", "),
            tool_usage_summary(explorer),
            explorer.snapshot.plan_mode,
            explorer.snapshot.auto_approve_permissions,
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
    ];
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

    match explorer.active_tab {
        ContextExplorerTab::History => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);

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

            let detail = Paragraph::new(format_context_entry_detail(explorer))
                .block(Block::default().title(" Detail ").borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .scroll((explorer.scroll_offset, 0));
            frame.render_widget(detail, columns[1]);
        }
        ContextExplorerTab::Tools => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);

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

            let detail = Paragraph::new(format_tool_detail(explorer))
                .block(
                    Block::default()
                        .title(" Tool Detail ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false })
                .scroll((explorer.scroll_offset, 0));
            frame.render_widget(detail, columns[1]);
        }
        ContextExplorerTab::Skills => {
            let columns = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
                .split(sections[2]);

            frame.render_widget(Clear, columns[0]);
            frame.render_widget(Clear, columns[1]);

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

            let detail = Paragraph::new(format_skill_detail(explorer))
                .block(
                    Block::default()
                        .title(" Skill Detail ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: false })
                .scroll((explorer.scroll_offset, 0));
            frame.render_widget(detail, columns[1]);
        }
        ContextExplorerTab::Plans => {
            frame.render_widget(Clear, sections[2]);
            let plans = Paragraph::new(Text::from(format_plans_tab_lines(explorer)))
                .block(Block::default().title(" Plans ").borders(Borders::ALL))
                .wrap(Wrap { trim: false })
                .scroll((explorer.scroll_offset, 0));
            frame.render_widget(plans, sections[2]);
        }
    }

    let footer = Paragraph::new("Esc close • ←→/h l tabs • ↑↓/j k navigate • PgUp/PgDn scroll")
        .alignment(Alignment::Center)
        .style(
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::DIM),
        );
    frame.render_widget(Clear, sections[3]);
    frame.render_widget(footer, sections[3]);
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
    let input_widget = Paragraph::new(Text::from(wrap_input_lines(
        &app.input,
        &label,
        area.width.saturating_sub(2),
    )))
    .block(Block::default().borders(Borders::ALL));

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
    fn draw_context_explorer_marks_selected_entry() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            auto_approve_permissions: true,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
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
    fn draw_context_explorer_keeps_selected_marker_after_detail_scroll() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec![],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            auto_approve_permissions: true,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
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
    fn closing_context_explorer_clears_overlay_content() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: Some("system prompt".into()),
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            auto_approve_permissions: true,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
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
            auto_approve_permissions: true,
            available_tools: vec![],
            loaded_skills: vec![],
            plans: vec![],
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
            auto_approve_permissions: true,
            available_tools: vec![quine_llm::ToolDefinition {
                name: "bash".into(),
                description: "Execute a shell command in the workspace.".into(),
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
                read_only: false,
                idempotent: false,
            }],
            loaded_skills: vec![],
            plans: vec![],
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
            auto_approve_permissions: true,
            available_tools: vec![quine_llm::ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: serde_json::json!({"type": "object"}),
                read_only: true,
                idempotent: true,
            }],
            loaded_skills: vec![],
            plans: vec![],
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
            auto_approve_permissions: true,
            available_tools: vec![],
            loaded_skills: vec![crate::context_debug::SkillSnapshot {
                name: "review".into(),
                description: "Review changes".into(),
                version: "1.0".into(),
                system_prompt: Some("Review carefully".into()),
                source_path: std::path::PathBuf::from("/tmp/project/.quine/skills/review.md"),
                tool_names: vec!["read_file".into(), "bash".into()],
            }],
            plans: vec![],
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
    fn draw_context_explorer_renders_plans_tab() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: chrono::Utc::now(),
            state: "idle".into(),
            system_prompt: None,
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            auto_approve_permissions: true,
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
    fn input_cursor_position_uses_display_width_for_wide_wrap_boundary() {
        let mut input = InputBuffer::new();
        input.set_from_string("1234567界");

        assert_eq!(input_cursor_position(&input, "> ", 10), (1, 1));
        assert_eq!(input_content_rows(&input, "> ", 10), 2);
    }
}
