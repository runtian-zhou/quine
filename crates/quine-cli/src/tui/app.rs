use std::collections::{HashSet, VecDeque};
use std::fmt;

use quine_harness::protocol::{notifications, JsonRpcNotification};

/// Spinner braille frames for the waiting animation.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// A single line in a diff display.
#[derive(Debug, Clone)]
pub enum DiffLine {
    /// Added line (shown in green with + prefix).
    Add(String),
    /// Removed line (shown in red with - prefix).
    #[allow(dead_code)]
    Remove(String),
    /// Header line (shown in bold/cyan).
    Header(String),
}

/// A single entry in the conversation view.
#[derive(Debug, Clone)]
pub enum ConversationEntry {
    User(String),
    AssistantText(String),
    ToolCall {
        tool_name: String,
        summary: String,
    },
    /// Write tool diff preview.
    WriteDiff {
        #[allow(dead_code)]
        file_path: String,
        diff_lines: Vec<DiffLine>,
    },
    Error(String),
    InteractionPrompt(String),
}

/// Current phase of the agent turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentPhase {
    Idle,
    Thinking,
    Streaming,
    RunningTool(String),
}

/// The kind of a pending interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionKind {
    /// Free-form question from ask_user tool.
    AskUser,
    /// Permission confirmation request.
    Permission,
    /// Single-select from options.
    SingleSelect,
    /// Multi-select from options.
    MultiSelect,
}

/// A queued interaction request from the daemon.
#[derive(Debug, Clone)]
pub struct PendingInteraction {
    /// The prompt text (shown in conversation view, kept here for reference).
    #[allow(dead_code)]
    pub prompt: String,
    pub kind: InteractionKind,
    /// Option labels for select interactions.
    #[allow(dead_code)]
    pub options: Vec<String>,
    /// Whether free-form "Other" input is allowed.
    #[allow(dead_code)]
    pub allow_freeform: bool,
}

/// State for option-based selection in the TUI.
pub struct OptionSelectState {
    /// Option labels.
    pub options: Vec<String>,
    /// Currently highlighted option index.
    pub cursor: usize,
    /// Selected indices (for MultiSelect).
    pub selected: HashSet<usize>,
    /// Whether multi-select is enabled.
    pub multi_select: bool,
    /// Whether freeform input is allowed.
    #[allow(dead_code)]
    pub allow_freeform: bool,
}

/// Actions the event loop should perform after handling an event.
pub enum AppAction {
    SendMessage(String),
    SubmitInteraction(String),
    Quit,
}

/// Multi-line input buffer with cursor tracking.
pub struct InputBuffer {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
        }
    }

    /// Insert a character at the current cursor position (UTF-8 safe).
    pub fn insert_char(&mut self, c: char) {
        let line = &mut self.lines[self.row];
        // Find the byte offset for self.col characters into the line.
        let byte_offset = line
            .char_indices()
            .nth(self.col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        line.insert(byte_offset, c);
        self.col += 1;
    }

    /// Insert a newline, splitting the current line at the cursor column.
    pub fn insert_newline(&mut self) {
        let line = &self.lines[self.row];
        let byte_offset = line
            .char_indices()
            .nth(self.col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        let remainder = self.lines[self.row][byte_offset..].to_string();
        self.lines[self.row].truncate(byte_offset);
        self.row += 1;
        self.lines.insert(self.row, remainder);
        self.col = 0;
    }

    /// Delete the character before the cursor (backspace).
    /// If at column 0, join with the previous line.
    pub fn delete_char_before(&mut self) {
        if self.col > 0 {
            let line = &mut self.lines[self.row];
            // Find byte offset of the char before col.
            let byte_start = line
                .char_indices()
                .nth(self.col - 1)
                .map(|(i, _)| i)
                .unwrap_or(0);
            let byte_end = line
                .char_indices()
                .nth(self.col)
                .map(|(i, _)| i)
                .unwrap_or(line.len());
            line.drain(byte_start..byte_end);
            self.col -= 1;
        } else if self.row > 0 {
            // Join current line with previous line.
            let current = self.lines.remove(self.row);
            self.row -= 1;
            let prev_char_count = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&current);
            self.col = prev_char_count;
        }
    }

    /// Move cursor left. Wraps to end of previous line.
    pub fn cursor_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].chars().count();
        }
    }

    /// Move cursor right. Wraps to start of next line.
    pub fn cursor_right(&mut self) {
        let line_char_count = self.lines[self.row].chars().count();
        if self.col < line_char_count {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Move cursor up one line, clamping column.
    pub fn cursor_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            let line_char_count = self.lines[self.row].chars().count();
            self.col = self.col.min(line_char_count);
        }
    }

    /// Move cursor down one line, clamping column.
    pub fn cursor_down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            let line_char_count = self.lines[self.row].chars().count();
            self.col = self.col.min(line_char_count);
        }
    }

    /// Check if the buffer is empty (single empty line).
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    /// Clear the buffer to a single empty line.
    pub fn clear(&mut self) {
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
    }

    /// Set buffer contents from a string (for history restore). Cursor goes to end.
    pub fn set_from_string(&mut self, s: &str) {
        self.lines = s.split('\n').map(|l| l.to_string()).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
    }

    /// Number of lines in the buffer.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// Whether the buffer contains multiple lines.
    pub fn is_multiline(&self) -> bool {
        self.lines.len() > 1
    }

    /// Current cursor row.
    pub fn row(&self) -> usize {
        self.row
    }

    /// Current cursor column (in characters, not bytes).
    pub fn col(&self) -> usize {
        self.col
    }

    /// Reference to the current line.
    #[allow(dead_code)]
    pub fn current_line(&self) -> &str {
        &self.lines[self.row]
    }

    /// Get the full content as a single string (lines joined by newlines).
    pub fn content(&self) -> String {
        self.lines.join("\n")
    }
}

impl fmt::Display for InputBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.lines.join("\n"))
    }
}

/// The main application state.
pub struct App {
    pub messages: Vec<ConversationEntry>,
    pub streaming_buffer: String,
    pub scroll_offset: u16,
    pub user_scrolled: bool,
    pub input: InputBuffer,
    pub interaction_queue: VecDeque<PendingInteraction>,
    pub phase: AgentPhase,
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub session_id: String,
    pub expanded_tools: HashSet<usize>,
    /// History of submitted inputs (oldest first).
    pub input_history: Vec<String>,
    /// Current position in history (None = not browsing history).
    history_index: Option<usize>,
    /// Saved in-progress input when entering history mode.
    saved_input: String,
    /// Active option selector state (for SingleSelect/MultiSelect interactions).
    pub option_select: Option<OptionSelectState>,
}

impl App {
    pub fn new(session_id: String) -> Self {
        Self {
            messages: Vec::new(),
            streaming_buffer: String::new(),
            scroll_offset: 0,
            user_scrolled: false,
            input: InputBuffer::new(),
            interaction_queue: VecDeque::new(),
            phase: AgentPhase::Idle,
            spinner_frame: 0,
            should_quit: false,
            session_id,
            expanded_tools: HashSet::new(),
            input_history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
            option_select: None,
        }
    }

    /// Advance the spinner animation frame.
    pub fn tick_spinner(&mut self) {
        if self.phase != AgentPhase::Idle {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
        }
    }

    /// Get the current spinner character.
    pub fn spinner_char(&self) -> char {
        SPINNER_FRAMES[self.spinner_frame]
    }

    /// Get the label for the input box.
    ///
    /// Permission prompts show a short label here — the full prompt is
    /// rendered in the conversation view instead.
    pub fn input_label(&self) -> String {
        if let Some(interaction) = self.interaction_queue.front() {
            let pending = self.interaction_queue.len();
            let badge = if pending > 1 {
                format!(" [{pending} pending]")
            } else {
                String::new()
            };
            match interaction.kind {
                InteractionKind::Permission => {
                    format!("[permission]{badge} (y/n) > ")
                }
                InteractionKind::AskUser => {
                    format!("[ask_user]{badge} > ")
                }
                InteractionKind::SingleSelect => {
                    format!("[select]{badge} [↑↓] Enter > ")
                }
                InteractionKind::MultiSelect => {
                    format!("[multi-select]{badge} [↑↓] Space/Enter > ")
                }
            }
        } else {
            "> ".to_string()
        }
    }

    /// Handle Enter/Ctrl+S: send message or submit interaction response.
    pub fn submit_input(&mut self) -> Option<AppAction> {
        // If in option-select mode, submit the selection.
        if let Some(select) = self.option_select.take() {
            let response = if select.multi_select {
                let labels: Vec<String> = select
                    .selected
                    .iter()
                    .copied()
                    .filter(|&i| i < select.options.len())
                    .map(|i| select.options[i].clone())
                    .collect();
                labels.join(", ")
            } else {
                select
                    .options
                    .get(select.cursor)
                    .cloned()
                    .unwrap_or_default()
            };
            self.interaction_queue.pop_front();
            self.messages
                .push(ConversationEntry::InteractionPrompt(response.clone()));
            self.auto_scroll();
            return Some(AppAction::SubmitInteraction(response));
        }

        let text = self.input.content().trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.history_index = None;
        self.saved_input.clear();

        if let Some(interaction) = self.interaction_queue.pop_front() {
            // Expand y/n shorthand for permission prompts.
            let response = if interaction.kind == InteractionKind::Permission {
                match text.to_lowercase().as_str() {
                    "y" | "yes" => "approved".to_string(),
                    "n" | "no" => "denied".to_string(),
                    other => other.to_string(),
                }
            } else {
                text.clone()
            };
            self.messages
                .push(ConversationEntry::InteractionPrompt(text));
            self.auto_scroll();
            Some(AppAction::SubmitInteraction(response))
        } else {
            // Normal user message — push to history.
            self.input_history.push(text.clone());
            self.messages.push(ConversationEntry::User(text.clone()));
            self.phase = AgentPhase::Thinking;
            self.auto_scroll();
            Some(AppAction::SendMessage(text))
        }
    }

    /// Move option selector cursor up.
    pub fn option_cursor_up(&mut self) {
        if let Some(ref mut select) = self.option_select {
            if select.cursor > 0 {
                select.cursor -= 1;
            }
        }
    }

    /// Move option selector cursor down.
    pub fn option_cursor_down(&mut self) {
        if let Some(ref mut select) = self.option_select {
            if select.cursor + 1 < select.options.len() {
                select.cursor += 1;
            }
        }
    }

    /// Toggle selection on current option (for MultiSelect).
    pub fn option_toggle(&mut self) {
        if let Some(ref mut select) = self.option_select {
            if select.multi_select {
                let idx = select.cursor;
                if select.selected.contains(&idx) {
                    select.selected.remove(&idx);
                } else {
                    select.selected.insert(idx);
                }
            }
        }
    }

    /// Check if we're in option selection mode.
    pub fn is_selecting_options(&self) -> bool {
        self.option_select.is_some()
    }

    /// Check if there is a pending interaction in the queue.
    pub fn has_pending_interaction(&self) -> bool {
        !self.interaction_queue.is_empty()
    }

    /// Navigate to the previous input in history (Up arrow).
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // Enter history mode — save current input.
                self.saved_input = self.input.content();
                let idx = self.input_history.len() - 1;
                self.history_index = Some(idx);
                self.input.set_from_string(&self.input_history[idx].clone());
            }
            Some(idx) if idx > 0 => {
                let idx = idx - 1;
                self.history_index = Some(idx);
                self.input.set_from_string(&self.input_history[idx].clone());
            }
            _ => {} // Already at oldest entry.
        }
    }

    /// Navigate to the next input in history (Down arrow).
    pub fn history_next(&mut self) {
        if let Some(idx) = self.history_index {
            if idx + 1 < self.input_history.len() {
                let idx = idx + 1;
                self.history_index = Some(idx);
                self.input.set_from_string(&self.input_history[idx].clone());
            } else {
                // Past the end — restore saved input.
                self.history_index = None;
                let saved = std::mem::take(&mut self.saved_input);
                self.input.set_from_string(&saved);
            }
        }
    }

    /// Apply a daemon notification to the app state.
    pub fn apply_notification(&mut self, notif: &JsonRpcNotification) {
        match notif.method.as_str() {
            notifications::STREAM_DELTA => {
                self.phase = AgentPhase::Streaming;
                if let Some(delta) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("delta"))
                    .and_then(|v| v.as_str())
                {
                    self.streaming_buffer.push_str(delta);
                }
                self.auto_scroll();
            }
            notifications::TEXT_COMPLETE => {
                let text = if let Some(full_text) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("full_text"))
                    .and_then(|v| v.as_str())
                {
                    full_text.to_string()
                } else {
                    std::mem::take(&mut self.streaming_buffer)
                };
                if !text.is_empty() {
                    self.messages.push(ConversationEntry::AssistantText(text));
                }
                self.streaming_buffer.clear();
                self.auto_scroll();
            }
            notifications::TOOL_REQUEST => {
                if let Some(params) = &notif.params {
                    let tool_name = params
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let summary = params
                        .get("arguments")
                        .and_then(|v| {
                            // Try to extract a short summary from arguments.
                            v.get("command")
                                .or(v.get("file_path"))
                                .or(v.get("question"))
                                .and_then(|s| s.as_str())
                        })
                        .unwrap_or("")
                        .to_string();
                    let summary = if summary.len() > 60 {
                        format!("{}…", &summary[..59])
                    } else {
                        summary
                    };
                    self.phase = AgentPhase::RunningTool(tool_name.clone());

                    // For write tool, show content preview as diff.
                    if tool_name == "write" {
                        if let Some(args) = params.get("arguments") {
                            let file_path = args
                                .get("file_path")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown")
                                .to_string();
                            let content =
                                args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                            let mut diff_lines =
                                vec![DiffLine::Header(format!("write: {file_path}"))];
                            for line in content.lines() {
                                diff_lines.push(DiffLine::Add(line.to_string()));
                            }
                            self.messages.push(ConversationEntry::WriteDiff {
                                file_path,
                                diff_lines,
                            });
                        } else {
                            self.messages
                                .push(ConversationEntry::ToolCall { tool_name, summary });
                        }
                    } else {
                        self.messages
                            .push(ConversationEntry::ToolCall { tool_name, summary });
                    }
                    self.auto_scroll();
                }
            }
            notifications::TURN_COMPLETE => {
                // Flush any remaining streaming buffer.
                if !self.streaming_buffer.is_empty() {
                    let text = std::mem::take(&mut self.streaming_buffer);
                    self.messages.push(ConversationEntry::AssistantText(text));
                }
                self.phase = AgentPhase::Idle;
                self.auto_scroll();
            }
            notifications::SESSION_ERROR => {
                if let Some(error) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("error"))
                    .and_then(|v| v.as_str())
                {
                    self.messages
                        .push(ConversationEntry::Error(error.to_string()));
                }
                self.phase = AgentPhase::Idle;
                self.auto_scroll();
            }
            notifications::INTERACTION_NEEDED => {
                let prompt = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("prompt"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("(interaction requested)")
                    .to_string();
                let kind_str = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("kind"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let options: Vec<String> = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("options"))
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                item.get("label")
                                    .and_then(|l| l.as_str())
                                    .map(|s| s.to_string())
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let allow_freeform = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("allow_freeform"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let kind = match kind_str {
                    "Confirmation" => InteractionKind::Permission,
                    "SingleSelect" => InteractionKind::SingleSelect,
                    "MultiSelect" => InteractionKind::MultiSelect,
                    _ => InteractionKind::AskUser,
                };
                // Show the prompt in the conversation view.
                let label = match kind {
                    InteractionKind::Permission => format!("⚠ Permission: {prompt}"),
                    InteractionKind::AskUser => format!("❓ {prompt}"),
                    InteractionKind::SingleSelect | InteractionKind::MultiSelect => {
                        let opt_list: Vec<String> = options
                            .iter()
                            .enumerate()
                            .map(|(i, o)| format!("  {}. {o}", i + 1))
                            .collect();
                        format!("❓ {prompt}\n{}", opt_list.join("\n"))
                    }
                };
                self.messages.push(ConversationEntry::Error(label));
                self.auto_scroll();

                let is_select = matches!(
                    kind,
                    InteractionKind::SingleSelect | InteractionKind::MultiSelect
                );
                self.interaction_queue.push_back(PendingInteraction {
                    prompt,
                    kind: kind.clone(),
                    options: options.clone(),
                    allow_freeform,
                });

                // If this is the first interaction and it has options, enter select mode.
                if is_select && self.interaction_queue.len() == 1 {
                    self.option_select = Some(OptionSelectState {
                        options,
                        cursor: 0,
                        selected: HashSet::new(),
                        multi_select: kind == InteractionKind::MultiSelect,
                        allow_freeform,
                    });
                }
            }
            _ => {}
        }
    }

    pub fn scroll_up(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_add(amount);
        self.user_scrolled = true;
    }

    pub fn scroll_down(&mut self, amount: u16) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
        if self.scroll_offset == 0 {
            self.user_scrolled = false;
        }
    }

    #[allow(dead_code)]
    pub fn toggle_tool_expand(&mut self, index: usize) {
        if self.expanded_tools.contains(&index) {
            self.expanded_tools.remove(&index);
        } else {
            self.expanded_tools.insert(index);
        }
    }

    fn auto_scroll(&mut self) {
        if !self.user_scrolled {
            self.scroll_offset = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_notif(method: &str, params: Option<serde_json::Value>) -> JsonRpcNotification {
        JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn submit_input_sends_message_when_no_interaction() {
        let mut app = App::new("test-session".into());
        app.input.set_from_string("hello");

        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SendMessage(msg)) if msg == "hello"));
        assert!(app.input.is_empty());
        assert_eq!(app.messages.len(), 1);
        assert!(matches!(&app.messages[0], ConversationEntry::User(s) if s == "hello"));
        assert_eq!(app.phase, AgentPhase::Thinking);
    }

    #[test]
    fn submit_input_responds_to_interaction() {
        let mut app = App::new("test-session".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "What is your name?".into(),
            kind: InteractionKind::AskUser,
            options: Vec::new(),
            allow_freeform: false,
        });
        app.input.set_from_string("Alice");

        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "Alice"));
        assert!(app.interaction_queue.is_empty());
    }

    #[test]
    fn submit_empty_input_does_nothing() {
        let mut app = App::new("test-session".into());
        app.input.set_from_string("   ");
        assert!(app.submit_input().is_none());
    }

    #[test]
    fn apply_stream_delta() {
        let mut app = App::new("s".into());
        let notif = make_notif(
            notifications::STREAM_DELTA,
            Some(serde_json::json!({"delta": "hello"})),
        );
        app.apply_notification(&notif);
        assert_eq!(app.streaming_buffer, "hello");
        assert_eq!(app.phase, AgentPhase::Streaming);
    }

    #[test]
    fn apply_text_complete_flushes_buffer() {
        let mut app = App::new("s".into());
        app.streaming_buffer = "hello world".into();
        let notif = make_notif(notifications::TEXT_COMPLETE, Some(serde_json::json!({})));
        app.apply_notification(&notif);
        assert!(app.streaming_buffer.is_empty());
        assert_eq!(app.messages.len(), 1);
        assert!(
            matches!(&app.messages[0], ConversationEntry::AssistantText(t) if t == "hello world")
        );
    }

    #[test]
    fn apply_tool_request() {
        let mut app = App::new("s".into());
        let notif = make_notif(
            notifications::TOOL_REQUEST,
            Some(serde_json::json!({
                "tool_name": "bash",
                "tool_use_id": "tc1",
                "arguments": {"command": "echo hello"}
            })),
        );
        app.apply_notification(&notif);
        assert!(matches!(app.phase, AgentPhase::RunningTool(ref name) if name == "bash"));
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn apply_turn_complete() {
        let mut app = App::new("s".into());
        app.phase = AgentPhase::Streaming;
        app.streaming_buffer = "leftover".into();
        let notif = make_notif(notifications::TURN_COMPLETE, Some(serde_json::json!({})));
        app.apply_notification(&notif);
        assert_eq!(app.phase, AgentPhase::Idle);
        assert!(app.streaming_buffer.is_empty());
        assert_eq!(app.messages.len(), 1);
    }

    #[test]
    fn apply_interaction_needed_queues() {
        let mut app = App::new("s".into());
        let notif = make_notif(
            notifications::INTERACTION_NEEDED,
            Some(serde_json::json!({"prompt": "Continue?"})),
        );
        app.apply_notification(&notif);
        assert_eq!(app.interaction_queue.len(), 1);
        assert_eq!(app.interaction_queue[0].prompt, "Continue?");
    }

    #[test]
    fn input_label_normal() {
        let app = App::new("s".into());
        assert_eq!(app.input_label(), "> ");
    }

    #[test]
    fn input_label_with_interaction() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Name?".into(),
            kind: InteractionKind::AskUser,
            options: Vec::new(),
            allow_freeform: false,
        });
        assert_eq!(app.input_label(), "[ask_user] > ");
    }

    #[test]
    fn input_label_with_multiple_interactions() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Q1?".into(),
            kind: InteractionKind::AskUser,
            options: Vec::new(),
            allow_freeform: false,
        });
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Q2?".into(),
            kind: InteractionKind::AskUser,
            options: Vec::new(),
            allow_freeform: false,
        });
        assert!(app.input_label().contains("[2 pending]"));
    }

    #[test]
    fn spinner_cycles() {
        let mut app = App::new("s".into());
        app.phase = AgentPhase::Thinking;
        let first = app.spinner_char();
        app.tick_spinner();
        let second = app.spinner_char();
        assert_ne!(first, second);
    }

    #[test]
    fn scroll_up_down() {
        let mut app = App::new("s".into());
        app.scroll_up(5);
        assert_eq!(app.scroll_offset, 5);
        assert!(app.user_scrolled);
        app.scroll_down(3);
        assert_eq!(app.scroll_offset, 2);
        assert!(app.user_scrolled);
        app.scroll_down(10);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.user_scrolled);
    }

    #[test]
    fn insert_and_delete_chars() {
        let mut app = App::new("s".into());
        app.input.insert_char('h');
        app.input.insert_char('i');
        assert_eq!(app.input.content(), "hi");
        assert_eq!(app.input.col(), 2);
        app.input.delete_char_before();
        assert_eq!(app.input.content(), "h");
        assert_eq!(app.input.col(), 1);
    }

    #[test]
    fn history_navigation() {
        let mut app = App::new("s".into());
        app.input_history.push("first".into());
        app.input_history.push("second".into());
        app.input.set_from_string("current");

        // Up -> most recent history entry.
        app.history_prev();
        assert_eq!(app.input.content(), "second");
        // Up again -> older entry.
        app.history_prev();
        assert_eq!(app.input.content(), "first");
        // Up at the top -> stays.
        app.history_prev();
        assert_eq!(app.input.content(), "first");
        // Down -> back to second.
        app.history_next();
        assert_eq!(app.input.content(), "second");
        // Down -> restores saved input.
        app.history_next();
        assert_eq!(app.input.content(), "current");
        // Down again -> no-op.
        app.history_next();
        assert_eq!(app.input.content(), "current");
    }

    #[test]
    fn history_empty_does_nothing() {
        let mut app = App::new("s".into());
        app.input.set_from_string("hello");
        app.history_prev();
        assert_eq!(app.input.content(), "hello");
    }

    #[test]
    fn submit_pushes_to_history() {
        let mut app = App::new("s".into());
        app.input.set_from_string("hello");
        app.submit_input();
        assert_eq!(app.input_history, vec!["hello"]);
    }

    #[test]
    fn permission_label() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow bash: rm -rf /tmp/foo?".into(),
            kind: InteractionKind::Permission,
            options: Vec::new(),
            allow_freeform: false,
        });
        let label = app.input_label();
        assert!(label.contains("[permission]"));
        assert!(label.contains("(y/n)"));
    }

    #[test]
    fn permission_y_shorthand() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow?".into(),
            kind: InteractionKind::Permission,
            options: Vec::new(),
            allow_freeform: false,
        });
        app.input.set_from_string("y");
        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "approved"));
    }

    #[test]
    fn permission_n_shorthand() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow?".into(),
            kind: InteractionKind::Permission,
            options: Vec::new(),
            allow_freeform: false,
        });
        app.input.set_from_string("n");
        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "denied"));
    }

    // --- New InputBuffer tests ---

    #[test]
    fn input_buffer_insert_newline() {
        let mut buf = InputBuffer::new();
        buf.insert_char('a');
        buf.insert_char('b');
        buf.insert_newline();
        buf.insert_char('c');
        assert_eq!(buf.content(), "ab\nc");
        assert_eq!(buf.line_count(), 2);
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 1);
    }

    #[test]
    fn input_buffer_backspace_at_line_start() {
        let mut buf = InputBuffer::new();
        buf.insert_char('a');
        buf.insert_newline();
        buf.insert_char('b');
        assert_eq!(buf.content(), "a\nb");
        // Move to start of line 1 and backspace -> joins lines.
        buf.cursor_left(); // col=0
        buf.delete_char_before();
        assert_eq!(buf.content(), "ab");
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.row(), 0);
        assert_eq!(buf.col(), 1); // cursor at end of "a"
    }

    #[test]
    fn input_buffer_cursor_up_down() {
        let mut buf = InputBuffer::new();
        buf.set_from_string("hello\nworld\nfoo");
        // Cursor at end of "foo" (row=2, col=3).
        assert_eq!(buf.row(), 2);
        assert_eq!(buf.col(), 3);

        buf.cursor_up();
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 3); // clamped to min(3, 5)

        buf.cursor_up();
        assert_eq!(buf.row(), 0);
        assert_eq!(buf.col(), 3);

        // Up at top -> no-op.
        buf.cursor_up();
        assert_eq!(buf.row(), 0);

        buf.cursor_down();
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 3);

        buf.cursor_down();
        assert_eq!(buf.row(), 2);
        assert_eq!(buf.col(), 3);

        // Down at bottom -> no-op.
        buf.cursor_down();
        assert_eq!(buf.row(), 2);
    }

    #[test]
    fn input_buffer_to_string() {
        let mut buf = InputBuffer::new();
        buf.set_from_string("line1\nline2\nline3");
        assert_eq!(buf.to_string(), "line1\nline2\nline3");
        assert_eq!(buf.content(), "line1\nline2\nline3");
        assert!(buf.is_multiline());
        assert!(!buf.is_empty());
    }

    #[test]
    fn input_buffer_cursor_wrap_left_right() {
        let mut buf = InputBuffer::new();
        buf.set_from_string("ab\ncd");
        // Cursor at end of "cd" (row=1, col=2).
        // Go to start of line 1.
        buf.cursor_left();
        buf.cursor_left();
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 0);
        // Left wraps to end of prev line.
        buf.cursor_left();
        assert_eq!(buf.row(), 0);
        assert_eq!(buf.col(), 2);
        // Right wraps to start of next line.
        buf.cursor_right();
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 0);
    }

    #[test]
    fn input_buffer_insert_newline_mid_line() {
        let mut buf = InputBuffer::new();
        buf.set_from_string("abcd");
        // Cursor at end (col=4). Move left twice to col=2.
        buf.cursor_left();
        buf.cursor_left();
        assert_eq!(buf.col(), 2);
        buf.insert_newline();
        assert_eq!(buf.content(), "ab\ncd");
        assert_eq!(buf.row(), 1);
        assert_eq!(buf.col(), 0);
    }

    #[test]
    fn input_buffer_clear() {
        let mut buf = InputBuffer::new();
        buf.set_from_string("hello\nworld");
        buf.clear();
        assert!(buf.is_empty());
        assert_eq!(buf.line_count(), 1);
        assert_eq!(buf.row(), 0);
        assert_eq!(buf.col(), 0);
    }

    #[test]
    fn input_buffer_utf8_safe() {
        let mut buf = InputBuffer::new();
        buf.insert_char('é');
        buf.insert_char('ñ');
        assert_eq!(buf.content(), "éñ");
        assert_eq!(buf.col(), 2);
        buf.cursor_left();
        assert_eq!(buf.col(), 1);
        buf.delete_char_before();
        assert_eq!(buf.content(), "ñ");
        assert_eq!(buf.col(), 0);
    }
}
