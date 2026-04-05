use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandRisk {
    ReadOnly,
    Mutating,
    NestedShell,
    Interpreter,
    Unknown,
}

impl CommandRisk {
    pub(crate) fn reason_label(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-oriented",
            Self::Mutating => "mutating",
            Self::NestedShell => "nested-shell",
            Self::Interpreter => "interpreter-launch",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDescriptor {
    pub command: String,
    pub program: Option<String>,
    #[serde(default)]
    pub argv: Vec<String>,
    pub risk: CommandRisk,
}

pub(crate) fn analyze_command(command: &str) -> CommandDescriptor {
    let tokens = tokenize(command);
    let effective_tokens = skip_env_wrappers(&tokens);
    let program = effective_tokens.first().cloned();
    let argv = effective_tokens.iter().skip(1).cloned().collect::<Vec<_>>();
    let risk = classify_risk(command, &effective_tokens);

    CommandDescriptor {
        command: command.to_string(),
        program,
        argv,
        risk,
    }
}

fn tokenize(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(std::string::ToString::to_string)
        .collect()
}

fn skip_env_wrappers(tokens: &[String]) -> Vec<String> {
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if token == "env" {
            index += 1;
            while let Some(next) = tokens.get(index) {
                if next.starts_with('-') || looks_like_assignment(next) {
                    index += 1;
                    continue;
                }
                break;
            }
            continue;
        }

        if looks_like_assignment(token) {
            index += 1;
            continue;
        }

        break;
    }

    tokens[index..].to_vec()
}

fn looks_like_assignment(token: &str) -> bool {
    token.contains('=') && !token.starts_with('=')
}

fn classify_risk(command: &str, tokens: &[String]) -> CommandRisk {
    let Some(program) = tokens.first().map(|token| token.as_str()) else {
        return CommandRisk::Unknown;
    };

    if is_nested_shell(program, &tokens[1..]) {
        return CommandRisk::NestedShell;
    }

    if is_interpreter_launcher(program, &tokens[1..]) {
        return CommandRisk::Interpreter;
    }

    if is_mutating_command(command, program) {
        return CommandRisk::Mutating;
    }

    if is_read_only_command(program, tokens.get(1).map(String::as_str)) {
        return CommandRisk::ReadOnly;
    }

    CommandRisk::Unknown
}

fn is_nested_shell(program: &str, argv: &[String]) -> bool {
    const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish"];
    const SHELL_EXEC_FLAGS: &[&str] = &["-c", "-lc", "-ic", "-xc", "-xec"];

    SHELLS.contains(&program)
        && argv
            .iter()
            .any(|arg| SHELL_EXEC_FLAGS.contains(&arg.as_str()))
}

fn is_interpreter_launcher(program: &str, argv: &[String]) -> bool {
    const INTERPRETERS: &[&str] = &["python", "python3", "perl", "ruby", "node", "php", "lua"];
    const INLINE_FLAGS: &[&str] = &["-c", "-e", "-p"];

    INTERPRETERS.contains(&program) && argv.iter().any(|arg| INLINE_FLAGS.contains(&arg.as_str()))
}

fn is_mutating_command(command: &str, program: &str) -> bool {
    const MUTATING_PROGRAMS: &[&str] = &[
        "touch", "mkdir", "rm", "rmdir", "mv", "cp", "install", "tee", "chmod", "chown", "truncate",
    ];

    MUTATING_PROGRAMS.contains(&program)
        || command.contains(" >")
        || command.contains(">>")
        || command.contains(">|")
        || command.contains("<<")
        || command.contains("| tee")
}

fn is_read_only_command(program: &str, subcommand: Option<&str>) -> bool {
    const READ_ONLY_PROGRAMS: &[&str] = &[
        "pwd", "ls", "cat", "head", "tail", "grep", "rg", "find", "wc", "stat", "file", "realpath",
        "which",
    ];
    const READ_ONLY_GIT_SUBCOMMANDS: &[&str] = &["status", "diff", "show", "log", "branch"];

    READ_ONLY_PROGRAMS.contains(&program)
        || (program == "git"
            && subcommand.is_some_and(|candidate| READ_ONLY_GIT_SUBCOMMANDS.contains(&candidate)))
}
