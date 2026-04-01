#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SlashCommand {
    BuiltIn { name: String, arguments: String },
    Skill { name: String, arguments: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryScopeArgument {
    User,
    Project,
    Session,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MemoryCommand {
    List {
        scope: MemoryScopeArgument,
    },
    Add {
        scope: MemoryScopeArgument,
        title: String,
        body: String,
    },
    Delete {
        scope: MemoryScopeArgument,
        id: String,
    },
}

pub(crate) fn parse_memory_command(arguments: &str) -> Result<MemoryCommand, String> {
    let trimmed = arguments.trim();
    let mut parts = trimmed.splitn(3, char::is_whitespace);
    let subcommand = parts
        .next()
        .ok_or_else(|| "Usage: /memory <list|add|delete> ...".to_string())?;
    let scope_text = parts
        .next()
        .ok_or_else(|| "Usage: /memory <list|add|delete> <user|project|session> ...".to_string())?;
    let rest = parts.next().unwrap_or("").trim();
    let scope = parse_memory_scope(scope_text)?;

    match subcommand {
        "list" => {
            if !rest.is_empty() {
                Err("Usage: /memory list <user|project|session>".to_string())
            } else {
                Ok(MemoryCommand::List { scope })
            }
        }
        "add" => {
            let (title, body) = rest.split_once(':').ok_or_else(|| {
                "Usage: /memory add <user|project|session> <title>: <body>".to_string()
            })?;
            let title = title.trim();
            let body = body.trim();
            if title.is_empty() || body.is_empty() {
                Err("Memory title and body must be non-empty".to_string())
            } else {
                Ok(MemoryCommand::Add {
                    scope,
                    title: title.to_string(),
                    body: body.to_string(),
                })
            }
        }
        "delete" => {
            if rest.is_empty() {
                Err("Usage: /memory delete <user|project|session> <id>".to_string())
            } else {
                Ok(MemoryCommand::Delete {
                    scope,
                    id: rest.to_string(),
                })
            }
        }
        _ => Err("Usage: /memory <list|add|delete> ...".to_string()),
    }
}

fn parse_memory_scope(scope: &str) -> Result<MemoryScopeArgument, String> {
    match scope {
        "user" => Ok(MemoryScopeArgument::User),
        "project" => Ok(MemoryScopeArgument::Project),
        "session" => Ok(MemoryScopeArgument::Session),
        _ => Err("Memory scope must be one of: user, project, session".to_string()),
    }
}

pub(crate) fn parse_slash_command(input: &str) -> Option<SlashCommand> {
    let trimmed = input.trim();
    let command = trimmed.strip_prefix('/')?;
    let (name, arguments) = match command.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim()),
        None => (command, ""),
    };

    match name {
        "quit" | "plan" | "loop" | "compact" | "memory" => Some(SlashCommand::BuiltIn {
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
    fn parses_memory_command() {
        assert_eq!(
            parse_slash_command("/memory list project"),
            Some(SlashCommand::BuiltIn {
                name: "memory".into(),
                arguments: "list project".into(),
            })
        );
    }

    #[test]
    fn parses_memory_add_arguments() {
        assert_eq!(
            parse_memory_command("add user Rust style: Prefer anyhow"),
            Ok(MemoryCommand::Add {
                scope: MemoryScopeArgument::User,
                title: "Rust style".into(),
                body: "Prefer anyhow".into(),
            })
        );
    }

    #[test]
    fn parses_memory_delete_arguments() {
        assert_eq!(
            parse_memory_command("delete session record-1"),
            Ok(MemoryCommand::Delete {
                scope: MemoryScopeArgument::Session,
                id: "record-1".into(),
            })
        );
    }

    #[test]
    fn rejects_invalid_memory_scope() {
        assert!(parse_memory_command("list team").is_err());
    }
}
