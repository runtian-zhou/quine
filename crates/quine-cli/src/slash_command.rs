#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    BuiltIn { name: String, arguments: String },
    Skill { name: String, arguments: String },
}

pub(crate) fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    let command = trimmed.strip_prefix('/')?;
    let (name, arguments) = match command.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };

    match name {
        "quit" | "plan" | "loop" | "compact" | "context" | "ps" => Some(SlashCommand::BuiltIn {
            name: name.to_string(),
            arguments: arguments.to_string(),
        }),
        _ if !name.is_empty() => Some(SlashCommand::Skill {
            name: name.to_string(),
            arguments: arguments.to_string(),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_with_arguments() {
        assert_eq!(
            parse_slash_command("/plan do thing"),
            Some(SlashCommand::BuiltIn {
                name: "plan".into(),
                arguments: "do thing".into(),
            })
        );
    }

    #[test]
    fn parses_plan_without_arguments() {
        assert_eq!(
            parse_slash_command("/plan"),
            Some(SlashCommand::BuiltIn {
                name: "plan".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_quit_command() {
        assert_eq!(
            parse_slash_command("/quit"),
            Some(SlashCommand::BuiltIn {
                name: "quit".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_skill_command_for_tui_dispatch() {
        assert_eq!(
            parse_slash_command("/review src/lib.rs"),
            Some(SlashCommand::Skill {
                name: "review".into(),
                arguments: "src/lib.rs".into(),
            })
        );
    }

    #[test]
    fn parses_loop_command() {
        assert_eq!(
            parse_slash_command("/loop every 5m check status"),
            Some(SlashCommand::BuiltIn {
                name: "loop".into(),
                arguments: "every 5m check status".into(),
            })
        );
    }

    #[test]
    fn parses_compact_command() {
        assert_eq!(
            parse_slash_command("/compact"),
            Some(SlashCommand::BuiltIn {
                name: "compact".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_context_command() {
        assert_eq!(
            parse_slash_command("/context"),
            Some(SlashCommand::BuiltIn {
                name: "context".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_ps_command() {
        assert_eq!(
            parse_slash_command("/ps tree"),
            Some(SlashCommand::BuiltIn {
                name: "ps".into(),
                arguments: "tree".into(),
            })
        );
    }

    #[test]
    fn ignores_non_command_input() {
        assert_eq!(parse_slash_command("hello"), None);
        assert_eq!(parse_slash_command("   hello"), None);
        assert_eq!(parse_slash_command("/"), None);
    }
}
