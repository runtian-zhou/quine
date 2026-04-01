pub mod bash_analysis;
pub mod composite;
pub mod llm_checker;
#[path = "rule_checker_impl.rs"]
pub mod rule_checker;

use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::session::SessionId;

/// Errors that can occur during permission checking.
#[derive(Debug, thiserror::Error)]
pub enum PermissionError {
    /// The LLM provider was unavailable or returned an error.
    #[error("llm provider error: {message}")]
    LlmUnavailable { message: String },

    /// Failed to parse the LLM's response.
    #[error("failed to parse permission response: {message}")]
    ParseError { message: String },

    /// An internal error occurred during permission checking.
    #[error("internal permission error: {message}")]
    Internal { message: String },
}

/// Structured explanation for a permission decision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum PermissionDecisionReason {
    RuleMatch {
        behavior: String,
        pattern: String,
        description: String,
    },
    StructuralRisk {
        kind: StructuralRiskKind,
        detail: String,
    },
    Fallback {
        command: String,
        detail: String,
    },
    LlmAssessment {
        summary: String,
    },
}

/// Specific shell structure that raised risk.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum StructuralRiskKind {
    Pipe,
    ControlOperator,
    Redirection,
    Heredoc,
    CommandSubstitution,
    Subshell,
    DirectoryChange,
    CompoundCommand,
    ComplexShell,
}

/// Suggested reusable permission scope for future UX.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PermissionSuggestion {
    pub pattern: String,
    pub description: String,
}

/// The result of a permission check on a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PermissionDecision {
    /// Safe to execute without confirmation.
    Allow,
    /// Potentially dangerous — requires user confirmation.
    /// Includes a risk score (0.0-1.0) and explanation.
    RequiresConfirmation {
        risk_score: f64,
        reason: String,
        detail: Option<PermissionDecisionReason>,
        suggestion: Option<PermissionSuggestion>,
    },
    /// Blocked outright — must not execute.
    Deny {
        reason: String,
        detail: Option<PermissionDecisionReason>,
    },
}

impl PermissionDecision {
    /// Returns a numeric severity for ordering decisions.
    /// Higher is more restrictive.
    fn severity(&self) -> u8 {
        match self {
            PermissionDecision::Allow => 0,
            PermissionDecision::RequiresConfirmation { .. } => 1,
            PermissionDecision::Deny { .. } => 2,
        }
    }

    /// Returns true if this decision is more restrictive than the other.
    pub fn is_more_restrictive_than(&self, other: &PermissionDecision) -> bool {
        self.severity() > other.severity()
    }
}

/// Metadata about the current session/tool invocation.
#[derive(Debug, Clone)]
pub struct PermissionContext {
    /// The session this tool is executing within.
    pub session_id: SessionId,
    /// The working directory for this tool execution.
    pub working_directory: PathBuf,
}

/// Trait for evaluating whether a tool call should be permitted.
///
/// Implementations can use pattern matching, LLM evaluation, or any
/// other strategy to assess the risk of a tool invocation.
#[async_trait]
pub trait PermissionChecker: Send + Sync {
    /// Evaluate a tool call and return a permission decision.
    async fn check(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        context: &PermissionContext,
    ) -> Result<PermissionDecision, PermissionError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_decision_severity_ordering() {
        let allow = PermissionDecision::Allow;
        let confirm = PermissionDecision::RequiresConfirmation {
            risk_score: 0.5,
            reason: "test".into(),
            detail: None,
            suggestion: None,
        };
        let deny = PermissionDecision::Deny {
            reason: "test".into(),
            detail: None,
        };

        assert!(!allow.is_more_restrictive_than(&confirm));
        assert!(confirm.is_more_restrictive_than(&allow));
        assert!(deny.is_more_restrictive_than(&confirm));
        assert!(deny.is_more_restrictive_than(&allow));
        assert!(!allow.is_more_restrictive_than(&allow));
    }

    #[test]
    fn permission_error_display() {
        let err = PermissionError::LlmUnavailable {
            message: "timeout".into(),
        };
        assert!(err.to_string().contains("timeout"));

        let err = PermissionError::ParseError {
            message: "bad json".into(),
        };
        assert!(err.to_string().contains("bad json"));
    }

    #[test]
    fn permission_decision_serialization() {
        let decision = PermissionDecision::RequiresConfirmation {
            risk_score: 0.7,
            reason: "risky command".into(),
            detail: None,
            suggestion: None,
        };
        let json = serde_json::to_string(&decision).unwrap();
        let deserialized: PermissionDecision = serde_json::from_str(&json).unwrap();
        match deserialized {
            PermissionDecision::RequiresConfirmation {
                risk_score,
                reason,
                detail,
                suggestion,
            } => {
                assert!((risk_score - 0.7).abs() < f64::EPSILON);
                assert_eq!(reason, "risky command");
                assert!(detail.is_none());
                assert!(suggestion.is_none());
            }
            _ => panic!("expected RequiresConfirmation"),
        }
    }
}
