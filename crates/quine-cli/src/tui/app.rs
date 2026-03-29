use std::collections::{HashSet, VecDeque};
use std::fmt;

use std::time::Duration;

use crate::slash_command::{parse_slash_command, SlashCommand};
use quine_harness::protocol::{notifications, JsonRpcNotification};

/// Spinner braille frames for the waiting animation.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Strip leading and trailing blank lines from text while preserving internal blank lines.
fn trim_blank_lines(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    lines[start..end].join("\n")
}

fn summarize_tool_call(tool_name: &str, arguments: &serde_json::Value) -> String {
    match tool_name {
        "plan" => {
            let operation = arguments
                .get("operation")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            match operation {
                "create_plan" => arguments
                    .get("title")
                    .and_then(|v| v.as_str())
                    .map(|title| format!("create_plan: {title}"))
                    .unwrap_or_else(|| "create_plan".to_string()),
                "update_plan" => {
                    let action_id = arguments
                        .get("action_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    let status = arguments
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?");
                    format!("update_plan: {action_id} -> {status}")
                }
                _ => operation.to_string(),
            }
        }
        _ => arguments
            .get("command")
            .or(arguments.get("file_path"))
            .or(arguments.get("question"))
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string(),
    }
}

fn build_apply_patch_preview(arguments: &serde_json::Value) -> Option<String> {
    let file_path = arguments.get("file_path")?.as_str()?;
    let mut lines = vec![format!("apply_patch: {file_path}")];

    if let Some(content) = arguments.get("new_file_content").and_then(|v| v.as_str()) {
        lines.push("--- new file ---".to_string());
        lines.extend(content.lines().map(|line| format!("+ {line}")));
        return Some(lines.join("\n"));
    }

    let edits = arguments.get("edits")?.as_array()?;
    for (index, edit) in edits.iter().enumerate() {
        if index > 0 {
            lines.push("---".to_string());
        }
        if edit
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            lines.push("(replace_all)".to_string());
        }
        let old_text = edit
            .get("old_text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let new_text = edit
            .get("new_text")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        lines.extend(old_text.lines().map(|line| format!("- {line}")));
        lines.extend(new_text.lines().map(|line| format!("+ {line}")));
    }

    Some(lines.join("\n"))
}

/// Status of a tool call execution.
#[derive(Debug, Clone)]
pub enum ToolStatus {
    Running,
    Success { duration_us: u64 },
    Error { duration_us: u64 },
}

/// A single entry in the conversation view.
#[derive(Debug, Clone)]
pub enum ConversationEntry {
    User(String),
    AssistantText(String),
    ToolCall {
        tool_name: String,
        tool_use_id: String,
        summary: String,
        status: ToolStatus,
        result_preview: Option<String>,
    },
    PatchPreview(String),
    PlanBox(String),
    PlanProgress {
        action_id: String,
        status: String,
        remaining: usize,
        total: usize,
    },
    Error(String),
    /// An interaction prompt from the agent (ask_user, permission, etc.)
    /// Contains the prompt text and optional numbered options.
    InteractionQuestion {
        prompt: String,
        options: Vec<String>,
    },
    InteractionPrompt(String),
    /// Turn summary with timing and token usage.
    TurnInfo {
        duration_us: u64,
        usage: Option<quine_llm::TokenUsage>,
    },
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
    /// Label identifying the source of this interaction (e.g. "subagent: <task>").
    pub source_label: Option<String>,
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
    pub allow_freeform: bool,
}

/// Actions the event loop should perform after handling an event.
pub enum AppAction {
    SendMessage(String),
    SendSlashSkillMessage {
        skill_name: String,
        request: String,
    },
    ScheduleLoop {
        request: String,
        delay: Duration,
        cadence: Option<Duration>,
    },
    EnterPlanMode {
        request: String,
        was_plan_mode: bool,
    },
    ExitPlanMode {
        final_plan: String,
    },
    SubmitInteraction(String),
    Cancel,
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
        let byte_offset = line
            .char_indices()
            .nth(self.col)
            .map(|(i, _)| i)
            .unwrap_or(line.len());
        line.insert(byte_offset, c);
        self.col += 1;
    }

    /// Delete the character before the cursor (backspace).
    /// If at column 0, join with the previous line.
    pub fn delete_char_before(&mut self) {
        if self.col > 0 {
            let line = &mut self.lines[self.row];
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

    /// Reference to a specific line.
    pub fn line(&self, row: usize) -> &str {
        &self.lines[row]
    }

    /// Display width of the current line prefix up to `col` characters.
    pub fn line_prefix_width(&self, row: usize, col: usize) -> usize {
        self.lines[row].chars().take(col).count()
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
    pub reasoning_buffer: String,
    pub streaming_buffer: String,
    pub scroll_offset: u32,
    pub user_scrolled: bool,
    pub input: InputBuffer,
    pub interaction_queue: VecDeque<PendingInteraction>,
    pub phase: AgentPhase,
    pub spinner_frame: usize,
    pub should_quit: bool,
    pub session_id: String,
    /// Token usage from the most recently completed turn.
    pub last_turn_usage: Option<quine_llm::TokenUsage>,
    /// Max context window for the configured model, if known.
    pub max_context_window: Option<u64>,
    /// History of submitted inputs (oldest first).
    pub input_history: Vec<String>,
    /// Current position in history (None = not browsing history).
    history_index: Option<usize>,
    /// Saved in-progress input when entering history mode.
    saved_input: String,
    /// Active option selector state (for SingleSelect/MultiSelect interactions).
    pub option_select: Option<OptionSelectState>,
    /// Whether this session is in read-only plan mode.
    pub plan_mode: bool,
    /// Last known conversation view height (set during rendering for scroll step sizing).
    pub last_view_height: u32,
}

struct LoopCommand {
    request: String,
    delay: Duration,
    cadence: Option<Duration>,
}

impl LoopCommand {
    fn description(&self) -> String {
        match self.cadence {
            Some(cadence) => format!("every {}s: {}", cadence.as_secs(), self.request),
            None => format!("in {}s: {}", self.delay.as_secs(), self.request),
        }
    }
}

fn parse_loop_arguments(arguments: &str) -> Result<LoopCommand, String> {
    let trimmed = arguments.trim();
    let mut parts = trimmed.split_whitespace();
    let mode = parts.next().ok_or_else(|| {
        "Usage: /loop every <duration> <message> | /loop in <duration> <message>".to_string()
    })?;
    let duration_text = parts.next().ok_or_else(|| {
        "Usage: /loop every <duration> <message> | /loop in <duration> <message>".to_string()
    })?;
    let request = parts.collect::<Vec<_>>().join(" ");
    if request.is_empty() {
        return Err(
            "Usage: /loop every <duration> <message> | /loop in <duration> <message>".into(),
        );
    }
    let duration = parse_duration(duration_text)?;
    match mode {
        "every" => Ok(LoopCommand {
            request,
            delay: duration,
            cadence: Some(duration),
        }),
        "in" => Ok(LoopCommand {
            request,
            delay: duration,
            cadence: None,
        }),
        _ => Err("Usage: /loop every <duration> <message> | /loop in <duration> <message>".into()),
    }
}

fn parse_duration(input: &str) -> Result<Duration, String> {
    if input.is_empty() {
        return Err("Duration cannot be empty".into());
    }
    let split_at = input
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| "Duration must include a unit like s, m, h, or d".to_string())?;
    let (value, unit) = input.split_at(split_at);
    let amount = value
        .parse::<u64>()
        .map_err(|_| format!("Invalid duration value: {input}"))?;
    match unit {
        "s" => Ok(Duration::from_secs(amount)),
        "m" => Ok(Duration::from_secs(amount * 60)),
        "h" => Ok(Duration::from_secs(amount * 60 * 60)),
        "d" => Ok(Duration::from_secs(amount * 60 * 60 * 24)),
        _ => Err(format!("Unsupported duration unit: {unit}")),
    }
}

impl App {
    pub fn new(session_id: String, plan_mode: bool, max_context_window: Option<u64>) -> Self {
        Self {
            messages: Vec::new(),
            reasoning_buffer: String::new(),
            streaming_buffer: String::new(),
            scroll_offset: 0,
            user_scrolled: false,
            input: InputBuffer::new(),
            interaction_queue: VecDeque::new(),
            phase: AgentPhase::Idle,
            spinner_frame: 0,
            should_quit: false,
            session_id,
            last_turn_usage: None,
            max_context_window,
            input_history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
            option_select: None,
            plan_mode,
            last_view_height: 0,
        }
    }

    /// Reset UI state after cancelling in-flight work.
    pub fn cancel_active_turn(&mut self) {
        self.phase = AgentPhase::Idle;
        self.reasoning_buffer.clear();
        self.streaming_buffer.clear();
        self.interaction_queue.clear();
        self.option_select = None;
        self.auto_scroll();
    }

    /// Reset state for a newly created session.
    pub fn reset_for_new_session(
        &mut self,
        session_id: String,
        plan_mode: bool,
        max_context_window: Option<u64>,
    ) {
        self.messages.clear();
        self.reasoning_buffer.clear();
        self.streaming_buffer.clear();
        self.scroll_offset = 0;
        self.user_scrolled = false;
        self.interaction_queue.clear();
        self.phase = AgentPhase::Idle;
        self.spinner_frame = 0;
        self.session_id = session_id;
        self.last_turn_usage = None;
        self.max_context_window = max_context_window;
        self.option_select = None;
        self.plan_mode = plan_mode;
        self.auto_scroll();
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
            let source = interaction.source_label.as_deref().unwrap_or("agent");
            let pending = self.interaction_queue.len();
            let badge = if pending > 1 {
                format!(" [{pending} pending]")
            } else {
                String::new()
            };
            match interaction.kind {
                InteractionKind::Permission => {
                    format!("[{source}] [permission]{badge} (y/n) > ")
                }
                InteractionKind::AskUser => {
                    format!("[{source}] [ask_user]{badge} > ")
                }
                InteractionKind::SingleSelect => {
                    format!("[{source}] [select]{badge} [↑↓] Enter > ")
                }
                InteractionKind::MultiSelect => {
                    format!("[{source}] [multi-select]{badge} [↑↓] Space/Enter > ")
                }
            }
        } else if self.plan_mode {
            "[plan] > ".to_string()
        } else {
            "> ".to_string()
        }
    }

    /// Handle Enter/Ctrl+S: send message or submit interaction response.
    pub fn submit_input(&mut self) -> Option<AppAction> {
        if let Some(select) = self.option_select.take() {
            if select.allow_freeform && select.cursor == select.options.len() - 1 {
                return None;
            }
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
            self.input_history.push(text.clone());
            if let Some(command) = parse_slash_command(&text) {
                match command {
                    SlashCommand::BuiltIn { name, arguments } => match name.as_str() {
                        "quit" => {
                            self.should_quit = true;
                            Some(AppAction::Quit)
                        }
                        "plan" => {
                            if arguments.is_empty() {
                                self.messages.push(ConversationEntry::Error(
                                    "Usage: /plan <request>".into(),
                                ));
                                self.auto_scroll();
                                None
                            } else {
                                let was_plan_mode = self.plan_mode;
                                if was_plan_mode {
                                    self.messages
                                        .push(ConversationEntry::User(arguments.clone()));
                                    self.phase = AgentPhase::Thinking;
                                    self.auto_scroll();
                                    Some(AppAction::SendMessage(arguments))
                                } else {
                                    Some(AppAction::EnterPlanMode {
                                        request: arguments,
                                        was_plan_mode,
                                    })
                                }
                            }
                        }
                        "loop" => match parse_loop_arguments(&arguments) {
                            Ok(loop_command) => {
                                self.messages.push(ConversationEntry::AssistantText(format!(
                                    "Scheduled loop: {}",
                                    loop_command.description()
                                )));
                                self.auto_scroll();
                                Some(AppAction::ScheduleLoop {
                                    request: loop_command.request,
                                    delay: loop_command.delay,
                                    cadence: loop_command.cadence,
                                })
                            }
                            Err(message) => {
                                self.messages.push(ConversationEntry::Error(message));
                                self.auto_scroll();
                                None
                            }
                        },
                        other => {
                            self.messages.push(ConversationEntry::Error(format!(
                                "Unknown slash command: /{other}"
                            )));
                            self.auto_scroll();
                            None
                        }
                    },
                    SlashCommand::Skill { name, arguments } => {
                        self.messages.push(ConversationEntry::User(text.clone()));
                        self.phase = AgentPhase::Thinking;
                        self.auto_scroll();
                        Some(AppAction::SendSlashSkillMessage {
                            skill_name: name,
                            request: arguments,
                        })
                    }
                }
            } else {
                self.messages.push(ConversationEntry::User(text.clone()));
                self.phase = AgentPhase::Thinking;
                self.auto_scroll();
                Some(AppAction::SendMessage(text))
            }
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
    #[allow(dead_code)]
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
                self.saved_input = self.input.content();
                self.history_index = Some(self.input_history.len() - 1);
            }
            Some(0) => {}
            Some(i) => {
                self.history_index = Some(i - 1);
            }
        }
        if let Some(i) = self.history_index {
            self.input.set_from_string(&self.input_history[i]);
        }
    }

    /// Navigate to the next input in history (Down arrow).
    pub fn history_next(&mut self) {
        let Some(i) = self.history_index else {
            return;
        };
        if i + 1 < self.input_history.len() {
            self.history_index = Some(i + 1);
            self.input.set_from_string(&self.input_history[i + 1]);
        } else {
            self.history_index = None;
            let saved = std::mem::take(&mut self.saved_input);
            self.input.set_from_string(&saved);
        }
    }

    /// Auto-scroll to the bottom when new content arrives.
    pub fn auto_scroll(&mut self) {
        self.user_scrolled = false;
        self.scroll_offset = 0;
    }

    /// Scroll up by a number of wrapped rows.
    pub fn scroll_up(&mut self, rows: u32) {
        self.user_scrolled = true;
        self.scroll_offset = self.scroll_offset.saturating_add(rows);
    }

    /// Scroll down by a number of wrapped rows.
    pub fn scroll_down(&mut self, rows: u32) {
        self.scroll_offset = self.scroll_offset.saturating_sub(rows);
        if self.scroll_offset == 0 {
            self.user_scrolled = false;
        }
    }

    /// Apply an incoming notification from the daemon to app state.
    pub fn apply_notification(&mut self, notif: &JsonRpcNotification) {
        match notif.method.as_str() {
            notifications::REASONING_DELTA => {
                if notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("delta"))
                    .and_then(|v| v.as_str())
                    .is_some()
                {
                    self.reasoning_buffer.clear();
                }
            }
            notifications::STREAM_DELTA => {
                if let Some(delta) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("delta"))
                    .and_then(|v| v.as_str())
                {
                    self.phase = AgentPhase::Streaming;
                    self.streaming_buffer.push_str(delta);
                    self.auto_scroll();
                }
            }
            notifications::TEXT_COMPLETE => {
                self.reasoning_buffer.clear();
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
                let text = trim_blank_lines(&text);
                if !text.is_empty() {
                    self.messages.push(ConversationEntry::AssistantText(text));
                }
                self.streaming_buffer.clear();
                self.auto_scroll();
            }
            notifications::TOOL_REQUEST => {
                self.reasoning_buffer.clear();
                if !self.streaming_buffer.trim().is_empty() {
                    let text = trim_blank_lines(&std::mem::take(&mut self.streaming_buffer));
                    if !text.is_empty() {
                        self.messages.push(ConversationEntry::AssistantText(text));
                    }
                }
                self.streaming_buffer.clear();

                if let Some(params) = &notif.params {
                    let tool_name = params
                        .get("tool_name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let tool_use_id = params
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let arguments = params.get("arguments").cloned().unwrap_or_default();
                    let summary = summarize_tool_call(&tool_name, &arguments);
                    let summary = if summary.len() > 60 {
                        format!("{}…", &summary[..59])
                    } else {
                        summary
                    };
                    self.phase = AgentPhase::RunningTool(tool_name.clone());
                    self.messages.push(ConversationEntry::ToolCall {
                        tool_name: tool_name.clone(),
                        tool_use_id,
                        summary,
                        status: ToolStatus::Running,
                        result_preview: None,
                    });
                    if tool_name == "apply_patch" {
                        if let Some(preview) = build_apply_patch_preview(&arguments) {
                            self.messages.push(ConversationEntry::PatchPreview(preview));
                        }
                    }
                    self.auto_scroll();
                }
            }
            notifications::TOOL_RESULT => {
                if let Some(params) = &notif.params {
                    let tool_use_id = params
                        .get("tool_use_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    let is_error = params
                        .get("is_error")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    let duration_us = params
                        .get("duration_us")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0);
                    let content = params.get("content").and_then(|v| v.as_str()).unwrap_or("");
                    for entry in self.messages.iter_mut().rev() {
                        if let ConversationEntry::ToolCall {
                            tool_name,
                            tool_use_id: id,
                            status,
                            result_preview,
                            ..
                        } = entry
                        {
                            if id.as_str() == tool_use_id {
                                *status = if is_error {
                                    ToolStatus::Error { duration_us }
                                } else {
                                    ToolStatus::Success { duration_us }
                                };
                                if tool_name == "plan" {
                                    let trimmed = trim_blank_lines(content);
                                    if !trimmed.is_empty() {
                                        *result_preview = Some(trimmed);
                                    }
                                }
                                break;
                            }
                        }
                    }
                }
            }
            notifications::TURN_COMPLETE => {
                self.reasoning_buffer.clear();
                if !self.streaming_buffer.trim().is_empty() {
                    let text = trim_blank_lines(&std::mem::take(&mut self.streaming_buffer));
                    if !text.is_empty() {
                        self.messages.push(ConversationEntry::AssistantText(text));
                    }
                }
                self.streaming_buffer.clear();
                let duration_us = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("duration_us"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let usage = notif.params.as_ref().and_then(|p| {
                    p.get("usage").and_then(|v| {
                        Some(quine_llm::TokenUsage {
                            input_tokens: v.get("input_tokens")?.as_u64()?,
                            output_tokens: v.get("output_tokens")?.as_u64()?,
                        })
                    })
                });
                self.last_turn_usage = usage.clone();
                self.messages
                    .push(ConversationEntry::TurnInfo { duration_us, usage });
                self.phase = AgentPhase::Idle;
                self.auto_scroll();
            }
            notifications::PLAN_PROGRESS => {
                if let Some(params) = &notif.params {
                    let action_id = params
                        .get("action_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_string();
                    let status = params
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let remaining = params
                        .get("remaining")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(0) as usize;
                    let total = params.get("total").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                    self.messages.push(ConversationEntry::PlanProgress {
                        action_id,
                        status,
                        remaining,
                        total,
                    });
                    self.auto_scroll();
                }
            }
            notifications::SESSION_ERROR => {
                if let Some(err) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("error"))
                    .and_then(|v| v.as_str())
                {
                    self.messages
                        .push(ConversationEntry::Error(err.to_string()));
                }
            }
            notifications::INTERACTION_NEEDED => {
                let prompt = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("prompt"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
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
                    .unwrap_or(true);
                let kind = match notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("kind"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("Question")
                {
                    "Confirmation" => InteractionKind::Permission,
                    "SingleSelect" => InteractionKind::SingleSelect,
                    "MultiSelect" => InteractionKind::MultiSelect,
                    _ => InteractionKind::AskUser,
                };
                let source_label = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("source_label"))
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string);

                self.messages.push(ConversationEntry::InteractionQuestion {
                    prompt: prompt.clone(),
                    options: options.clone(),
                });
                self.interaction_queue.push_back(PendingInteraction {
                    prompt,
                    kind: kind.clone(),
                    options: options.clone(),
                    allow_freeform,
                    source_label,
                });

                match kind {
                    InteractionKind::SingleSelect | InteractionKind::MultiSelect => {
                        let mut select_options = options;
                        if allow_freeform {
                            select_options.push("Other...".to_string());
                        }
                        self.option_select = Some(OptionSelectState {
                            options: select_options,
                            cursor: 0,
                            selected: HashSet::new(),
                            multi_select: kind == InteractionKind::MultiSelect,
                            allow_freeform,
                        });
                    }
                    InteractionKind::Permission | InteractionKind::AskUser => {
                        self.option_select = None;
                    }
                }
                self.auto_scroll();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_notif(method: &str, params: serde_json::Value) -> JsonRpcNotification {
        JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
        }
    }

    #[test]
    fn trim_blank_lines_strips_edges_and_preserves_internal_blank_lines() {
        assert_eq!(
            trim_blank_lines("\n\n  first line\n\nsecond line\n\n"),
            "  first line\n\nsecond line"
        );
        assert_eq!(trim_blank_lines("\n \n\t\n"), "");
    }

    #[test]
    fn text_complete_stores_trimmed_assistant_text() {
        let mut app = App::new("test".into(), false, None);
        let notif = make_notif(
            notifications::TEXT_COMPLETE,
            serde_json::json!({
                "full_text": "\n\nhello\n\nworld\n\n"
            }),
        );

        app.apply_notification(&notif);

        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::AssistantText(text)) if text == "hello\n\nworld"
        ));
    }

    #[test]
    fn turn_complete_flushes_trimmed_streaming_buffer() {
        let mut app = App::new("test".into(), false, None);
        app.streaming_buffer = "\n\nhello\n\nworld\n\n".into();

        let notif = make_notif(
            notifications::TURN_COMPLETE,
            serde_json::json!({
                "duration_us": 12
            }),
        );

        app.apply_notification(&notif);

        assert!(matches!(
            app.messages.first(),
            Some(ConversationEntry::AssistantText(text)) if text == "hello\n\nworld"
        ));
        assert!(matches!(
            app.messages.get(1),
            Some(ConversationEntry::TurnInfo {
                duration_us: 12,
                ..
            })
        ));
        assert!(app.streaming_buffer.is_empty());
    }

    #[test]
    fn tool_request_for_apply_patch_adds_preview() {
        let mut app = App::new("test".into(), false, None);
        let notif = make_notif(
            notifications::TOOL_REQUEST,
            serde_json::json!({
                "tool_name": "apply_patch",
                "tool_use_id": "toolu_1",
                "arguments": {
                    "file_path": "src/main.rs",
                    "edits": [
                        {
                            "old_text": "old line\n",
                            "new_text": "new line\n"
                        }
                    ]
                }
            }),
        );

        app.apply_notification(&notif);

        assert!(matches!(
            app.messages.first(),
            Some(ConversationEntry::ToolCall { tool_name, .. }) if tool_name == "apply_patch"
        ));
        assert!(matches!(
            app.messages.get(1),
            Some(ConversationEntry::PatchPreview(preview))
                if preview.contains("apply_patch: src/main.rs")
                && preview.contains("- old line")
                && preview.contains("+ new line")
        ));
    }

    #[test]
    fn plan_tool_result_is_attached_to_tool_call() {
        let mut app = App::new("test".into(), false, None);
        let request = make_notif(
            notifications::TOOL_REQUEST,
            serde_json::json!({
                "tool_name": "plan",
                "tool_use_id": "toolu_plan",
                "arguments": {
                    "operation": "create_plan",
                    "title": "Example plan",
                    "actions": []
                }
            }),
        );
        app.apply_notification(&request);

        let result = make_notif(
            notifications::TOOL_RESULT,
            serde_json::json!({
                "tool_use_id": "toolu_plan",
                "tool_name": "plan",
                "content": "Plan: Example plan (2 actions)\n\n  [a1] First task                     (ready)\n  [a2] Second task                    (blocked by: a1)",
                "is_error": false,
                "duration_us": 42
            }),
        );
        app.apply_notification(&result);

        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::ToolCall {
                tool_name,
                status: ToolStatus::Success { duration_us: 42 },
                result_preview: Some(preview),
                ..
            }) if tool_name == "plan" && preview.contains("[a1]") && preview.contains("[a2]")
        ));
    }

    #[test]
    fn plan_progress_notification_is_recorded() {
        let mut app = App::new("test".into(), false, None);
        let notif = make_notif(
            notifications::PLAN_PROGRESS,
            serde_json::json!({
                "plan_id": "plan_123",
                "action_id": "implement-ui",
                "status": "in_progress",
                "remaining": 2,
                "total": 5
            }),
        );

        app.apply_notification(&notif);

        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::PlanProgress {
                action_id,
                status,
                remaining,
                total,
            }) if action_id == "implement-ui"
                && status == "in_progress"
                && *remaining == 2
                && *total == 5
        ));
    }

    #[test]
    fn cancel_active_turn_clears_pending_interaction_state() {
        let mut app = App::new("test".into(), false, None);
        app.phase = AgentPhase::RunningTool("bash".into());
        app.reasoning_buffer = "thinking".into();
        app.streaming_buffer = "partial output".into();
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow?".into(),
            kind: InteractionKind::Permission,
            options: Vec::new(),
            allow_freeform: false,
            source_label: Some("agent".into()),
        });
        app.option_select = Some(OptionSelectState {
            options: vec!["Yes".into(), "No".into()],
            cursor: 0,
            selected: HashSet::new(),
            multi_select: false,
            allow_freeform: false,
        });

        app.cancel_active_turn();

        assert_eq!(app.phase, AgentPhase::Idle);
        assert!(app.reasoning_buffer.is_empty());
        assert!(app.streaming_buffer.is_empty());
        assert!(app.interaction_queue.is_empty());
        assert!(app.option_select.is_none());
    }

    #[test]
    fn submit_input_plan_command_enters_plan_mode() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/plan audit slash commands");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::EnterPlanMode {
                request,
                was_plan_mode: false
            }) if request == "audit slash commands"
        ));
        assert!(!app.plan_mode);
        assert_eq!(app.input_label(), "> ");
        assert!(app.messages.is_empty());
    }

    #[test]
    fn reset_for_new_session_clears_old_transcript_and_buffers() {
        let mut app = App::new("old".into(), false, Some(1024));
        app.messages.push(ConversationEntry::User("stale".into()));
        app.reasoning_buffer = "thinking".into();
        app.streaming_buffer = "partial".into();
        app.scroll_offset = 5;
        app.user_scrolled = true;
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "pick".into(),
            kind: InteractionKind::AskUser,
            options: Vec::new(),
            allow_freeform: true,
            source_label: None,
        });
        app.phase = AgentPhase::Thinking;
        app.spinner_frame = 2;
        app.last_turn_usage = Some(quine_llm::TokenUsage {
            input_tokens: 1,
            output_tokens: 2,
        });
        app.option_select = Some(OptionSelectState {
            options: vec!["a".into()],
            cursor: 0,
            selected: HashSet::new(),
            multi_select: false,
            allow_freeform: false,
        });

        app.reset_for_new_session("new".into(), true, Some(2048));

        assert!(app.messages.is_empty());
        assert!(app.reasoning_buffer.is_empty());
        assert!(app.streaming_buffer.is_empty());
        assert!(app.interaction_queue.is_empty());
        assert!(app.option_select.is_none());
        assert_eq!(app.phase, AgentPhase::Idle);
        assert_eq!(app.spinner_frame, 0);
        assert_eq!(app.session_id, "new");
        assert_eq!(app.max_context_window, Some(2048));
        assert!(app.last_turn_usage.is_none());
        assert!(app.plan_mode);
        assert_eq!(app.scroll_offset, 0);
        assert!(!app.user_scrolled);
    }

    #[test]
    fn submit_input_plan_without_arguments_is_local_only() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/plan");

        let action = app.submit_input();

        assert!(action.is_none());
        assert!(!app.plan_mode);
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::Error(text)) if text == "Usage: /plan <request>"
        ));
    }

    #[test]
    fn submit_input_unknown_slash_command_is_skill_session_handoff() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/review audit this");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::SendSlashSkillMessage { skill_name, request })
                if skill_name == "review" && request == "audit this"
        ));
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::User(text)) if text == "/review audit this"
        ));
        assert!(matches!(app.phase, AgentPhase::Thinking));
    }

    #[test]
    fn submit_input_bare_skill_command_is_skill_session_handoff() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/feature-planning");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::SendSlashSkillMessage { skill_name, request })
                if skill_name == "feature-planning" && request.is_empty()
        ));
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::User(text)) if text == "/feature-planning"
        ));
        assert!(matches!(app.phase, AgentPhase::Thinking));
    }

    #[test]
    fn submit_input_loop_command_schedules_work() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/loop every 5m check logs");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ScheduleLoop { request, delay, cadence })
                if request == "check logs"
                    && delay == Duration::from_secs(300)
                    && cadence == Some(Duration::from_secs(300))
        ));
    }

    #[test]
    fn submit_input_invalid_loop_is_local_error() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/loop every nope check logs");

        let action = app.submit_input();

        assert!(action.is_none());
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::Error(text)) if text.contains("Invalid duration")
                || text.contains("Unsupported duration")
        ));
    }
}
