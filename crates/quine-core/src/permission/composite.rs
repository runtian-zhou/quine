use async_trait::async_trait;

use super::{PermissionChecker, PermissionContext, PermissionDecision, PermissionError};

/// Composite permission checker that chains multiple checkers and takes the
/// most restrictive decision.
///
/// Runs all checkers in order. The final decision is the most restrictive
/// across all results:
/// - If any checker returns `Deny`, the final decision is `Deny`.
/// - If any checker returns `RequiresConfirmation`, the highest risk score
///   and its reason are used.
/// - Otherwise, `Allow`.
pub struct CompositeChecker {
    checkers: Vec<Box<dyn PermissionChecker>>,
}

impl CompositeChecker {
    /// Create a new `CompositeChecker` with the given list of checkers.
    pub fn new(checkers: Vec<Box<dyn PermissionChecker>>) -> Self {
        Self { checkers }
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
        let mut most_restrictive = PermissionDecision::Allow;

        for checker in &self.checkers {
            let decision = checker.check(tool_name, arguments, context).await?;

            if decision.is_more_restrictive_than(&most_restrictive) {
                most_restrictive = match (&most_restrictive, &decision) {
                    // If both are RequiresConfirmation, keep the higher risk score
                    (
                        PermissionDecision::RequiresConfirmation {
                            risk_score: existing_score,
                            ..
                        },
                        PermissionDecision::RequiresConfirmation {
                            risk_score: new_score,
                            ..
                        },
                    ) => {
                        if *new_score > *existing_score {
                            decision
                        } else {
                            most_restrictive
                        }
                    }
                    _ => decision,
                };
            } else if let (
                PermissionDecision::RequiresConfirmation {
                    risk_score: existing_score,
                    ..
                },
                PermissionDecision::RequiresConfirmation {
                    risk_score: new_score,
                    ..
                },
            ) = (&most_restrictive, &decision)
            {
                // Same severity level but check if the new score is higher
                if new_score > existing_score {
                    most_restrictive = decision;
                }
            }
        }

        Ok(most_restrictive)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::SessionId;
    use std::path::PathBuf;

    fn test_context() -> PermissionContext {
        PermissionContext {
            session_id: SessionId::new(),
            working_directory: PathBuf::from("/tmp"),
        }
    }

    /// A checker that always returns a fixed decision.
    struct FixedChecker {
        decision: PermissionDecision,
    }

    impl FixedChecker {
        fn allow() -> Self {
            Self {
                decision: PermissionDecision::Allow,
            }
        }

        fn confirm(score: f64, reason: &str) -> Self {
            Self {
                decision: PermissionDecision::RequiresConfirmation {
                    risk_score: score,
                    reason: reason.to_string(),
                },
            }
        }

        fn deny(reason: &str) -> Self {
            Self {
                decision: PermissionDecision::Deny {
                    reason: reason.to_string(),
                },
            }
        }
    }

    #[async_trait]
    impl PermissionChecker for FixedChecker {
        async fn check(
            &self,
            _tool_name: &str,
            _arguments: &serde_json::Value,
            _context: &PermissionContext,
        ) -> Result<PermissionDecision, PermissionError> {
            Ok(self.decision.clone())
        }
    }

    #[tokio::test]
    async fn composite_all_allow() {
        let checker = CompositeChecker::new(vec![
            Box::new(FixedChecker::allow()),
            Box::new(FixedChecker::allow()),
        ]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn composite_deny_wins() {
        let checker = CompositeChecker::new(vec![
            Box::new(FixedChecker::allow()),
            Box::new(FixedChecker::deny("dangerous")),
            Box::new(FixedChecker::confirm(0.5, "risky")),
        ]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }

    #[tokio::test]
    async fn composite_confirm_wins_over_allow() {
        let checker = CompositeChecker::new(vec![
            Box::new(FixedChecker::allow()),
            Box::new(FixedChecker::confirm(0.6, "network access")),
        ]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(matches!(
            decision,
            PermissionDecision::RequiresConfirmation { .. }
        ));
    }

    #[tokio::test]
    async fn composite_highest_risk_score_wins() {
        let checker = CompositeChecker::new(vec![
            Box::new(FixedChecker::confirm(0.3, "low risk")),
            Box::new(FixedChecker::confirm(0.8, "high risk")),
        ]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        match decision {
            PermissionDecision::RequiresConfirmation { risk_score, reason } => {
                assert!((risk_score - 0.8).abs() < f64::EPSILON);
                assert_eq!(reason, "high risk");
            }
            _ => panic!("expected RequiresConfirmation"),
        }
    }

    #[tokio::test]
    async fn composite_empty_checkers_allows() {
        let checker = CompositeChecker::new(vec![]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Allow));
    }

    #[tokio::test]
    async fn composite_deny_first_still_wins() {
        let checker = CompositeChecker::new(vec![
            Box::new(FixedChecker::deny("blocked")),
            Box::new(FixedChecker::allow()),
        ]);
        let ctx = test_context();
        let decision = checker
            .check("bash", &serde_json::json!({}), &ctx)
            .await
            .unwrap();
        assert!(matches!(decision, PermissionDecision::Deny { .. }));
    }
}
