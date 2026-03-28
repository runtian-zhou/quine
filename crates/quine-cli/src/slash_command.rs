#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SlashCommand {
    pub(crate) name: String,
    pub(crate) arguments: String,
}

pub(crate) fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    let command = trimmed.strip_prefix('/')?;
    let (name, arguments) = match command.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };

    Some(SlashCommand {
        name: name.to_string(),
        arguments: arguments.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plan_with_arguments() {
        assert_eq!(
            parse_slash_command("/plan do thing"),
            Some(SlashCommand {
                name: "plan".into(),
                arguments: "do thing".into(),
            })
        );
    }

    #[test]
    fn parses_plan_without_arguments() {
        assert_eq!(
            parse_slash_command("/plan"),
            Some(SlashCommand {
                name: "plan".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_quit_command() {
        assert_eq!(
            parse_slash_command("/quit"),
            Some(SlashCommand {
                name: "quit".into(),
                arguments: String::new(),
            })
        );
    }

    #[test]
    fn parses_unknown_command_for_local_rejection() {
        assert_eq!(
            parse_slash_command("/does-not-exist now"),
            Some(SlashCommand {
                name: "does-not-exist".into(),
                arguments: "now".into(),
            })
        );
    }

    #[test]
    fn ignores_non_command_input() {
        assert_eq!(parse_slash_command("hello"), None);
        assert_eq!(parse_slash_command("   hello"), None);
        assert_eq!(parse_slash_command("   "), None);
    }
}
