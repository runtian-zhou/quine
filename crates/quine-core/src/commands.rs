use crate::conversation::{entries_to_messages, Entry};
use crate::log::ConversationLog;
use crate::llm_types::ChatMessage;
use crate::permissions::PermissionManager;

/// Result of executing a slash command.
pub enum CommandResult {
    /// Continue the conversation loop normally.
    Continue,
    /// Exit the conversation.
    Exit,
    /// The conversation was rewound — messages were rebuilt from entries.
    Rewound,
    /// The command was not recognized.
    Unknown(String),
}

/// Session-level token usage accumulator.
#[derive(Default)]
pub struct SessionUsage {
    pub total_input: u64,
    pub total_output: u64,
    pub total_cache_creation: u64,
    pub total_cache_read: u64,
}

impl SessionUsage {
    pub fn add(&mut self, input: u64, output: u64, cache_creation: u64, cache_read: u64) {
        self.total_input += input;
        self.total_output += output;
        self.total_cache_creation += cache_creation;
        self.total_cache_read += cache_read;
    }
}

/// Try to handle a slash command. Returns None if the input is not a command.
pub fn handle_command(
    input: &str,
    conv_log: &mut ConversationLog,
    messages: &mut Vec<ChatMessage>,
    session_usage: &SessionUsage,
    permissions: &PermissionManager,
) -> Option<CommandResult> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }

    let parts: Vec<&str> = input.splitn(2, ' ').collect();
    let cmd = parts[0];
    let arg = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd {
        "/help" => {
            println!("\x1b[1;36mAvailable commands:\x1b[0m");
            println!("  \x1b[1m/help\x1b[0m           Show this help message");
            println!("  \x1b[1m/undo\x1b[0m           Remove the last user turn and its responses");
            println!("  \x1b[1m/rewind N\x1b[0m       Go back N user turns");
            println!("  \x1b[1m/clear\x1b[0m          Reset conversation to empty");
            println!("  \x1b[1m/tokens\x1b[0m         Show session token usage");
            println!("  \x1b[1m/permissions\x1b[0m    Show tool permission status");
            println!("  \x1b[1m/quit\x1b[0m, \x1b[1m/exit\x1b[0m   Exit the session");
            Some(CommandResult::Continue)
        }
        "/quit" | "/exit" => Some(CommandResult::Exit),
        "/undo" => {
            rewind(conv_log, messages, 1);
            Some(CommandResult::Rewound)
        }
        "/rewind" => {
            let n: usize = arg.parse().unwrap_or(1);
            if n == 0 {
                println!("\x1b[33mNothing to rewind.\x1b[0m");
                return Some(CommandResult::Continue);
            }
            rewind(conv_log, messages, n);
            Some(CommandResult::Rewound)
        }
        "/clear" => {
            conv_log.entries.clear();
            messages.clear();
            println!("\x1b[33mConversation cleared.\x1b[0m");
            Some(CommandResult::Rewound)
        }
        "/tokens" => {
            print_token_usage(session_usage);
            Some(CommandResult::Continue)
        }
        "/permissions" => {
            permissions.print_status();
            Some(CommandResult::Continue)
        }
        _ => Some(CommandResult::Unknown(cmd.to_string())),
    }
}

/// Rewind the conversation by N user turns.
fn rewind(conv_log: &mut ConversationLog, messages: &mut Vec<ChatMessage>, n: usize) {
    // Find the Nth-from-last UserMessage index.
    let user_indices: Vec<usize> = conv_log
        .entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| matches!(e, Entry::UserMessage { .. }).then_some(i))
        .collect();

    if user_indices.is_empty() || n > user_indices.len() {
        println!("\x1b[33mNothing to rewind.\x1b[0m");
        return;
    }

    let target = user_indices[user_indices.len() - n];
    conv_log.entries.truncate(target);
    *messages = entries_to_messages(&conv_log.entries);
    println!(
        "\x1b[33mRewound {} turn{}. Conversation now has {} entries.\x1b[0m",
        n,
        if n == 1 { "" } else { "s" },
        conv_log.entries.len()
    );
}

pub fn format_tokens(n: u64) -> String {
    if n >= 999_950 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn print_token_usage(usage: &SessionUsage) {
    println!("\x1b[1;36mSession token usage:\x1b[0m");
    println!("  Input:          {}", format_tokens(usage.total_input));
    println!("  Output:         {}", format_tokens(usage.total_output));
    if usage.total_cache_creation > 0 || usage.total_cache_read > 0 {
        println!(
            "  Cache created:  {}",
            format_tokens(usage.total_cache_creation)
        );
        println!(
            "  Cache read:     {}",
            format_tokens(usage.total_cache_read)
        );
    }
}

/// Print a one-line usage summary after each turn.
pub fn print_turn_usage(input: u64, output: u64, session: &SessionUsage) {
    println!(
        "\x1b[90m[{} in \u{2192} {} out | session: {} in \u{2192} {} out]\x1b[0m",
        format_tokens(input),
        format_tokens(output),
        format_tokens(session.total_input),
        format_tokens(session.total_output),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::conversation::{entries_to_messages, Entry, ToolCall, ToolOutput};

    fn make_conv_log() -> ConversationLog {
        ConversationLog::new(
            "test-model".to_string(),
            "test-provider".to_string(),
            "system prompt".to_string(),
        )
    }

    // --- format_tokens ---

    #[test]
    fn format_tokens_small_numbers() {
        assert_eq!(format_tokens(0), "0", "zero should format as '0'");
        assert_eq!(format_tokens(1), "1", "single digit should format as-is");
        assert_eq!(
            format_tokens(999),
            "999",
            "just under 1K should format as-is"
        );
    }

    #[test]
    fn format_tokens_thousands() {
        assert_eq!(format_tokens(1_000), "1.0K", "1000 should format as '1.0K'");
        assert_eq!(format_tokens(1_500), "1.5K", "1500 should format as '1.5K'");
        assert_eq!(
            format_tokens(12_345),
            "12.3K",
            "12345 should format as '12.3K'"
        );
        assert_eq!(
            format_tokens(999_999),
            "1.0M",
            "999999 should round up to 1.0M"
        );
    }

    #[test]
    fn format_tokens_millions() {
        assert_eq!(
            format_tokens(1_000_000),
            "1.0M",
            "1M should format as '1.0M'"
        );
        assert_eq!(
            format_tokens(2_500_000),
            "2.5M",
            "2.5M should format as '2.5M'"
        );
    }

    // --- SessionUsage ---

    #[test]
    fn session_usage_default_is_zero() {
        let usage = SessionUsage::default();
        assert_eq!(usage.total_input, 0, "default input should be 0");
        assert_eq!(usage.total_output, 0, "default output should be 0");
        assert_eq!(
            usage.total_cache_creation, 0,
            "default cache creation should be 0"
        );
        assert_eq!(usage.total_cache_read, 0, "default cache read should be 0");
    }

    #[test]
    fn session_usage_add_accumulates() {
        let mut usage = SessionUsage::default();
        usage.add(100, 50, 10, 20);
        assert_eq!(usage.total_input, 100, "first add: input should be 100");
        assert_eq!(usage.total_output, 50, "first add: output should be 50");

        usage.add(200, 100, 5, 15);
        assert_eq!(usage.total_input, 300, "second add: input should be 300");
        assert_eq!(usage.total_output, 150, "second add: output should be 150");
        assert_eq!(
            usage.total_cache_creation, 15,
            "cache creation should be 15"
        );
        assert_eq!(usage.total_cache_read, 35, "cache read should be 35");
    }

    // --- handle_command ---

    #[test]
    fn handle_command_returns_none_for_non_commands() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("hello world", &mut log, &mut messages, &usage, &perms);
        assert!(result.is_none(), "non-slash input should return None");
    }

    #[test]
    fn handle_command_quit() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/quit", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Exit)),
            "/quit should return Exit"
        );
    }

    #[test]
    fn handle_command_exit() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/exit", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Exit)),
            "/exit should return Exit"
        );
    }

    #[test]
    fn handle_command_help_returns_continue() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/help", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Continue)),
            "/help should return Continue"
        );
    }

    #[test]
    fn handle_command_tokens_returns_continue() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/tokens", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Continue)),
            "/tokens should return Continue"
        );
    }

    #[test]
    fn handle_command_unknown_returns_unknown() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/foobar", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Unknown(ref s)) if s == "/foobar"),
            "/foobar should return Unknown('/foobar')"
        );
    }

    #[test]
    fn handle_command_clear_empties_conversation() {
        let mut log = make_conv_log();
        log.push(Entry::UserMessage {
            content: "hello".to_string(),
        });
        log.push(Entry::AssistantMessage {
            content: "hi".to_string(),
            tool_calls: vec![],
        });
        let mut messages = entries_to_messages(&log.entries);
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        assert_eq!(log.entries.len(), 2, "should have 2 entries before clear");
        assert_eq!(messages.len(), 2, "should have 2 messages before clear");

        let result = handle_command("/clear", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Rewound)),
            "/clear should return Rewound"
        );
        assert_eq!(log.entries.len(), 0, "entries should be empty after clear");
        assert_eq!(messages.len(), 0, "messages should be empty after clear");
    }

    #[test]
    fn handle_command_undo_removes_last_turn() {
        let mut log = make_conv_log();
        log.push(Entry::UserMessage {
            content: "first".to_string(),
        });
        log.push(Entry::AssistantMessage {
            content: "response1".to_string(),
            tool_calls: vec![],
        });
        log.push(Entry::UserMessage {
            content: "second".to_string(),
        });
        log.push(Entry::AssistantMessage {
            content: "response2".to_string(),
            tool_calls: vec![],
        });
        let mut messages = entries_to_messages(&log.entries);
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        assert_eq!(log.entries.len(), 4, "should have 4 entries before undo");

        let result = handle_command("/undo", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Rewound)),
            "/undo should return Rewound"
        );
        assert_eq!(
            log.entries.len(),
            2,
            "should have 2 entries after undo (first turn remains)"
        );
        // Verify the remaining entries are the first turn
        assert!(
            matches!(&log.entries[0], Entry::UserMessage { content } if content == "first"),
            "first entry should be the first user message"
        );
    }

    #[test]
    fn handle_command_rewind_multiple_turns() {
        let mut log = make_conv_log();
        for i in 0..3 {
            log.push(Entry::UserMessage {
                content: format!("msg{}", i),
            });
            log.push(Entry::AssistantMessage {
                content: format!("resp{}", i),
                tool_calls: vec![],
            });
        }
        let mut messages = entries_to_messages(&log.entries);
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        assert_eq!(log.entries.len(), 6, "should have 6 entries before rewind");

        let result = handle_command("/rewind 2", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Rewound)),
            "/rewind should return Rewound"
        );
        assert_eq!(
            log.entries.len(),
            2,
            "should have 2 entries after rewinding 2 turns (first turn remains)"
        );
    }

    #[test]
    fn handle_command_rewind_zero_does_nothing() {
        let mut log = make_conv_log();
        log.push(Entry::UserMessage {
            content: "test".to_string(),
        });
        let mut messages = entries_to_messages(&log.entries);
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/rewind 0", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Continue)),
            "/rewind 0 should return Continue"
        );
        assert_eq!(
            log.entries.len(),
            1,
            "entries should be unchanged after /rewind 0"
        );
    }

    #[test]
    fn handle_command_undo_on_empty_does_nothing() {
        let mut log = make_conv_log();
        let mut messages = vec![];
        let usage = SessionUsage::default();
        let perms = PermissionManager::new();

        let result = handle_command("/undo", &mut log, &mut messages, &usage, &perms);
        assert!(
            matches!(result, Some(CommandResult::Rewound)),
            "/undo on empty should still return Rewound"
        );
        assert_eq!(log.entries.len(), 0, "entries should remain empty");
    }

    // --- entries_to_messages ---

    #[test]
    fn entries_to_messages_empty() {
        let messages = entries_to_messages(&[]);
        assert_eq!(
            messages.len(),
            0,
            "empty entries should produce empty messages"
        );
    }

    #[test]
    fn entries_to_messages_user_and_assistant() {
        let entries = vec![
            Entry::UserMessage {
                content: "hello".to_string(),
            },
            Entry::AssistantMessage {
                content: "world".to_string(),
                tool_calls: vec![],
            },
        ];
        let messages = entries_to_messages(&entries);
        assert_eq!(messages.len(), 2, "should produce 2 messages");
        assert_eq!(messages[0].role, "user", "first message should be user");
        assert_eq!(
            messages[1].role, "assistant",
            "second message should be assistant"
        );
        assert_eq!(
            messages[0].content.as_text(),
            "hello",
            "user content should be 'hello'"
        );
        assert_eq!(
            messages[1].content.as_text(),
            "world",
            "assistant content should be 'world'"
        );
    }

    #[test]
    fn entries_to_messages_groups_tool_results() {
        let entries = vec![
            Entry::UserMessage {
                content: "do something".to_string(),
            },
            Entry::AssistantMessage {
                content: "".to_string(),
                tool_calls: vec![
                    ToolCall {
                        id: "tc1".to_string(),
                        name: "Read".to_string(),
                        arguments: serde_json::json!({"file_path": "test.txt"}),
                    },
                    ToolCall {
                        id: "tc2".to_string(),
                        name: "Read".to_string(),
                        arguments: serde_json::json!({"file_path": "other.txt"}),
                    },
                ],
            },
            Entry::ToolExecution {
                tool_call_id: "tc1".to_string(),
                tool_name: "Read".to_string(),
                arguments: serde_json::json!({}),
                result: ToolOutput {
                    success: true,
                    output: "file1 content".to_string(),
                },
            },
            Entry::ToolExecution {
                tool_call_id: "tc2".to_string(),
                tool_name: "Read".to_string(),
                arguments: serde_json::json!({}),
                result: ToolOutput {
                    success: true,
                    output: "file2 content".to_string(),
                },
            },
        ];
        let messages = entries_to_messages(&entries);
        // user message, assistant message with tool_use blocks, user message with tool_result blocks
        assert_eq!(
            messages.len(),
            3,
            "should produce 3 messages: user, assistant, tool_results"
        );
        assert_eq!(messages[0].role, "user", "first should be user");
        assert_eq!(messages[1].role, "assistant", "second should be assistant");
        assert_eq!(
            messages[2].role, "user",
            "third should be user (tool results)"
        );
    }

    // --- Permission tests ---

    #[test]
    fn risk_level_bash_is_high() {
        assert_eq!(
            PermissionManager::risk_level("Bash"),
            crate::permissions::RiskLevel::High,
            "Bash should have High risk level"
        );
    }

    #[test]
    fn risk_level_write_is_medium() {
        assert_eq!(
            PermissionManager::risk_level("Write"),
            crate::permissions::RiskLevel::Medium,
            "Write should have Medium risk level"
        );
    }

    #[test]
    fn risk_level_edit_is_medium() {
        assert_eq!(
            PermissionManager::risk_level("Edit"),
            crate::permissions::RiskLevel::Medium,
            "Edit should have Medium risk level"
        );
    }

    #[test]
    fn risk_level_read_is_low() {
        assert_eq!(
            PermissionManager::risk_level("Read"),
            crate::permissions::RiskLevel::Low,
            "Read should have Low risk level"
        );
    }

    #[test]
    fn risk_level_glob_is_low() {
        assert_eq!(
            PermissionManager::risk_level("Glob"),
            crate::permissions::RiskLevel::Low,
            "Glob should have Low risk level"
        );
    }

    #[test]
    fn risk_level_unknown_tool_is_low() {
        assert_eq!(
            PermissionManager::risk_level("SomeUnknownTool"),
            crate::permissions::RiskLevel::Low,
            "Unknown tools should default to Low risk level"
        );
    }

    #[test]
    fn permission_manager_low_risk_auto_approves() {
        let mut pm = PermissionManager::new();
        assert!(
            pm.check("Read", "test.txt"),
            "Low risk tools should be auto-approved without prompting"
        );
    }

    #[test]
    fn permission_manager_low_risk_auto_approves_multiple_tools() {
        let mut pm = PermissionManager::new();
        assert!(pm.check("Read", "a.txt"), "Read should auto-approve");
        assert!(pm.check("Glob", "*.rs"), "Glob should auto-approve");
        assert!(pm.check("Grep", "pattern"), "Grep should auto-approve");
        assert!(
            pm.check("ListDirectory", "."),
            "ListDirectory should auto-approve"
        );
        assert!(pm.check("Todo", "list"), "Todo should auto-approve");
    }
}
