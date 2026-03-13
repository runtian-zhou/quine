use crate::interactive::entries_to_messages;
use crate::permissions::PermissionManager;
use quine_core::conversation::Entry;
use quine_core::log::ConversationLog;
use quine_llm::types::ChatMessage;

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

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
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
pub fn print_turn_usage(
    input: u64,
    output: u64,
    session: &SessionUsage,
) {
    println!(
        "\x1b[90m[{} in \u{2192} {} out | session: {} in \u{2192} {} out]\x1b[0m",
        format_tokens(input),
        format_tokens(output),
        format_tokens(session.total_input),
        format_tokens(session.total_output),
    );
}
