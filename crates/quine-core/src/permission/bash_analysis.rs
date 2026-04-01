use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BashCommandAnalysis {
    pub raw: String,
    pub normalized: String,
    pub base_command: Option<String>,
    pub subcommands: Vec<AnalyzedSubcommand>,
    pub has_pipe: bool,
    pub has_control_operator: bool,
    pub has_redirection: bool,
    pub has_heredoc: bool,
    pub has_command_substitution: bool,
    pub has_subshell: bool,
    pub changes_directory: bool,
    pub too_complex: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnalyzedSubcommand {
    pub raw: String,
    pub normalized: String,
    pub base_command: Option<String>,
}

const WRAPPER_COMMANDS: &[&str] = &["env", "timeout", "command", "stdbuf", "nice", "nohup"];

pub fn analyze_bash_command(command: &str) -> BashCommandAnalysis {
    let raw = command.trim().to_string();
    let has_pipe = contains_unquoted(command, "|") && !contains_unquoted(command, "||");
    let has_control_operator = contains_unquoted(command, "&&")
        || contains_unquoted(command, "||")
        || contains_unquoted(command, ";");
    let has_redirection = contains_unquoted(command, ">") || contains_unquoted(command, "<");
    let has_heredoc = contains_unquoted(command, "<<");
    let has_command_substitution =
        contains_unquoted(command, "$(") || contains_unquoted(command, "`");
    let has_subshell = contains_unquoted(command, "(") || contains_unquoted(command, ")");

    let subcommands = split_shell_segments(command)
        .into_iter()
        .map(|segment| {
            let normalized = normalize_simple_command(&segment);
            let base_command = base_command(&normalized);
            AnalyzedSubcommand {
                raw: segment.trim().to_string(),
                normalized,
                base_command,
            }
        })
        .filter(|segment| !segment.raw.is_empty())
        .collect::<Vec<_>>();

    let normalized = subcommands
        .first()
        .map(|segment| segment.normalized.clone())
        .unwrap_or_else(|| normalize_simple_command(command));
    let base_command = subcommands
        .first()
        .and_then(|segment| segment.base_command.clone())
        .or_else(|| base_command(&normalized));
    let changes_directory = subcommands
        .iter()
        .any(|segment| segment.base_command.as_deref() == Some("cd"));
    let too_complex = has_heredoc || has_command_substitution || unmatched_quotes(command);

    BashCommandAnalysis {
        raw,
        normalized,
        base_command,
        subcommands,
        has_pipe,
        has_control_operator,
        has_redirection,
        has_heredoc,
        has_command_substitution,
        has_subshell,
        changes_directory,
        too_complex,
    }
}

fn normalize_simple_command(command: &str) -> String {
    let tokens = tokenize_shell_words(command);
    if tokens.is_empty() {
        return command.trim().to_string();
    }

    let mut index = 0;
    while index < tokens.len() && is_env_assignment(&tokens[index]) {
        index += 1;
    }

    while index < tokens.len() {
        let token = tokens[index].as_str();
        if token == "env" {
            index += 1;
            while index < tokens.len() && is_env_assignment(&tokens[index]) {
                index += 1;
            }
            continue;
        }
        if token == "timeout" {
            index += 1;
            if index < tokens.len() && !tokens[index].starts_with('-') {
                index += 1;
            }
            continue;
        }
        if WRAPPER_COMMANDS.contains(&token) {
            index += 1;
            continue;
        }
        break;
    }

    tokens[index..].join(" ").trim().to_string()
}

fn base_command(command: &str) -> Option<String> {
    tokenize_shell_words(command).into_iter().next()
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn tokenize_shell_words(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => escaped = true,
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                } else {
                    current.push(ch);
                }
            }
            c if c.is_whitespace() && quote.is_none() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            _ => current.push(ch),
        }
    }

    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn split_shell_segments(command: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            current.push(ch);
            escaped = false;
            index += 1;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => {
                current.push(ch);
                escaped = true;
                index += 1;
            }
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
                current.push(ch);
                index += 1;
            }
            '|' if quote.is_none() => {
                if index + 1 < chars.len() && chars[index + 1] == '|' {
                    if !current.trim().is_empty() {
                        segments.push(current.trim().to_string());
                    }
                    current.clear();
                    index += 2;
                } else {
                    if !current.trim().is_empty() {
                        segments.push(current.trim().to_string());
                    }
                    current.clear();
                    index += 1;
                }
            }
            '&' if quote.is_none() && index + 1 < chars.len() && chars[index + 1] == '&' => {
                if !current.trim().is_empty() {
                    segments.push(current.trim().to_string());
                }
                current.clear();
                index += 2;
            }
            ';' if quote.is_none() => {
                if !current.trim().is_empty() {
                    segments.push(current.trim().to_string());
                }
                current.clear();
                index += 1;
            }
            _ => {
                current.push(ch);
                index += 1;
            }
        }
    }

    if !current.trim().is_empty() {
        segments.push(current.trim().to_string());
    }

    if segments.is_empty() {
        vec![command.trim().to_string()]
    } else {
        segments
    }
}

fn contains_unquoted(command: &str, needle: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    let chars: Vec<char> = command.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let mut index = 0;

    while index < chars.len() {
        let ch = chars[index];
        if escaped {
            escaped = false;
            index += 1;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => {
                escaped = true;
                index += 1;
                continue;
            }
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
                index += 1;
                continue;
            }
            _ => {}
        }

        if quote.is_none()
            && index + needle_chars.len() <= chars.len()
            && chars[index..index + needle_chars.len()] == needle_chars[..]
        {
            return true;
        }
        index += 1;
    }

    false
}

fn unmatched_quotes(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if quote != Some('\'') => escaped = true,
            '\'' | '"' => {
                if quote == Some(ch) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(ch);
                }
            }
            _ => {}
        }
    }
    quote.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_env_prefix() {
        let analysis = analyze_bash_command("FOO=bar cargo test");
        assert_eq!(analysis.base_command.as_deref(), Some("cargo"));
        assert_eq!(analysis.normalized, "cargo test");
    }

    #[test]
    fn normalizes_wrapper_command() {
        let analysis = analyze_bash_command("timeout 5 cargo test");
        assert_eq!(analysis.base_command.as_deref(), Some("cargo"));
        assert_eq!(analysis.normalized, "cargo test");
    }

    #[test]
    fn detects_pipe_and_segments() {
        let analysis = analyze_bash_command("cargo test | tee out.log");
        assert!(analysis.has_pipe);
        assert_eq!(analysis.subcommands.len(), 2);
    }

    #[test]
    fn detects_redirection() {
        let analysis = analyze_bash_command("echo hi > out.txt");
        assert!(analysis.has_redirection);
    }

    #[test]
    fn detects_command_substitution() {
        let analysis = analyze_bash_command("echo $(whoami)");
        assert!(analysis.has_command_substitution);
        assert!(analysis.too_complex);
    }

    #[test]
    fn detects_cd_composition() {
        let analysis = analyze_bash_command("cd repo && git status");
        assert!(analysis.changes_directory);
        assert!(analysis.has_control_operator);
        assert_eq!(analysis.subcommands.len(), 2);
    }

    #[test]
    fn marks_unmatched_quotes_complex() {
        let analysis = analyze_bash_command("echo 'oops");
        assert!(analysis.too_complex);
    }
}
