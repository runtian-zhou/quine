use async_trait::async_trait;
use futures::StreamExt;
use quine_llm::{LlmEvent, LlmProvider, Message};
use serde::Deserialize;

use super::{PermissionChecker, PermissionContext, PermissionDecision, PermissionError};

/// System prompt for the LLM-based permission checker.
const PERMISSION_SYSTEM_PROMPT: &str = r#"You are a security evaluator for a coding agent's tool calls. Your job is to assess the risk of executing a tool invocation.

Evaluate the tool name and arguments, and respond with ONLY a JSON object (no markdown, no explanation):

{
  "score": <float between 0.0 and 1.0>,
  "reason": "<brief explanation of the risk>",
  "decision": "<one of: allow, confirm, deny>"
}

Guidelines:
- score 0.0-0.3: Safe operations (reading files, listing directories, running tests)
- score 0.3-0.7: Potentially risky (writing files, network access, installing packages)
- score 0.7-1.0: Dangerous (deleting files, system commands, privilege escalation, disk operations)

Be conservative: when in doubt, recommend "confirm"."#;

/// LLM-based permission checker that uses an LLM provider to evaluate tool calls.
///
/// Sends the tool name and arguments to a separate LLM call with a system prompt
/// asking for risk evaluation. The LLM response is parsed as JSON and mapped to
/// a `PermissionDecision`.
pub struct LlmChecker {
    provider: Box<dyn LlmProvider>,
}

impl LlmChecker {
    /// Create a new `LlmChecker` with the given LLM provider.
    pub fn new(provider: Box<dyn LlmProvider>) -> Self {
        Self { provider }
    }

    /// Parse the LLM's JSON response into a `PermissionDecision`.
    fn parse_response(text: &str) -> Result<PermissionDecision, PermissionError> {
        #[derive(Deserialize)]
        struct LlmResponse {
            score: f64,
            reason: String,
            decision: String,
        }

        // Try to extract JSON from the response (LLM might include extra text)
        let json_text = extract_json(text).ok_or_else(|| PermissionError::ParseError {
            message: format!("no JSON object found in LLM response: {text}"),
        })?;

        let response: LlmResponse =
            serde_json::from_str(json_text).map_err(|e| PermissionError::ParseError {
                message: format!("failed to parse LLM response: {e}"),
            })?;

        let score = response.score.clamp(0.0, 1.0);

        if score > 0.7 || response.decision.eq_ignore_ascii_case("deny") {
            Ok(PermissionDecision::Deny {
                reason: response.reason,
            })
        } else if response.decision.eq_ignore_ascii_case("allow") {
            Ok(PermissionDecision::Allow)
        } else {
            Ok(PermissionDecision::RequiresConfirmation {
                risk_score: score,
                reason: response.reason,
            })
        }
    }
}

/// Extract the first JSON object from a string.
fn extract_json(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0;
    for (i, ch) in text[start..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }
    None
}

#[async_trait]
impl PermissionChecker for LlmChecker {
    async fn check(
        &self,
        tool_name: &str,
        arguments: &serde_json::Value,
        _context: &PermissionContext,
    ) -> Result<PermissionDecision, PermissionError> {
        let user_message = format!(
            "Tool: {tool_name}\nArguments: {}",
            serde_json::to_string_pretty(arguments).unwrap_or_else(|_| arguments.to_string())
        );

        let messages = vec![
            Message::system(PERMISSION_SYSTEM_PROMPT),
            Message::user(user_message),
        ];

        let stream_result = self.provider.send(&messages, &[]).await.map_err(|e| {
            PermissionError::LlmUnavailable {
                message: e.to_string(),
            }
        })?;

        let mut stream = stream_result;
        let mut full_text = String::new();

        while let Some(event_result) = stream.next().await {
            match event_result {
                Ok(LlmEvent::TextDelta { text }) => {
                    full_text.push_str(&text);
                }
                Ok(LlmEvent::Done) => break,
                Err(e) => {
                    return Err(PermissionError::LlmUnavailable {
                        message: e.to_string(),
                    });
                }
                _ => {}
            }
        }

        // Fall back to RequiresConfirmation if parsing fails
        Self::parse_response(&full_text).or_else(|_| {
            Ok(PermissionDecision::RequiresConfirmation {
                risk_score: 0.5,
                reason: format!(
                    "could not parse LLM permission response, defaulting to confirmation: {full_text}"
                ),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use quine_llm::ToolDefinition;
    use std::path::PathBuf;
    use std::pin::Pin;

    fn test_context() -> PermissionContext {
        PermissionContext {
            session_id: SessionId::new(),
            working_directory: PathBuf::from("/tmp"),
        }
    }

    /// Mock provider that returns a fixed JSON response.
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

    #[async_trait::async_trait]
    impl LlmProvider for MockPermissionProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            let text = self.response.clone();
            let events = vec![Ok(LlmEvent::TextDelta { text }), Ok(LlmEvent::Done)];
            Ok(Box::pin(futures::stream::iter(events)))
        }
    }

    /// Mock provider that returns an error.
    struct ErrorProvider;

    #[async_trait::async_trait]
    impl LlmProvider for ErrorProvider {
        async fn send(
            &self,
            _messages: &[Message],
            _tools: &[ToolDefinition],
        ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>>
        {
            Err(anyhow::anyhow!("LLM unavailable"))
        }
    }

    #[tokio::test]
    async fn llm_checker_allow_response() {
        let provider = MockPermissionProvider::new(
            r#"{"score": 0.1, "reason": "read-only operation", "decision": "allow"}"#,
        );
        let checker = LlmChecker::new(Box::new(provider));
        let ctx = test_context();

        let decision = checker
            .check("bash", &serde_json::json!({"command": "ls"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn llm_checker_deny_response() {
        let provider = MockPermissionProvider::new(
            r#"{"score": 0.9, "reason": "destructive operation", "decision": "deny"}"#,
        );
        let checker = LlmChecker::new(Box::new(provider));
        let ctx = test_context();

        let decision = checker
            .check("bash", &serde_json::json!({"command": "rm -rf /"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn llm_checker_confirm_response() {
        let provider = MockPermissionProvider::new(
            r#"{"score": 0.5, "reason": "network access", "decision": "confirm"}"#,
        );
        let checker = LlmChecker::new(Box::new(provider));
        let ctx = test_context();

        let decision = checker
            .check(
                "bash",
                &serde_json::json!({"command": "curl example.com"}),
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
    async fn llm_checker_unparseable_falls_back_to_confirm() {
        let provider = MockPermissionProvider::new("I don't understand the request.");
        let checker = LlmChecker::new(Box::new(provider));
        let ctx = test_context();

        let decision = checker
            .check("bash", &serde_json::json!({"command": "something"}), &ctx)
            .await
            .unwrap();
        assert!(matches!(
            decision,
            PermissionDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn llm_checker_provider_error() {
        let checker = LlmChecker::new(Box::new(ErrorProvider));
        let ctx = test_context();

        let result = checker
            .check("bash", &serde_json::json!({"command": "ls"}), &ctx)
            .await;
        assert!(matches!(
            result,
            Err(PermissionError::LlmUnavailable { .. })
        ));
    }

    #[test]
    fn extract_json_from_text() {
        let text =
            r#"Here is my analysis: {"score": 0.5, "reason": "test", "decision": "confirm"} done."#;
        let json = extract_json(text).unwrap();
        assert!(json.starts_with('{'));
        assert!(json.ends_with('}'));

        let parsed: serde_json::Value = serde_json::from_str(json).unwrap();
        assert_eq!(parsed["score"], 0.5);
    }

    #[test]
    fn extract_json_no_json() {
        assert!(extract_json("no json here").is_none());
    }

    #[test]
    fn parse_response_allow() {
        let resp = r#"{"score": 0.1, "reason": "safe", "decision": "allow"}"#;
        let decision = LlmChecker::parse_response(resp).unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[test]
    fn parse_response_deny() {
        let resp = r#"{"score": 0.9, "reason": "dangerous", "decision": "deny"}"#;
        let decision = LlmChecker::parse_response(resp).unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[test]
    fn parse_response_confirm() {
        let resp = r#"{"score": 0.5, "reason": "moderate", "decision": "confirm"}"#;
        let decision = LlmChecker::parse_response(resp).unwrap();
        assert!(matches!(
            decision,
            PermissionDecision::RequiresConfirmation { .. }
        ));
    }

    #[test]
    fn parse_response_score_clamped() {
        let resp = r#"{"score": 1.5, "reason": "extreme", "decision": "deny"}"#;
        let decision = LlmChecker::parse_response(resp).unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }
}
