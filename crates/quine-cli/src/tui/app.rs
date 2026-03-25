use std::collections::{HashSet, VecDeque};

use quine_harness::protocol::{notifications, JsonRpcNotification};

/// Spinner braille frames for the waiting animation.
const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// A single entry in the conversation view.
#[derive(Debug, Clone)]
pub enum ConversationEntry {
    User(String),
    AssistantText(String),
    ToolCall { tool_name: String, summary: String },
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
}

/// A queued interaction request from the daemon.
#[derive(Debug, Clone)]
pub struct PendingInteraction {
    pub prompt: String,
    pub kind: InteractionKind,
}

/// Actions the event loop should perform after handling an event.
pub enum AppAction {
    SendMessage(String),
    SubmitInteraction(String),
    Quit,
}

/// The main application state.
pub struct App {
    pub messages: Vec<ConversationEntry>,
    pub streaming_buffer: String,
    pub scroll_offset: u16,
    pub user_scrolled: bool,
    pub input: String,
    pub cursor_pos: usize,
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
}

impl App {
    pub fn new(session_id: String) -> Self {
        Self {
            messages: Vec::new(),
            streaming_buffer: String::new(),
            scroll_offset: 0,
            user_scrolled: false,
            input: String::new(),
            cursor_pos: 0,
            interaction_queue: VecDeque::new(),
            phase: AgentPhase::Idle,
            spinner_frame: 0,
            should_quit: false,
            session_id,
            expanded_tools: HashSet::new(),
            input_history: Vec::new(),
            history_index: None,
            saved_input: String::new(),
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
                    format!("[permission]{badge} {} (y/n) > ", interaction.prompt)
                }
                InteractionKind::AskUser => {
                    format!("[ask_user]{badge} {} > ", interaction.prompt)
                }
            }
        } else {
            "> ".to_string()
        }
    }

    /// Handle Enter key: send message or submit interaction response.
    pub fn submit_input(&mut self) -> Option<AppAction> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor_pos = 0;
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

    /// Navigate to the previous input in history (Up arrow).
    pub fn history_prev(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        match self.history_index {
            None => {
                // Enter history mode — save current input.
                self.saved_input = self.input.clone();
                let idx = self.input_history.len() - 1;
                self.history_index = Some(idx);
                self.input = self.input_history[idx].clone();
                self.cursor_pos = self.input.len();
            }
            Some(idx) if idx > 0 => {
                let idx = idx - 1;
                self.history_index = Some(idx);
                self.input = self.input_history[idx].clone();
                self.cursor_pos = self.input.len();
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
                self.input = self.input_history[idx].clone();
                self.cursor_pos = self.input.len();
            } else {
                // Past the end — restore saved input.
                self.history_index = None;
                self.input = std::mem::take(&mut self.saved_input);
                self.cursor_pos = self.input.len();
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
                    self.messages
                        .push(ConversationEntry::ToolCall { tool_name, summary });
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
                let kind = if kind_str == "Confirmation" {
                    InteractionKind::Permission
                } else {
                    InteractionKind::AskUser
                };
                // Show permission prompts as a highlighted entry in conversation.
                if kind == InteractionKind::Permission {
                    self.messages.push(ConversationEntry::Error(format!(
                        "⚠ Permission requested: {prompt}"
                    )));
                    self.auto_scroll();
                }
                self.interaction_queue
                    .push_back(PendingInteraction { prompt, kind });
            }
            _ => {}
        }
    }

    /// Insert a character at the cursor position.
    pub fn insert_char(&mut self, c: char) {
        self.input.insert(self.cursor_pos, c);
        self.cursor_pos += c.len_utf8();
    }

    /// Delete the character before the cursor (backspace).
    pub fn delete_char_before(&mut self) {
        if self.cursor_pos > 0 {
            let prev = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.input.drain(prev..self.cursor_pos);
            self.cursor_pos = prev;
        }
    }

    /// Move cursor left.
    pub fn cursor_left(&mut self) {
        if self.cursor_pos > 0 {
            self.cursor_pos = self.input[..self.cursor_pos]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
        }
    }

    /// Move cursor right.
    pub fn cursor_right(&mut self) {
        if self.cursor_pos < self.input.len() {
            self.cursor_pos = self.input[self.cursor_pos..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor_pos + i)
                .unwrap_or(self.input.len());
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
        app.input = "hello".into();
        app.cursor_pos = 5;

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
        });
        app.input = "Alice".into();
        app.cursor_pos = 5;

        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "Alice"));
        assert!(app.interaction_queue.is_empty());
    }

    #[test]
    fn submit_empty_input_does_nothing() {
        let mut app = App::new("test-session".into());
        app.input = "   ".into();
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
        });
        assert_eq!(app.input_label(), "[ask_user] Name? > ");
    }

    #[test]
    fn input_label_with_multiple_interactions() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Q1?".into(),
            kind: InteractionKind::AskUser,
        });
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Q2?".into(),
            kind: InteractionKind::AskUser,
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
        app.insert_char('h');
        app.insert_char('i');
        assert_eq!(app.input, "hi");
        assert_eq!(app.cursor_pos, 2);
        app.delete_char_before();
        assert_eq!(app.input, "h");
        assert_eq!(app.cursor_pos, 1);
    }

    #[test]
    fn history_navigation() {
        let mut app = App::new("s".into());
        app.input_history.push("first".into());
        app.input_history.push("second".into());
        app.input = "current".into();

        // Up → most recent history entry.
        app.history_prev();
        assert_eq!(app.input, "second");
        // Up again → older entry.
        app.history_prev();
        assert_eq!(app.input, "first");
        // Up at the top → stays.
        app.history_prev();
        assert_eq!(app.input, "first");
        // Down → back to second.
        app.history_next();
        assert_eq!(app.input, "second");
        // Down → restores saved input.
        app.history_next();
        assert_eq!(app.input, "current");
        // Down again → no-op.
        app.history_next();
        assert_eq!(app.input, "current");
    }

    #[test]
    fn history_empty_does_nothing() {
        let mut app = App::new("s".into());
        app.input = "hello".into();
        app.history_prev();
        assert_eq!(app.input, "hello");
    }

    #[test]
    fn submit_pushes_to_history() {
        let mut app = App::new("s".into());
        app.input = "hello".into();
        app.cursor_pos = 5;
        app.submit_input();
        assert_eq!(app.input_history, vec!["hello"]);
    }

    #[test]
    fn permission_label() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow bash: rm -rf /tmp/foo?".into(),
            kind: InteractionKind::Permission,
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
        });
        app.input = "y".into();
        app.cursor_pos = 1;
        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "approved"));
    }

    #[test]
    fn permission_n_shorthand() {
        let mut app = App::new("s".into());
        app.interaction_queue.push_back(PendingInteraction {
            prompt: "Allow?".into(),
            kind: InteractionKind::Permission,
        });
        app.input = "n".into();
        app.cursor_pos = 1;
        let action = app.submit_input();
        assert!(matches!(action, Some(AppAction::SubmitInteraction(r)) if r == "denied"));
    }
}
