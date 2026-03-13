use std::collections::HashMap;
use std::io::Write;

/// Risk level for tool operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiskLevel {
    /// Auto-approve (Read, Glob, Grep, ListDirectory, Skill, Todo)
    Low,
    /// Ask on first use, remember choice (Write, Edit)
    Medium,
    /// Always ask, user can respond y/n/always (Bash)
    High,
}

/// User's permission decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Deny,
    AlwaysAllow,
}

/// Manages tool execution permissions.
pub struct PermissionManager {
    /// Remembered decisions per tool name.
    decisions: HashMap<String, PermissionDecision>,
}

impl PermissionManager {
    pub fn new() -> Self {
        Self {
            decisions: HashMap::new(),
        }
    }

    /// Returns the risk level for a given tool name.
    pub fn risk_level(tool_name: &str) -> RiskLevel {
        match tool_name {
            "Bash" => RiskLevel::High,
            "Write" | "Edit" => RiskLevel::Medium,
            _ => RiskLevel::Low,
        }
    }

    /// Check if tool execution is allowed. May prompt the user.
    /// Returns true if allowed, false if denied.
    pub fn check(&mut self, tool_name: &str, context: &str) -> bool {
        let risk = Self::risk_level(tool_name);

        match risk {
            RiskLevel::Low => true,
            RiskLevel::Medium => {
                if let Some(decision) = self.decisions.get(tool_name) {
                    return *decision != PermissionDecision::Deny;
                }
                let decision = self.prompt_user(tool_name, context);
                self.decisions.insert(tool_name.to_string(), decision);
                decision != PermissionDecision::Deny
            }
            RiskLevel::High => {
                if let Some(PermissionDecision::AlwaysAllow) = self.decisions.get(tool_name) {
                    return true;
                }
                let decision = self.prompt_user(tool_name, context);
                if decision == PermissionDecision::AlwaysAllow {
                    self.decisions.insert(tool_name.to_string(), decision);
                }
                decision != PermissionDecision::Deny
            }
        }
    }

    fn prompt_user(&self, tool_name: &str, context: &str) -> PermissionDecision {
        let risk = Self::risk_level(tool_name);
        let options = if risk == RiskLevel::High {
            "(y/n/always)"
        } else {
            "(y/n)"
        };

        print!(
            "\x1b[1;33m? Allow {} {}?\x1b[0m {} ",
            tool_name, context, options
        );
        std::io::stdout().flush().ok();

        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            return PermissionDecision::Deny;
        }

        match input.trim().to_lowercase().as_str() {
            "y" | "yes" => PermissionDecision::Allow,
            "always" | "a" => PermissionDecision::AlwaysAllow,
            _ => PermissionDecision::Deny,
        }
    }

    /// Print current permission state.
    pub fn print_status(&self) {
        println!("\x1b[1;36mTool permissions:\x1b[0m");
        let tools = [
            ("Read", RiskLevel::Low),
            ("Glob", RiskLevel::Low),
            ("Grep", RiskLevel::Low),
            ("ListDirectory", RiskLevel::Low),
            ("Write", RiskLevel::Medium),
            ("Edit", RiskLevel::Medium),
            ("Bash", RiskLevel::High),
        ];
        for (name, risk) in &tools {
            let status = match self.decisions.get(*name) {
                Some(PermissionDecision::AlwaysAllow) => "always allowed",
                Some(PermissionDecision::Allow) => "allowed",
                Some(PermissionDecision::Deny) => "denied",
                None => match risk {
                    RiskLevel::Low => "auto-approve",
                    RiskLevel::Medium => "ask on first use",
                    RiskLevel::High => "ask every time",
                },
            };
            let risk_label = match risk {
                RiskLevel::Low => "\x1b[32mlow\x1b[0m",
                RiskLevel::Medium => "\x1b[33mmedium\x1b[0m",
                RiskLevel::High => "\x1b[31mhigh\x1b[0m",
            };
            println!("  \x1b[1m{:<15}\x1b[0m risk: {}  status: {}", name, risk_label, status);
        }
    }
}
