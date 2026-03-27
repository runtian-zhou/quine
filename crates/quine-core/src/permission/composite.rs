use async_trait::async_trait;

use super::{PermissionChecker, PermissionContext, PermissionDecision, PermissionError};
use crate::permission::llm_checker::LlmChecker;
use crate::permission::rule_checker::RuleBasedChecker;

/// Permission checker that evaluates commands with the LLM first, then applies
/// a manual allowlist override only for commands the LLM marked as dangerous.
pub struct CompositeChecker {
    llm_checker: Option<LlmChecker>,
    rule_checker: RuleBasedChecker,
}

impl CompositeChecker {
    /// Create a composite checker with an optional LLM evaluator and a
    /// rule-based checker used as a manual override.
    pub fn new(llm_checker: Option<LlmChecker>, rule_checker: RuleBasedChecker) -> Self {
        Self {
            llm_checker,
            rule_checker,
        }
    }
}

#[async_trait]
impl PermissionChecker for CompositeChecker {
    async fn check(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        context: &PermissionContext,
    ) -> Result<PermissionDecision, PermissionError> {
        if let Some(llm_checker) = &self.llm_checker {
            let llm_decision = llm_checker.check(tool_name, arguments, context).await?;

            if matches!(llm_decision, PermissionDecision::Deny { .. })
                && self
                    .rule_checker
                    .is_manually_allowlisted(tool_name, arguments)
            {
                return Ok(PermissionDecision::Allow);
            }

            return Ok(llm_decision);
        }

        self.rule_checker.check(tool_name, arguments, context).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::llm_checker::LlmChecker;
    use crate::session::SessionId;
    use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
    use std::path::PathBuf;
    use std::pin::Pin;
    use std::sync::Arc;

    fn test_context() -> PermissionContext {
        PermissionContext {
            session_id: SessionId::new(),
            working_directory: PathBuf::from("/tmp"),
        }
    }

    struct MockPermissionProvider {
        response: String,
    }

    impl MockPermissionProvider {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for MockPermissionProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let events = vec![
                Ok(LlmEvent::TextDelta {
                    text: self.response.clone(),
                }),
                Ok(LlmEvent::Done { usage: None }),
            ];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    #[tokio::test]
    async fn falls_back_to_rule_checker_without_llm() {
        let checker = CompositeChecker::new(None, RuleBasedChecker::new());
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({"command": "ls -la"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn returns_llm_decision_when_not_dangerous() {
        let llm = LlmChecker::new(Arc::new(MockPermissionProvider::new(
            r#"{"score": 0.5, "reason": "network access", "decision": "confirm"}"#,
        )));
        let checker = CompositeChecker::new(Some(llm), RuleBasedChecker::new());
        let ctx = test_context();
        let decision = checker
            .check(
                "bash",
                &serde_json::json!({"command": "curl https://example.com"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(
            decision,
            PermissionDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn manual_allowlist_overrides_dangerous_llm_decision() {
        let llm = LlmChecker::new(Arc::new(MockPermissionProvider::new(
            r#"{"score": 0.95, "reason": "dangerous", "decision": "deny"}"#,
        )));
        let checker = CompositeChecker::new(Some(llm), RuleBasedChecker::new());
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({"command": "ls -la"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn dangerous_llm_decision_stands_without_allowlist_override() {
        let llm = LlmChecker::new(Arc::new(MockPermissionProvider::new(
            r#"{"score": 0.95, "reason": "dangerous", "decision": "deny"}"#,
        )));
        let checker = CompositeChecker::new(Some(llm), RuleBasedChecker::new());
        let ctx = test_context();
        let decision = checker
            .check(
                "bash",
                &serde_json::json!({"command": "curl https://example.com"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }
}
