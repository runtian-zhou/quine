use std::collections::{HashSet, VecDeque};
use std::fmt;

use std::time::{Duration, Instant};

use crate::context_debug::{HistoryEntry, SessionContextSnapshot};
use crate::slash_command::{parse_slash_command, SlashCommand};
use quine_harness::protocol::{notifications, JsonRpcNotification};
use ratatui::text::Line;
use unicode_width::UnicodeWidthChar;

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
    Running {
        started_at: Instant,
        timeout: Option<Duration>,
    },
    Success {
        duration_us: u64,
    },
    Error {
        duration_us: u64,
    },
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
    ToolBatch {
        calls: Vec<ToolBatchCall>,
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
    InteractionPrompt {
        summary: Option<String>,
        prompt: String,
    },
    /// Turn summary with timing and token usage.
    TurnInfo {
        duration_us: u64,
        usage: Option<quine_llm::TokenUsage>,
    },
}

#[derive(Debug, Clone)]
pub struct ToolBatchCall {
    pub tool_name: String,
    pub summary: String,
    pub status: ToolStatus,
    pub result_preview: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchSessionCandidate {
    pub session_id: String,
    pub summary: Option<String>,
}

/// Actions the event loop should perform after handling an event.
pub enum AppAction {
    SendMessage(String),
    ShowContext,
    ListSessions {
        tree: bool,
    },
    CompactSession,
    ClearSession,
    SwitchSession {
        session_id: String,
    },
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingPlanExit {
    FinalPlan { final_plan: String },
    StartSkillSession { skill_name: String, request: String },
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
        self.lines[row]
            .chars()
            .take(col)
            .map(|ch| ch.width().unwrap_or(0))
            .sum()
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextExplorerTab {
    History,
    Tools,
    Skills,
    Plans,
}

#[derive(Debug, Clone)]
pub struct ContextExplorerState {
    pub snapshot: SessionContextSnapshot,
    pub active_tab: ContextExplorerTab,
    pub selected_index: usize,
    pub scroll_offset: u16,
}

impl ContextExplorerState {
    fn new(snapshot: SessionContextSnapshot) -> Self {
        Self {
            snapshot,
            active_tab: ContextExplorerTab::History,
            selected_index: 0,
            scroll_offset: 0,
        }
    }

    fn entry_count(&self) -> usize {
        self.snapshot.history.len()
    }

    fn tool_count(&self) -> usize {
        self.snapshot.available_tools.len()
    }

    fn skill_count(&self) -> usize {
        self.snapshot.loaded_skills.len()
    }

    fn reset_detail_state(&mut self) {
        self.selected_index = 0;
        self.scroll_offset = 0;
    }

    pub fn selected_entry(&self) -> Option<&HistoryEntry> {
        self.snapshot.history.get(self.selected_index)
    }

    pub fn selected_tool(&self) -> Option<&quine_llm::ToolDefinition> {
        self.snapshot.available_tools.get(self.selected_index)
    }

    pub fn selected_skill(&self) -> Option<&crate::context_debug::SkillSnapshot> {
        self.snapshot.loaded_skills.get(self.selected_index)
    }
}

/// The main application state.
pub struct App {
    pub messages: Vec<ConversationEntry>,
    pub reasoning_buffer: String,
    pub streaming_buffer: String,
    pub current_turn_assistant_text: Option<String>,
    last_backend_event_at: Option<Instant>,
    last_backend_event_label: Option<&'static str>,
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
    pub loaded_skill_commands: Vec<String>,
    pub slash_select_active: bool,
    pub switch_select_active: bool,
    pub switch_session_candidates: Vec<SwitchSessionCandidate>,
    /// Whether this session is in read-only plan mode.
    pub plan_mode: bool,
    /// Pending local confirmation required before leaving plan mode.
    pub pending_plan_exit: Option<PendingPlanExit>,
    /// Last known conversation view height (set during rendering for scroll step sizing).
    pub last_view_height: u32,
    /// Interactive explorer for large `/context` snapshots in the TUI.
    pub context_explorer: Option<ContextExplorerState>,
    /// Monotonic revision for conversation-affecting state.
    conversation_revision: u64,
    /// Cached rendered conversation lines and wrapped height for the current width.
    pub conversation_cache: Option<ConversationRenderCache>,
}

pub struct ConversationRenderCache {
    pub width: u16,
    pub revision: u64,
    pub lines: Vec<Line<'static>>,
    pub content_height: u32,
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
            delay: Duration::ZERO,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionStateNotification {
    Idle,
    Streaming,
    AwaitingToolResult,
    Waiting,
    Paused,
    Destroyed,
}

fn notification_session_id(notif: &JsonRpcNotification) -> Option<&str> {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("session_id"))
        .and_then(|value| value.as_str())
}

fn parse_session_state(value: &serde_json::Value) -> Option<SessionStateNotification> {
    let state = value.as_str()?;
    match state {
        "Idle" | "idle" => Some(SessionStateNotification::Idle),
        "Streaming" | "streaming" => Some(SessionStateNotification::Streaming),
        "AwaitingToolResult" | "awaiting_tool_result" => {
            Some(SessionStateNotification::AwaitingToolResult)
        }
        "Waiting" | "waiting" => Some(SessionStateNotification::Waiting),
        "Paused" | "paused" => Some(SessionStateNotification::Paused),
        "Destroyed" | "destroyed" => Some(SessionStateNotification::Destroyed),
        _ => None,
    }
}

impl App {
    pub fn new(session_id: String, plan_mode: bool, max_context_window: Option<u64>) -> Self {
        Self {
            messages: Vec::new(),
            reasoning_buffer: String::new(),
            streaming_buffer: String::new(),
            current_turn_assistant_text: None,
            last_backend_event_at: None,
            last_backend_event_label: None,
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
            loaded_skill_commands: Vec::new(),
            slash_select_active: false,
            switch_select_active: false,
            switch_session_candidates: Vec::new(),
            plan_mode,
            pending_plan_exit: None,
            last_view_height: 0,
            context_explorer: None,
            conversation_revision: 0,
            conversation_cache: None,
        }
    }

    fn invalidate_conversation_cache(&mut self) {
        self.conversation_revision = self.conversation_revision.wrapping_add(1);
        self.conversation_cache = None;
    }

    pub fn conversation_revision(&self) -> u64 {
        self.conversation_revision
    }

    pub fn push_message(&mut self, entry: ConversationEntry) {
        self.messages.push(entry);
        self.invalidate_conversation_cache();
    }

    pub fn set_phase(&mut self, phase: AgentPhase) {
        if self.phase != phase {
            self.phase = phase;
            self.invalidate_conversation_cache();
        }
    }

    /// Reset UI state after cancelling in-flight work.
    pub fn cancel_active_turn(&mut self) {
        self.set_phase(AgentPhase::Idle);
        self.reasoning_buffer.clear();
        if !self.streaming_buffer.is_empty() {
            self.streaming_buffer.clear();
            self.invalidate_conversation_cache();
        }
        self.current_turn_assistant_text = None;
        self.interaction_queue.clear();
        self.option_select = None;
        self.pending_plan_exit = None;
        self.context_explorer = None;
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
        self.current_turn_assistant_text = None;
        self.last_backend_event_at = None;
        self.last_backend_event_label = None;
        self.scroll_offset = 0;
        self.user_scrolled = false;
        self.interaction_queue.clear();
        self.phase = AgentPhase::Idle;
        self.spinner_frame = 0;
        self.session_id = session_id;
        self.last_turn_usage = None;
        self.max_context_window = max_context_window;
        self.option_select = None;
        self.loaded_skill_commands.clear();
        self.plan_mode = plan_mode;
        self.pending_plan_exit = None;
        self.context_explorer = None;
        self.invalidate_conversation_cache();
        self.auto_scroll();
    }

    pub fn exit_plan_mode(&mut self) {
        self.plan_mode = false;
        self.pending_plan_exit = None;
        self.set_phase(AgentPhase::Idle);
        self.invalidate_conversation_cache();
    }

    pub fn load_session_context(&mut self, snapshot: SessionContextSnapshot) {
        self.loaded_skill_commands = snapshot
            .loaded_skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        self.messages.clear();

        for entry in snapshot.history {
            match entry {
                HistoryEntry::Text { role, text } => match role.as_str() {
                    "user" => self.push_message(ConversationEntry::User(text)),
                    "assistant" => {
                        if !text.is_empty() {
                            self.push_message(ConversationEntry::AssistantText(text));
                        }
                    }
                    _ => {}
                },
                HistoryEntry::ToolUse { role, text, .. } => {
                    if role == "assistant" {
                        if let Some(text) = text.filter(|value| !value.is_empty()) {
                            self.push_message(ConversationEntry::AssistantText(text));
                        }
                    }
                }
                HistoryEntry::ToolResult { .. } => {}
            }
        }

        self.invalidate_conversation_cache();
        self.auto_scroll();
    }

    /// Advance the spinner animation frame.
    pub fn tick_spinner(&mut self) {
        if self.phase != AgentPhase::Idle {
            self.spinner_frame = (self.spinner_frame + 1) % SPINNER_FRAMES.len();
            if self.has_running_tool_timers() {
                self.invalidate_conversation_cache();
            }
        }
    }

    fn has_running_tool_timers(&self) -> bool {
        self.messages.iter().any(|entry| {
            matches!(
                entry,
                ConversationEntry::ToolCall {
                    tool_name,
                    status: ToolStatus::Running { .. },
                    ..
                } if tool_name == "bash"
            )
        })
    }

    /// Get the current spinner character.
    pub fn spinner_char(&self) -> char {
        SPINNER_FRAMES[self.spinner_frame]
    }

    pub fn phase_status_text(&self) -> String {
        let base = match &self.phase {
            AgentPhase::Idle => "Idle".to_string(),
            AgentPhase::Thinking => format!("{} Thinking", self.spinner_char()),
            AgentPhase::Streaming => format!("{} Responding", self.spinner_char()),
            AgentPhase::RunningTool(name) => {
                format!("{} Running {name}", self.spinner_char())
            }
        };

        match self.backend_activity_text() {
            Some(activity) => format!("{base} · {activity}"),
            None => base,
        }
    }

    fn note_backend_event(&mut self, label: &'static str) {
        self.last_backend_event_at = Some(Instant::now());
        self.last_backend_event_label = Some(label);
    }

    fn backend_activity_text(&self) -> Option<String> {
        const STALE_AFTER_SECS: u64 = 15;

        let last_at = self.last_backend_event_at?;
        let label = self.last_backend_event_label.unwrap_or("event");
        let age_secs = Instant::now().saturating_duration_since(last_at).as_secs();

        let prefix = if matches!(self.phase, AgentPhase::Idle) {
            "last backend"
        } else if age_secs >= STALE_AFTER_SECS {
            "backend quiet"
        } else {
            "backend active"
        };

        Some(format!("{prefix}: {label} {age_secs}s ago"))
    }

    /// Get the label for the input box.
    ///
    /// Permission prompts show a short label here — the full prompt is
    /// rendered in the conversation view instead.
    pub fn input_label(&self) -> String {
        if self.pending_plan_exit.is_some() {
            "[plan] [confirm-exit] (y/n) > ".to_string()
        } else if let Some(interaction) = self.interaction_queue.front() {
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

    pub(crate) fn finalize_slash_command_selection(&mut self) -> bool {
        if self.slash_select_active {
            self.accept_slash_command_option()
        } else {
            self.autocomplete_slash_command()
        }
    }

    pub(crate) fn autocomplete_slash_command(&mut self) -> bool {
        let text = self.input.content();
        let Some(stripped) = text.strip_prefix('/') else {
            return false;
        };
        let prefix = stripped.trim_start();

        let commands = [
            "ps", "ps tree", "plan", "loop", "compact", "context", "clear", "switch",
        ];
        let matches: Vec<&str> = commands
            .into_iter()
            .filter(|candidate| prefix.is_empty() || candidate.starts_with(prefix))
            .collect();

        if matches.len() != 1 {
            return false;
        }

        let completed = format!("/{} ", matches[0]);
        self.input.set_from_string(&completed);
        self.refresh_slash_command_options();
        true
    }

    pub(crate) fn slash_command_hint(&self) -> Option<Vec<(String, &'static str)>> {
        let text = self.input.content();
        let prefix = text.strip_prefix('/')?;
        let prefix = prefix.trim_start();

        let mut commands = vec![
            ("ps".to_string(), "/ps".to_string(), "list sessions"),
            (
                "ps tree".to_string(),
                "/ps tree".to_string(),
                "show session tree",
            ),
            ("plan".to_string(), "/plan".to_string(), "toggle plan mode"),
            (
                "loop".to_string(),
                "/loop".to_string(),
                "run autonomous loop",
            ),
            (
                "compact".to_string(),
                "/compact".to_string(),
                "compact current session",
            ),
            (
                "context".to_string(),
                "/context".to_string(),
                "show context details",
            ),
            (
                "clear".to_string(),
                "/clear".to_string(),
                "start a fresh session",
            ),
            (
                "switch".to_string(),
                "/switch".to_string(),
                "switch to another session",
            ),
        ];
        commands.extend(self.loaded_skill_commands.iter().cloned().map(|skill| {
            let command = format!("/{skill}");
            (skill, command, "run a skill command")
        }));

        let filtered: Vec<(String, &'static str)> = commands
            .into_iter()
            .filter(|(matcher, _, _)| prefix.is_empty() || matcher.starts_with(prefix))
            .map(|(_, command, help)| (command, help))
            .collect();

        if filtered.is_empty() {
            None
        } else {
            Some(filtered)
        }
    }

    /// Handle Enter/Ctrl+S: send message or submit interaction response.
    pub fn submit_input(&mut self) -> Option<AppAction> {
        if self.interaction_queue.front().is_some() {
            let select = self.option_select.take()?;
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
            self.push_message(ConversationEntry::InteractionPrompt {
                summary: None,
                prompt: response.clone(),
            });
            self.auto_scroll();
            return Some(AppAction::SubmitInteraction(response));
        }

        let text = self.input.content().trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.option_select = None;
        self.slash_select_active = false;
        self.switch_select_active = false;
        self.history_index = None;
        self.saved_input.clear();

        if let Some(pending_exit) = self.pending_plan_exit.take() {
            self.push_message(ConversationEntry::InteractionPrompt {
                summary: None,
                prompt: text.clone(),
            });
            self.auto_scroll();
            return match text.to_ascii_lowercase().as_str() {
                "y" | "yes" => match pending_exit {
                    PendingPlanExit::FinalPlan { final_plan } => {
                        Some(AppAction::ExitPlanMode { final_plan })
                    }
                    PendingPlanExit::StartSkillSession {
                        skill_name,
                        request,
                    } => Some(AppAction::SendSlashSkillMessage {
                        skill_name,
                        request,
                    }),
                },
                "n" | "no" => {
                    self.push_message(ConversationEntry::AssistantText(
                        "Stayed in plan mode.".into(),
                    ));
                    self.auto_scroll();
                    None
                }
                _ => {
                    self.pending_plan_exit = Some(pending_exit);
                    self.push_message(ConversationEntry::Error("Please answer yes or no.".into()));
                    self.auto_scroll();
                    None
                }
            };
        }

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
            self.messages.push(ConversationEntry::InteractionPrompt {
                summary: None,
                prompt: text,
            });
            self.invalidate_conversation_cache();
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
                        "ps" => {
                            let tree = matches!(arguments.trim(), "tree");
                            Some(AppAction::ListSessions { tree })
                        }
                        "compact" => {
                            if arguments.is_empty() {
                                self.push_message(ConversationEntry::AssistantText(
                                    "Compacting context...".into(),
                                ));
                                self.set_phase(AgentPhase::Thinking);
                                self.auto_scroll();
                                Some(AppAction::CompactSession)
                            } else {
                                self.push_message(ConversationEntry::Error(
                                    "Usage: /compact".into(),
                                ));
                                self.auto_scroll();
                                None
                            }
                        }
                        "context" => {
                            if arguments.is_empty() {
                                Some(AppAction::ShowContext)
                            } else {
                                self.messages
                                    .push(ConversationEntry::Error("Usage: /context".into()));
                                self.auto_scroll();
                                None
                            }
                        }
                        "clear" => {
                            if arguments.is_empty() {
                                self.push_message(ConversationEntry::AssistantText(
                                    "Starting a fresh session...".into(),
                                ));
                                self.set_phase(AgentPhase::Thinking);
                                self.auto_scroll();
                                Some(AppAction::ClearSession)
                            } else {
                                self.push_message(ConversationEntry::Error("Usage: /clear".into()));
                                self.auto_scroll();
                                None
                            }
                        }
                        "switch" => {
                            let target = arguments.trim();
                            if target.is_empty() {
                                self.push_message(ConversationEntry::Error(
                                    "Usage: /switch <session-id>".into(),
                                ));
                                self.auto_scroll();
                                None
                            } else {
                                Some(AppAction::SwitchSession {
                                    session_id: target.to_string(),
                                })
                            }
                        }
                        "plan" => {
                            if arguments.is_empty() {
                                self.push_message(ConversationEntry::Error(
                                    "Usage: /plan <request>".into(),
                                ));
                                self.auto_scroll();
                                None
                            } else {
                                let was_plan_mode = self.plan_mode;
                                if was_plan_mode {
                                    self.push_message(ConversationEntry::User(arguments.clone()));
                                    self.set_phase(AgentPhase::Thinking);
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
                                self.push_message(ConversationEntry::AssistantText(format!(
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
                                self.push_message(ConversationEntry::Error(message));
                                self.auto_scroll();
                                None
                            }
                        },
                        other => {
                            self.push_message(ConversationEntry::Error(format!(
                                "Unknown slash command: /{other}"
                            )));
                            self.auto_scroll();
                            None
                        }
                    },
                    SlashCommand::Skill { name, arguments } => {
                        if self.plan_mode {
                            self.push_message(ConversationEntry::User(text.clone()));
                            self.push_message(ConversationEntry::InteractionQuestion {
                                prompt: format!(
                                    "Leave plan mode and start /{name}? Answer yes or no."
                                ),
                                options: vec!["Yes".into(), "No".into()],
                            });
                            self.pending_plan_exit = Some(PendingPlanExit::StartSkillSession {
                                skill_name: name,
                                request: arguments,
                            });
                            self.auto_scroll();
                            None
                        } else {
                            self.set_phase(AgentPhase::Thinking);
                            self.auto_scroll();
                            Some(AppAction::SendSlashSkillMessage {
                                skill_name: name,
                                request: arguments,
                            })
                        }
                    }
                }
            } else {
                self.push_message(ConversationEntry::User(text.clone()));
                self.set_phase(AgentPhase::Thinking);
                self.auto_scroll();
                Some(AppAction::SendMessage(text))
            }
        }
    }

    pub fn request_plan_exit_confirmation(&mut self, pending_exit: PendingPlanExit) {
        let prompt = match &pending_exit {
            PendingPlanExit::FinalPlan { .. } => {
                "Leave plan mode and start a normal session with this final plan? Answer yes or no."
                    .to_string()
            }
            PendingPlanExit::StartSkillSession { skill_name, .. } => {
                format!("Leave plan mode and start /{skill_name}? Answer yes or no.")
            }
        };
        self.push_message(ConversationEntry::InteractionQuestion {
            prompt,
            options: vec!["Yes".into(), "No".into()],
        });
        self.pending_plan_exit = Some(pending_exit);
        self.auto_scroll();
    }

    pub fn begin_turn(&mut self) {
        self.current_turn_assistant_text = None;
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

    pub(crate) fn refresh_slash_command_options(&mut self) {
        let Some(hints) = self.slash_command_hint() else {
            if self.slash_select_active {
                self.option_select = None;
                self.slash_select_active = false;
            }
            return;
        };

        let options: Vec<String> = hints
            .into_iter()
            .map(|(command, help)| format!("{command}\t{help}"))
            .collect();
        let previous_cursor = self
            .option_select
            .as_ref()
            .map(|state| state.cursor)
            .unwrap_or(0);
        let cursor = previous_cursor.min(options.len().saturating_sub(1));
        self.option_select = Some(OptionSelectState {
            options,
            cursor,
            multi_select: false,
            selected: HashSet::new(),
            allow_freeform: true,
        });
        self.slash_select_active = true;
    }

    pub(crate) fn accept_slash_command_option(&mut self) -> bool {
        if !self.slash_select_active {
            return false;
        }
        let Some(select) = self.option_select.as_ref() else {
            return false;
        };
        let Some(line) = select.options.get(select.cursor) else {
            return false;
        };
        let Some(command) = line.split('\t').next() else {
            return false;
        };
        self.input.set_from_string(&format!("{command} "));
        self.slash_select_active = false;
        self.option_select = None;
        self.refresh_switch_session_options();
        true
    }

    pub(crate) fn preview_slash_command_option(&mut self) -> bool {
        if !self.slash_select_active {
            return false;
        }
        let Some(select) = self.option_select.as_ref() else {
            return false;
        };
        let Some(line) = select.options.get(select.cursor) else {
            return false;
        };
        let Some(command) = line.split('\t').next() else {
            return false;
        };
        self.input.set_from_string(command);
        self.refresh_switch_session_options();
        true
    }

    fn switch_session_prefix(&self) -> Option<String> {
        let content = self.input.content();
        let remainder = content.strip_prefix("/switch")?;
        if remainder.contains('\n') {
            return None;
        }
        Some(remainder.trim_start().to_string())
    }

    pub(crate) fn set_switch_session_candidates(&mut self, sessions: Vec<SwitchSessionCandidate>) {
        self.switch_session_candidates = sessions;
        self.refresh_switch_session_options();
    }

    pub(crate) fn refresh_switch_session_options(&mut self) {
        let Some(prefix) = self.switch_session_prefix() else {
            if self.switch_select_active {
                self.switch_select_active = false;
                self.option_select = None;
            }
            return;
        };

        let options: Vec<String> = self
            .switch_session_candidates
            .iter()
            .filter(|session| prefix.is_empty() || session.session_id.starts_with(prefix.as_str()))
            .map(
                |session| match session.summary.as_deref().filter(|value| !value.is_empty()) {
                    Some(summary) => format!("{}\t{}", session.session_id, summary),
                    None => session.session_id.clone(),
                },
            )
            .collect();

        if options.is_empty() {
            if self.switch_select_active {
                self.switch_select_active = false;
                self.option_select = None;
            }
            return;
        }

        let previous_cursor = self
            .option_select
            .as_ref()
            .map(|state| state.cursor)
            .unwrap_or(0);
        let cursor = previous_cursor.min(options.len().saturating_sub(1));
        self.option_select = Some(OptionSelectState {
            options,
            cursor,
            multi_select: false,
            selected: HashSet::new(),
            allow_freeform: true,
        });
        self.switch_select_active = true;
        self.slash_select_active = false;
    }

    pub(crate) fn accept_switch_session_option(&mut self) -> bool {
        let Some(select) = self.option_select.as_ref() else {
            return false;
        };
        let Some(option) = select.options.get(select.cursor) else {
            return false;
        };
        let session_id = option.split('\t').next().unwrap_or(option);
        self.input.set_from_string(&format!("/switch {session_id}"));
        self.switch_select_active = false;
        self.option_select = None;
        true
    }

    pub fn context_explorer_active(&self) -> bool {
        self.context_explorer.is_some()
    }

    pub fn open_context_explorer(&mut self, snapshot: SessionContextSnapshot) {
        self.loaded_skill_commands = snapshot
            .loaded_skills
            .iter()
            .map(|skill| skill.name.clone())
            .collect();
        self.context_explorer = Some(ContextExplorerState::new(snapshot));
        self.conversation_cache = None;
    }

    pub fn close_context_explorer(&mut self) {
        self.context_explorer = None;
        self.conversation_cache = None;
    }

    pub fn context_explorer_prev_tab(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            explorer.active_tab = match explorer.active_tab {
                ContextExplorerTab::History => ContextExplorerTab::Plans,
                ContextExplorerTab::Tools => ContextExplorerTab::History,
                ContextExplorerTab::Skills => ContextExplorerTab::Tools,
                ContextExplorerTab::Plans => ContextExplorerTab::Skills,
            };
            explorer.reset_detail_state();
        }
    }

    pub fn context_explorer_next_tab(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            explorer.active_tab = match explorer.active_tab {
                ContextExplorerTab::History => ContextExplorerTab::Tools,
                ContextExplorerTab::Tools => ContextExplorerTab::Skills,
                ContextExplorerTab::Skills => ContextExplorerTab::Plans,
                ContextExplorerTab::Plans => ContextExplorerTab::History,
            };
            explorer.reset_detail_state();
        }
    }

    pub fn context_explorer_move_up(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            match explorer.active_tab {
                ContextExplorerTab::History
                | ContextExplorerTab::Tools
                | ContextExplorerTab::Skills => {
                    let count = match explorer.active_tab {
                        ContextExplorerTab::History => explorer.entry_count(),
                        ContextExplorerTab::Tools => explorer.tool_count(),
                        ContextExplorerTab::Skills => explorer.skill_count(),
                        ContextExplorerTab::Plans => 0,
                    };
                    if count > 0 {
                        explorer.selected_index = if explorer.selected_index == 0 {
                            count - 1
                        } else {
                            explorer.selected_index - 1
                        };
                        explorer.scroll_offset = 0;
                    }
                }
                ContextExplorerTab::Plans => {
                    explorer.scroll_offset = explorer.scroll_offset.saturating_sub(1);
                }
            }
        }
    }

    pub fn context_explorer_move_down(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            match explorer.active_tab {
                ContextExplorerTab::History => {
                    let count = explorer.entry_count();
                    if count > 0 {
                        explorer.selected_index = (explorer.selected_index + 1) % count;
                        explorer.scroll_offset = 0;
                    }
                }
                ContextExplorerTab::Tools => {
                    let count = explorer.tool_count();
                    if count > 0 {
                        explorer.selected_index = (explorer.selected_index + 1) % count;
                        explorer.scroll_offset = 0;
                    }
                }
                ContextExplorerTab::Skills => {
                    let count = explorer.skill_count();
                    if count > 0 {
                        explorer.selected_index = (explorer.selected_index + 1) % count;
                        explorer.scroll_offset = 0;
                    }
                }
                ContextExplorerTab::Plans => {
                    explorer.scroll_offset = explorer.scroll_offset.saturating_add(1);
                }
            }
        }
    }

    pub fn context_explorer_move_to_first(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            explorer.scroll_offset = 0;
            if matches!(
                explorer.active_tab,
                ContextExplorerTab::History
                    | ContextExplorerTab::Tools
                    | ContextExplorerTab::Skills
            ) {
                explorer.selected_index = 0;
            }
        }
    }

    pub fn context_explorer_move_to_last(&mut self) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            match explorer.active_tab {
                ContextExplorerTab::History => {
                    if explorer.entry_count() > 0 {
                        explorer.selected_index = explorer.entry_count() - 1;
                    }
                }
                ContextExplorerTab::Tools => {
                    if explorer.tool_count() > 0 {
                        explorer.selected_index = explorer.tool_count() - 1;
                    }
                }
                ContextExplorerTab::Skills => {
                    if explorer.skill_count() > 0 {
                        explorer.selected_index = explorer.skill_count() - 1;
                    }
                }
                ContextExplorerTab::Plans => {}
            }
            explorer.scroll_offset = 0;
        }
    }

    pub fn context_explorer_scroll_up(&mut self, rows: u16) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            explorer.scroll_offset = explorer.scroll_offset.saturating_sub(rows);
        }
    }

    pub fn context_explorer_scroll_down(&mut self, rows: u16) {
        if let Some(explorer) = self.context_explorer.as_mut() {
            explorer.scroll_offset = explorer.scroll_offset.saturating_add(rows);
        }
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
        if notification_session_id(notif).is_some_and(|session_id| session_id != self.session_id) {
            return;
        }

        match notif.method.as_str() {
            notifications::REASONING_DELTA => {
                self.note_backend_event("reasoning");
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
                self.note_backend_event("stream");
                if let Some(delta) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("delta"))
                    .and_then(|v| v.as_str())
                {
                    self.set_phase(AgentPhase::Streaming);
                    self.streaming_buffer.push_str(delta);
                    self.invalidate_conversation_cache();
                    self.auto_scroll();
                }
            }
            notifications::TEXT_COMPLETE => {
                self.note_backend_event("text complete");
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
                    self.current_turn_assistant_text = Some(text.clone());
                    self.push_message(ConversationEntry::AssistantText(text));
                }
                if !self.streaming_buffer.is_empty() {
                    self.streaming_buffer.clear();
                    self.invalidate_conversation_cache();
                }
                self.auto_scroll();
            }
            notifications::TOOL_REQUEST => {
                self.note_backend_event("tool call");
                self.reasoning_buffer.clear();
                if !self.streaming_buffer.trim().is_empty() {
                    let text = trim_blank_lines(&std::mem::take(&mut self.streaming_buffer));
                    if !text.is_empty() {
                        self.current_turn_assistant_text = Some(text.clone());
                        self.push_message(ConversationEntry::AssistantText(text));
                    }
                }
                if !self.streaming_buffer.is_empty() {
                    self.streaming_buffer.clear();
                    self.invalidate_conversation_cache();
                }

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
                    self.set_phase(AgentPhase::RunningTool(tool_name.clone()));
                    let timeout = match tool_name.as_str() {
                        "bash" => Some(Duration::from_secs(
                            arguments
                                .get("timeout")
                                .and_then(|v| v.as_u64())
                                .unwrap_or(120),
                        )),
                        _ => None,
                    };
                    self.push_message(ConversationEntry::ToolCall {
                        tool_name: tool_name.clone(),
                        tool_use_id,
                        summary,
                        status: ToolStatus::Running {
                            started_at: Instant::now(),
                            timeout,
                        },
                        result_preview: None,
                    });
                    if tool_name == "apply_patch" {
                        if let Some(preview) = build_apply_patch_preview(&arguments) {
                            self.push_message(ConversationEntry::PatchPreview(preview));
                        }
                    }
                    self.auto_scroll();
                }
            }
            notifications::TOOL_RESULT => {
                self.note_backend_event("tool result");
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
                                if tool_name == "plan" || tool_name == "bash" {
                                    let trimmed = trim_blank_lines(content);
                                    if !trimmed.is_empty() {
                                        *result_preview = Some(trimmed);
                                    }
                                }
                                if matches!(self.phase, AgentPhase::RunningTool(_)) {
                                    self.set_phase(AgentPhase::Thinking);
                                }
                                self.invalidate_conversation_cache();
                                break;
                            }
                        }
                    }
                }
            }
            notifications::TURN_COMPLETE => {
                self.note_backend_event("turn complete");
                self.reasoning_buffer.clear();
                if !self.streaming_buffer.trim().is_empty() {
                    let text = trim_blank_lines(&std::mem::take(&mut self.streaming_buffer));
                    if !text.is_empty() {
                        self.current_turn_assistant_text = Some(text.clone());
                        self.push_message(ConversationEntry::AssistantText(text));
                    }
                }
                if !self.streaming_buffer.is_empty() {
                    self.streaming_buffer.clear();
                    self.invalidate_conversation_cache();
                }
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
                self.push_message(ConversationEntry::TurnInfo { duration_us, usage });
                self.set_phase(AgentPhase::Idle);
                self.auto_scroll();
            }
            notifications::SESSION_STATE_CHANGED => {
                self.note_backend_event("state");
                if let Some(state) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("state"))
                    .and_then(parse_session_state)
                {
                    match state {
                        SessionStateNotification::Idle => self.set_phase(AgentPhase::Idle),
                        SessionStateNotification::Streaming => {
                            if !self.streaming_buffer.is_empty() {
                                self.set_phase(AgentPhase::Streaming);
                            } else {
                                self.set_phase(AgentPhase::Thinking);
                            }
                        }
                        SessionStateNotification::AwaitingToolResult => {
                            if !matches!(self.phase, AgentPhase::RunningTool(_)) {
                                self.set_phase(AgentPhase::Thinking);
                            }
                        }
                        SessionStateNotification::Waiting | SessionStateNotification::Paused => {
                            self.set_phase(AgentPhase::Idle);
                        }
                        SessionStateNotification::Destroyed => self.set_phase(AgentPhase::Idle),
                    }
                }
            }
            notifications::PLAN_PROGRESS => {
                self.note_backend_event("plan");
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
                    self.push_message(ConversationEntry::PlanProgress {
                        action_id,
                        status,
                        remaining,
                        total,
                    });
                    self.auto_scroll();
                }
            }
            notifications::SESSION_ERROR => {
                self.note_backend_event("error");
                if let Some(err) = notif
                    .params
                    .as_ref()
                    .and_then(|p| p.get("error"))
                    .and_then(|v| v.as_str())
                {
                    self.push_message(ConversationEntry::Error(err.to_string()));
                }
            }
            notifications::INTERACTION_NEEDED => {
                self.note_backend_event("interaction");
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

                self.push_message(ConversationEntry::InteractionQuestion {
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
    use chrono::Utc;
    use std::time::{Duration, Instant};

    fn make_notif(method: &str, params: serde_json::Value) -> JsonRpcNotification {
        JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params: Some(params),
        }
    }

    #[test]
    fn context_explorer_navigation_resets_detail_scroll() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: Utc::now(),
            state: "idle".into(),
            system_prompt: Some("system".into()),
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
        app.context_explorer_scroll_down(8);
        app.context_explorer_move_down();

        let explorer = app.context_explorer.as_ref().expect("explorer open");
        assert_eq!(explorer.selected_index, 1);
        assert_eq!(explorer.scroll_offset, 0);

        app.context_explorer_move_to_first();
        let explorer = app.context_explorer.as_ref().expect("explorer open");
        assert_eq!(explorer.selected_index, 0);
    }

    #[test]
    fn context_explorer_tab_switch_resets_detail_state() {
        let snapshot = SessionContextSnapshot {
            session_id: "session-1".into(),
            created_at: Utc::now(),
            state: "idle".into(),
            system_prompt: Some("system".into()),
            skills: vec!["review".into()],
            working_directory: std::path::PathBuf::from("/tmp/project"),
            plan_mode: false,
            available_tools: vec![quine_llm::ToolDefinition {
                name: "read_file".into(),
                description: "Read file".into(),
                parameters: serde_json::json!({"type": "object"}),
                read_only: true,
                idempotent: true,
            }],
            loaded_skills: vec![crate::context_debug::SkillSnapshot {
                name: "review".into(),
                description: "Review changes".into(),
                version: "1.0".into(),
                system_prompt: Some("Review carefully".into()),
                source_path: std::path::PathBuf::from("/tmp/project/.quine/skills/review.md"),
                tool_names: vec!["read_file".into()],
            }],
            plans: vec![],
            lineage: crate::context_debug::SessionLineageSnapshot::default(),
            prompt_memory: None,
            compact_memory_summary_markdown: None,
            memory_diagnostics: None,
            permission_diagnostics: None,
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
        app.context_explorer_scroll_down(8);
        app.context_explorer_next_tab();

        let explorer = app.context_explorer.as_ref().expect("explorer open");
        assert_eq!(explorer.active_tab, ContextExplorerTab::Tools);
        assert_eq!(explorer.selected_index, 0);
        assert_eq!(explorer.scroll_offset, 0);

        app.context_explorer_next_tab();
        let explorer = app.context_explorer.as_ref().expect("explorer open");
        assert_eq!(explorer.active_tab, ContextExplorerTab::Skills);
        assert_eq!(explorer.selected_index, 0);
        assert_eq!(explorer.scroll_offset, 0);
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
    fn tick_spinner_does_not_invalidate_conversation_cache() {
        let mut app = App::new("test".into(), false, None);
        app.set_phase(AgentPhase::Thinking);
        let revision = app.conversation_revision();

        app.tick_spinner();

        assert_eq!(app.conversation_revision(), revision);
        assert_ne!(app.spinner_frame, 0);
    }

    #[test]
    fn tick_spinner_invalidates_cache_for_running_bash_timer() {
        let mut app = App::new("test".into(), false, None);
        app.push_message(ConversationEntry::ToolCall {
            tool_name: "bash".into(),
            tool_use_id: "toolu_1".into(),
            summary: "sleep 1".into(),
            status: ToolStatus::Running {
                started_at: Instant::now(),
                timeout: Some(Duration::from_secs(120)),
            },
            result_preview: None,
        });
        let revision = app.conversation_revision();
        app.set_phase(AgentPhase::RunningTool("bash".into()));

        app.tick_spinner();

        assert!(app.conversation_revision() > revision);
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
            Some(ConversationEntry::ToolCall {
                tool_name,
                status: ToolStatus::Running { timeout: None, .. },
                ..
            }) if tool_name == "apply_patch"
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
        assert_eq!(app.phase, AgentPhase::Thinking);
    }

    #[test]
    fn bash_tool_result_is_attached_to_tool_call() {
        let mut app = App::new("test".into(), false, None);
        let request = make_notif(
            notifications::TOOL_REQUEST,
            serde_json::json!({
                "tool_name": "bash",
                "tool_use_id": "toolu_bash",
                "arguments": {
                    "command": "pwd"
                }
            }),
        );
        app.apply_notification(&request);

        let result = make_notif(
            notifications::TOOL_RESULT,
            serde_json::json!({
                "tool_use_id": "toolu_bash",
                "tool_name": "bash",
                "content": "/tmp/project\nline two\nline three",
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
            }) if tool_name == "bash" && preview.contains("/tmp/project") && preview.contains("line three")
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
    fn session_state_changed_resets_streaming_phase_to_idle() {
        let mut app = App::new("session-1".into(), false, None);
        app.streaming_buffer = "partial output".into();
        app.set_phase(AgentPhase::Streaming);

        app.apply_notification(&make_notif(
            notifications::SESSION_STATE_CHANGED,
            serde_json::json!({
                "session_id": "session-1",
                "state": "idle"
            }),
        ));

        assert_eq!(app.phase, AgentPhase::Idle);
    }

    #[test]
    fn session_state_changed_ignores_other_sessions() {
        let mut app = App::new("session-1".into(), false, None);
        app.set_phase(AgentPhase::Streaming);

        app.apply_notification(&make_notif(
            notifications::SESSION_STATE_CHANGED,
            serde_json::json!({
                "session_id": "session-2",
                "state": "idle"
            }),
        ));

        assert_eq!(app.phase, AgentPhase::Streaming);
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
    fn cancel_active_turn_clears_pending_plan_exit_confirmation() {
        let mut app = App::new("test".into(), true, None);
        app.pending_plan_exit = Some(PendingPlanExit::FinalPlan {
            final_plan: "Plan text".into(),
        });

        app.cancel_active_turn();

        assert!(app.pending_plan_exit.is_none());
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
    fn submit_input_compact_triggers_manual_compaction() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/compact");

        let action = app.submit_input();

        assert!(matches!(action, Some(AppAction::CompactSession)));
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::AssistantText(text)) if text == "Compacting context..."
        ));
        assert!(matches!(app.phase, AgentPhase::Thinking));
    }

    #[test]
    fn autocomplete_slash_command_completes_unique_match() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/comp");

        assert!(app.autocomplete_slash_command());
        assert_eq!(app.input.content(), "/compact ");
    }

    #[test]
    fn autocomplete_slash_command_does_not_complete_ambiguous_match() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/p");

        assert!(!app.autocomplete_slash_command());
        assert_eq!(app.input.content(), "/p");
    }

    #[test]
    fn autocomplete_slash_command_completes_subcommand() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/ps t");

        assert!(app.autocomplete_slash_command());
        assert_eq!(app.input.content(), "/ps tree ");
    }

    #[test]
    fn finalize_slash_command_selection_uses_highlighted_option() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/");
        app.refresh_slash_command_options();
        app.option_cursor_down();
        app.preview_slash_command_option();

        assert!(app.finalize_slash_command_selection());
        assert_eq!(app.input.content(), "/ps tree ");
    }

    #[test]
    fn accept_slash_command_option_applies_selected_command() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/");
        app.refresh_slash_command_options();
        app.option_cursor_down();

        assert!(app.accept_slash_command_option());
        assert_eq!(app.input.content(), "/ps tree ");
        assert!(!app.slash_select_active);
    }

    #[test]
    fn preview_slash_command_option_shows_selected_command() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/");
        app.refresh_slash_command_options();
        app.option_cursor_down();

        assert!(app.preview_slash_command_option());
        assert_eq!(app.input.content(), "/ps tree");
        assert!(app.slash_select_active);
    }

    #[test]
    fn submit_input_ps_command_lists_sessions() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/ps");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ListSessions { tree: false })
        ));
        assert!(app.messages.is_empty());
    }

    #[test]
    fn submit_input_ps_tree_command_lists_tree_sessions() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/ps tree");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ListSessions { tree: true })
        ));
        assert!(app.messages.is_empty());
    }

    #[test]
    fn input_label_shows_default_prompt() {
        let app = App::new("test".into(), false, None);
        assert_eq!(app.input_label(), "> ");
    }

    #[test]
    fn input_label_shows_plan_prompt_in_plan_mode() {
        let app = App::new("test".into(), true, None);
        assert_eq!(app.input_label(), "[plan] > ");
    }

    #[test]
    fn slash_command_hint_hidden_without_slash_prefix() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("hello");
        assert_eq!(app.slash_command_hint(), None);
    }

    #[test]
    fn slash_command_hint_shows_available_commands_for_slash_prefix() {
        let mut app = App::new("test".into(), false, None);
        app.loaded_skill_commands = vec!["review".to_string(), "ship-it".to_string()];
        app.input.set_from_string("/");
        assert_eq!(
            app.slash_command_hint(),
            Some(vec![
                ("/ps".to_string(), "list sessions"),
                ("/ps tree".to_string(), "show session tree"),
                ("/plan".to_string(), "toggle plan mode"),
                ("/loop".to_string(), "run autonomous loop"),
                ("/compact".to_string(), "compact current session"),
                ("/context".to_string(), "show context details"),
                ("/clear".to_string(), "start a fresh session"),
                ("/switch".to_string(), "switch to another session"),
                ("/review".to_string(), "run a skill command"),
                ("/ship-it".to_string(), "run a skill command"),
            ])
        );
    }

    #[test]
    fn slash_command_hint_does_not_include_placeholder_skill_entry() {
        let mut app = App::new("test".into(), false, None);
        app.loaded_skill_commands = vec!["review".to_string()];
        app.input.set_from_string("/");

        let hints = app.slash_command_hint().expect("slash hints");

        assert!(hints.iter().any(|(command, _)| command == "/review"));
        assert!(!hints.iter().any(|(command, _)| command == "/<skill>"));
    }

    #[test]
    fn slash_command_hint_filters_by_prefix() {
        let mut app = App::new("test".into(), false, None);
        app.loaded_skill_commands = vec!["plan-review".to_string(), "ps-audit".to_string()];
        app.input.set_from_string("/p");
        assert_eq!(
            app.slash_command_hint(),
            Some(vec![
                ("/ps".to_string(), "list sessions"),
                ("/ps tree".to_string(), "show session tree"),
                ("/plan".to_string(), "toggle plan mode"),
                ("/plan-review".to_string(), "run a skill command"),
                ("/ps-audit".to_string(), "run a skill command"),
            ])
        );
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
        assert!(app.messages.is_empty());
        assert!(matches!(app.phase, AgentPhase::Thinking));
    }

    #[test]
    fn submit_input_skill_command_in_plan_mode_requests_confirmation() {
        let mut app = App::new("test".into(), true, None);
        app.input.set_from_string("/review audit this");

        let action = app.submit_input();

        assert!(action.is_none());
        assert!(matches!(
            app.pending_plan_exit,
            Some(PendingPlanExit::StartSkillSession {
                ref skill_name,
                ref request
            }) if skill_name == "review" && request == "audit this"
        ));
        assert!(matches!(
            app.messages.first(),
            Some(ConversationEntry::User(text)) if text == "/review audit this"
        ));
        assert!(matches!(
            app.messages.get(1),
            Some(ConversationEntry::InteractionQuestion { prompt, options })
                if prompt.contains("Leave plan mode and start /review")
                && options == &vec!["Yes".to_string(), "No".to_string()]
        ));
        assert_eq!(app.input_label(), "[plan] [confirm-exit] (y/n) > ");
        assert!(matches!(app.phase, AgentPhase::Idle));
    }

    #[test]
    fn submit_input_yes_confirms_pending_final_plan_exit() {
        let mut app = App::new("test".into(), true, None);
        app.request_plan_exit_confirmation(PendingPlanExit::FinalPlan {
            final_plan: "Final plan".into(),
        });
        app.input.set_from_string("yes");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ExitPlanMode { final_plan }) if final_plan == "Final plan"
        ));
        assert!(app.pending_plan_exit.is_none());
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::InteractionPrompt { summary: None, prompt }) if prompt == "yes"
        ));
    }

    #[test]
    fn submit_input_no_keeps_plan_mode_active() {
        let mut app = App::new("test".into(), true, None);
        app.request_plan_exit_confirmation(PendingPlanExit::FinalPlan {
            final_plan: "Final plan".into(),
        });
        app.input.set_from_string("no");

        let action = app.submit_input();

        assert!(action.is_none());
        assert!(app.pending_plan_exit.is_none());
        assert!(app.plan_mode);
        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::AssistantText(text)) if text == "Stayed in plan mode."
        ));
    }

    #[test]
    fn request_plan_exit_confirmation_records_prompt() {
        let mut app = App::new("test".into(), true, None);

        app.request_plan_exit_confirmation(PendingPlanExit::FinalPlan {
            final_plan: "Final plan".into(),
        });

        assert!(matches!(
            app.messages.last(),
            Some(ConversationEntry::InteractionQuestion { prompt, options })
                if prompt.contains("start a normal session with this final plan")
                && options == &vec!["Yes".to_string(), "No".to_string()]
        ));
        assert!(matches!(
            app.pending_plan_exit,
            Some(PendingPlanExit::FinalPlan { ref final_plan }) if final_plan == "Final plan"
        ));
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
        assert!(app.messages.is_empty());
        assert!(matches!(app.phase, AgentPhase::Thinking));
    }

    #[test]
    fn submit_input_loop_every_schedules_immediate_first_run() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/loop every 5m check logs");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ScheduleLoop { request, delay, cadence })
                if request == "check logs"
                    && delay == Duration::ZERO
                    && cadence == Some(Duration::from_secs(300))
        ));
    }

    #[test]
    fn submit_input_loop_in_preserves_requested_delay() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/loop in 5m check logs");

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::ScheduleLoop { request, delay, cadence })
                if request == "check logs"
                    && delay == Duration::from_secs(300)
                    && cadence.is_none()
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

    #[test]
    fn submit_input_switch_command_uses_typed_target_even_with_switch_options_active() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/switch alpha");
        app.set_switch_session_candidates(vec![
            SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: Some("Alpha summary".into()),
            },
            SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
        ]);

        let action = app.submit_input();

        assert!(matches!(
            action,
            Some(AppAction::SwitchSession { session_id }) if session_id == "alpha"
        ));
        assert!(app.option_select.is_none());
        assert!(!app.switch_select_active);
    }

    #[test]
    fn switch_session_active_char_input_updates_filter_and_options() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/switch a");
        app.set_switch_session_candidates(vec![
            SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: Some("Alpha summary".into()),
            },
            SwitchSessionCandidate {
                session_id: "beta".into(),
                summary: None,
            },
            SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
        ]);

        app.input.insert_char('l');
        app.refresh_switch_session_options();

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
    fn switch_session_active_backspace_updates_filter_and_options() {
        let mut app = App::new("test".into(), false, None);
        app.input.set_from_string("/switch alp");
        app.set_switch_session_candidates(vec![
            SwitchSessionCandidate {
                session_id: "alpha".into(),
                summary: None,
            },
            SwitchSessionCandidate {
                session_id: "alpine".into(),
                summary: Some("Alpine summary".into()),
            },
            SwitchSessionCandidate {
                session_id: "beta".into(),
                summary: None,
            },
        ]);

        app.input.delete_char_before();
        app.refresh_switch_session_options();

        assert_eq!(app.input.content(), "/switch al");
        let options = app
            .option_select
            .as_ref()
            .map(|select| select.options.clone());
        assert_eq!(
            options,
            Some(vec!["alpha".into(), "alpine\tAlpine summary".into()])
        );
    }
}
