pub mod scheduler;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Unique identifier for an action plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PlanId(Uuid);

impl PlanId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PlanId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for PlanId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Human-readable identifier for an action within a plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ActionId(String);

impl ActionId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The execution status of an action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ActionStatus {
    Pending,
    InProgress,
    Completed,
    Failed { error: String },
    Skipped { reason: String },
}

impl ActionStatus {
    /// Whether this status represents a terminal state.
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            ActionStatus::Completed | ActionStatus::Failed { .. } | ActionStatus::Skipped { .. }
        )
    }

    /// Short display label for the status.
    pub fn label(&self) -> &str {
        match self {
            ActionStatus::Pending => "pending",
            ActionStatus::InProgress => "in-progress",
            ActionStatus::Completed => "completed",
            ActionStatus::Failed { .. } => "failed",
            ActionStatus::Skipped { .. } => "skipped",
        }
    }
}

/// A single action within a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Action {
    pub action_id: ActionId,
    pub title: String,
    pub description: String,
    /// IDs of actions that must complete before this one can start.
    pub depends_on: Vec<ActionId>,
    pub status: ActionStatus,
    /// Output/result after execution.
    pub result: Option<String>,
}

/// A plan consisting of a DAG of actions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionPlan {
    pub plan_id: PlanId,
    pub title: String,
    pub actions: Vec<Action>,
}

impl ActionPlan {
    /// Count the number of actions that are not yet in a terminal state.
    pub fn remaining_count(&self) -> usize {
        self.actions
            .iter()
            .filter(|a| !a.status.is_terminal())
            .count()
    }

    /// Whether all actions are in a terminal state.
    pub fn is_complete(&self) -> bool {
        self.actions.iter().all(|a| a.status.is_terminal())
    }

    /// Find an action by ID (mutable).
    pub fn get_action_mut(&mut self, action_id: &ActionId) -> Option<&mut Action> {
        self.actions.iter_mut().find(|a| a.action_id == *action_id)
    }

    /// Find an action by ID.
    pub fn get_action(&self, action_id: &ActionId) -> Option<&Action> {
        self.actions.iter().find(|a| a.action_id == *action_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_id_display_and_parse() {
        let id = PlanId::new();
        let s = id.to_string();
        let parsed: PlanId = s.parse().unwrap();
        assert_eq!(id, parsed);
    }

    #[test]
    fn action_id_as_str() {
        let id = ActionId::new("a1");
        assert_eq!(id.as_str(), "a1");
        assert_eq!(id.to_string(), "a1");
    }

    #[test]
    fn action_status_is_terminal() {
        assert!(!ActionStatus::Pending.is_terminal());
        assert!(!ActionStatus::InProgress.is_terminal());
        assert!(ActionStatus::Completed.is_terminal());
        assert!(ActionStatus::Failed {
            error: "err".into()
        }
        .is_terminal());
        assert!(ActionStatus::Skipped { reason: "r".into() }.is_terminal());
    }

    #[test]
    fn action_plan_remaining_and_complete() {
        let mut plan = ActionPlan {
            plan_id: PlanId::new(),
            title: "test".into(),
            actions: vec![
                Action {
                    action_id: ActionId::new("a1"),
                    title: "one".into(),
                    description: "desc".into(),
                    depends_on: vec![],
                    status: ActionStatus::Completed,
                    result: None,
                },
                Action {
                    action_id: ActionId::new("a2"),
                    title: "two".into(),
                    description: "desc".into(),
                    depends_on: vec![],
                    status: ActionStatus::Pending,
                    result: None,
                },
            ],
        };
        assert_eq!(plan.remaining_count(), 1);
        assert!(!plan.is_complete());

        plan.actions[1].status = ActionStatus::Completed;
        assert_eq!(plan.remaining_count(), 0);
        assert!(plan.is_complete());
    }

    #[test]
    fn action_plan_get_action() {
        let plan = ActionPlan {
            plan_id: PlanId::new(),
            title: "test".into(),
            actions: vec![Action {
                action_id: ActionId::new("a1"),
                title: "one".into(),
                description: "desc".into(),
                depends_on: vec![],
                status: ActionStatus::Pending,
                result: None,
            }],
        };
        assert!(plan.get_action(&ActionId::new("a1")).is_some());
        assert!(plan.get_action(&ActionId::new("a2")).is_none());
    }

    #[test]
    fn serialization_roundtrip() {
        let plan = ActionPlan {
            plan_id: PlanId::new(),
            title: "test plan".into(),
            actions: vec![Action {
                action_id: ActionId::new("a1"),
                title: "one".into(),
                description: "desc".into(),
                depends_on: vec![ActionId::new("a0")],
                status: ActionStatus::Failed {
                    error: "boom".into(),
                },
                result: Some("partial".into()),
            }],
        };
        let json = serde_json::to_string(&plan).unwrap();
        let parsed: ActionPlan = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.title, "test plan");
        assert_eq!(parsed.actions[0].action_id, ActionId::new("a1"));
    }
}
