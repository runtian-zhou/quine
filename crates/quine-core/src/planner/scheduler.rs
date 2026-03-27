use std::collections::{HashMap, HashSet, VecDeque};

use super::{ActionId, ActionPlan, ActionStatus};

/// Errors from plan validation.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PlanError {
    #[error("cycle detected in action dependency graph")]
    CycleDetected,

    #[error("action '{action_id}' depends on unknown action '{depends_on}'")]
    MissingDependency {
        action_id: String,
        depends_on: String,
    },

    #[error("duplicate action ID: '{0}'")]
    DuplicateActionId(String),
}

/// Validate that the plan's actions form a valid DAG.
///
/// Checks for:
/// - Duplicate action IDs
/// - Missing dependency references
/// - Cycles (via topological sort)
pub fn validate_dag(plan: &ActionPlan) -> Result<(), PlanError> {
    let mut seen_ids = HashSet::new();

    // Check for duplicate IDs
    for action in &plan.actions {
        if !seen_ids.insert(&action.action_id) {
            return Err(PlanError::DuplicateActionId(
                action.action_id.as_str().to_string(),
            ));
        }
    }

    // Check for missing dependencies
    for action in &plan.actions {
        for dep in &action.depends_on {
            if !seen_ids.contains(dep) {
                return Err(PlanError::MissingDependency {
                    action_id: action.action_id.as_str().to_string(),
                    depends_on: dep.as_str().to_string(),
                });
            }
        }
    }

    // Topological sort to detect cycles (Kahn's algorithm)
    let mut in_degree: HashMap<&ActionId, usize> = HashMap::new();
    let mut adjacency: HashMap<&ActionId, Vec<&ActionId>> = HashMap::new();

    for action in &plan.actions {
        in_degree.entry(&action.action_id).or_insert(0);
        adjacency.entry(&action.action_id).or_default();
        for dep in &action.depends_on {
            adjacency.entry(dep).or_default().push(&action.action_id);
            *in_degree.entry(&action.action_id).or_insert(0) += 1;
        }
    }

    let mut queue: VecDeque<&ActionId> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut visited = 0usize;

    while let Some(node) = queue.pop_front() {
        visited += 1;
        if let Some(neighbors) = adjacency.get(node) {
            for neighbor in neighbors {
                if let Some(deg) = in_degree.get_mut(neighbor) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
        }
    }

    if visited != plan.actions.len() {
        return Err(PlanError::CycleDetected);
    }

    Ok(())
}

/// Return actions whose dependencies are all `Completed` and whose own status is `Pending`.
pub fn get_ready_actions(plan: &ActionPlan) -> Vec<&super::Action> {
    let completed: HashSet<&ActionId> = plan
        .actions
        .iter()
        .filter(|a| matches!(a.status, ActionStatus::Completed))
        .map(|a| &a.action_id)
        .collect();

    plan.actions
        .iter()
        .filter(|a| {
            matches!(a.status, ActionStatus::Pending)
                && a.depends_on.iter().all(|dep| completed.contains(dep))
        })
        .collect()
}

/// Mark all downstream dependents of a failed action as `Skipped`.
pub fn skip_dependents(plan: &mut ActionPlan, failed_action_id: &ActionId) {
    // Build the set of actions to skip by BFS through the dependency graph
    let mut to_skip: HashSet<ActionId> = HashSet::new();
    let mut queue: VecDeque<ActionId> = VecDeque::new();
    queue.push_back(failed_action_id.clone());

    // Build adjacency: for each action, which actions depend on it
    let mut dependents: HashMap<ActionId, Vec<ActionId>> = HashMap::new();
    for action in &plan.actions {
        for dep in &action.depends_on {
            dependents
                .entry(dep.clone())
                .or_default()
                .push(action.action_id.clone());
        }
    }

    while let Some(current) = queue.pop_front() {
        if let Some(deps) = dependents.get(&current) {
            for dep_id in deps {
                if to_skip.insert(dep_id.clone()) {
                    queue.push_back(dep_id.clone());
                }
            }
        }
    }

    // Apply skips
    for action in &mut plan.actions {
        if to_skip.contains(&action.action_id) && !action.status.is_terminal() {
            action.status = ActionStatus::Skipped {
                reason: "dependency failed".into(),
            };
        }
    }
}

/// Render a text representation of the plan with status indicators.
pub fn render_plan(plan: &ActionPlan) -> String {
    let ready_set: HashSet<&ActionId> = get_ready_actions(plan)
        .iter()
        .map(|a| &a.action_id)
        .collect();

    let mut lines = Vec::new();
    lines.push(format!(
        "Plan: {} ({} actions)",
        plan.title,
        plan.actions.len()
    ));

    for action in &plan.actions {
        let (status_emoji, status_indicator) = match &action.status {
            ActionStatus::Pending => {
                if ready_set.contains(&action.action_id) {
                    ("🟢", "ready".to_string())
                } else {
                    let deps: Vec<&str> = action.depends_on.iter().map(|d| d.as_str()).collect();
                    ("🟡", format!("blocked by: {}", deps.join(", ")))
                }
            }
            ActionStatus::InProgress => ("🔄", "in-progress".to_string()),
            ActionStatus::Completed => ("✅", "completed".to_string()),
            ActionStatus::Failed { error } => ("❌", format!("failed: {error}")),
            ActionStatus::Skipped { reason } => ("⏭️", format!("skipped: {reason}")),
        };

        lines.push(format!(
            "  {status_emoji} [{}] {:<30} {status_indicator}",
            action.action_id, action.title
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{Action, ActionPlan, PlanId};

    fn make_action(id: &str, deps: &[&str]) -> Action {
        Action {
            action_id: ActionId::new(id),
            title: format!("Action {id}"),
            description: format!("Description for {id}"),
            depends_on: deps.iter().map(|d| ActionId::new(*d)).collect(),
            status: ActionStatus::Pending,
            result: None,
        }
    }

    fn make_plan(actions: Vec<Action>) -> ActionPlan {
        ActionPlan {
            plan_id: PlanId::new(),
            title: "Test Plan".into(),
            actions,
        }
    }

    #[test]
    fn validate_dag_valid() {
        let plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &[]),
            make_action("a3", &["a1", "a2"]),
            make_action("a4", &["a3"]),
        ]);
        assert!(validate_dag(&plan).is_ok());
    }

    #[test]
    fn validate_dag_cycle_detected() {
        let plan = make_plan(vec![
            make_action("a1", &["a3"]),
            make_action("a2", &["a1"]),
            make_action("a3", &["a2"]),
        ]);
        let err = validate_dag(&plan).unwrap_err();
        assert!(matches!(err, PlanError::CycleDetected));
    }

    #[test]
    fn validate_dag_missing_dependency() {
        let plan = make_plan(vec![make_action("a1", &["a_nonexistent"])]);
        let err = validate_dag(&plan).unwrap_err();
        match err {
            PlanError::MissingDependency {
                action_id,
                depends_on,
            } => {
                assert_eq!(action_id, "a1");
                assert_eq!(depends_on, "a_nonexistent");
            }
            other => panic!("expected MissingDependency, got {other:?}"),
        }
    }

    #[test]
    fn validate_dag_duplicate_id() {
        let plan = make_plan(vec![make_action("a1", &[]), make_action("a1", &[])]);
        let err = validate_dag(&plan).unwrap_err();
        assert!(matches!(err, PlanError::DuplicateActionId(ref id) if id == "a1"));
    }

    #[test]
    fn get_ready_actions_initial() {
        let plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &[]),
            make_action("a3", &["a1", "a2"]),
        ]);
        let ready = get_ready_actions(&plan);
        let ids: Vec<&str> = ready.iter().map(|a| a.action_id.as_str()).collect();
        assert!(ids.contains(&"a1"));
        assert!(ids.contains(&"a2"));
        assert!(!ids.contains(&"a3"));
    }

    #[test]
    fn get_ready_actions_after_completion() {
        let mut plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &[]),
            make_action("a3", &["a1", "a2"]),
        ]);
        plan.actions[0].status = ActionStatus::Completed;
        plan.actions[1].status = ActionStatus::Completed;

        let ready = get_ready_actions(&plan);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].action_id.as_str(), "a3");
    }

    #[test]
    fn get_ready_actions_partial_deps() {
        let mut plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &[]),
            make_action("a3", &["a1", "a2"]),
        ]);
        plan.actions[0].status = ActionStatus::Completed;
        // a2 still pending (but has no deps, so it's ready); a3 is not ready
        // because a2 is not yet completed.

        let ready = get_ready_actions(&plan);
        let ids: Vec<&str> = ready.iter().map(|a| a.action_id.as_str()).collect();
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&"a2"));
        assert!(!ids.contains(&"a3"));
    }

    #[test]
    fn skip_dependents_propagates() {
        let mut plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &["a1"]),
            make_action("a3", &["a2"]),
            make_action("a4", &[]), // independent, should not be skipped
        ]);
        plan.actions[0].status = ActionStatus::Failed {
            error: "boom".into(),
        };

        skip_dependents(&mut plan, &ActionId::new("a1"));

        assert!(matches!(
            plan.actions[1].status,
            ActionStatus::Skipped { .. }
        ));
        assert!(matches!(
            plan.actions[2].status,
            ActionStatus::Skipped { .. }
        ));
        assert!(matches!(plan.actions[3].status, ActionStatus::Pending));
    }

    #[test]
    fn skip_dependents_does_not_skip_already_completed() {
        let mut plan = make_plan(vec![make_action("a1", &[]), make_action("a2", &["a1"])]);
        plan.actions[1].status = ActionStatus::Completed;
        plan.actions[0].status = ActionStatus::Failed {
            error: "boom".into(),
        };

        skip_dependents(&mut plan, &ActionId::new("a1"));

        // a2 was already completed, should remain completed
        assert!(matches!(plan.actions[1].status, ActionStatus::Completed));
    }

    #[test]
    fn render_plan_output() {
        let plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &[]),
            make_action("a3", &["a1", "a2"]),
        ]);
        let rendered = render_plan(&plan);
        assert!(rendered.contains("Test Plan"));
        assert!(rendered.contains("🟢"));
        assert!(rendered.contains("blocked by:"));
        assert!(rendered.contains("[a1]"));
        assert!(rendered.contains("[a3]"));
    }

    #[test]
    fn render_plan_with_mixed_statuses() {
        let mut plan = make_plan(vec![
            make_action("a1", &[]),
            make_action("a2", &["a1"]),
            make_action("a3", &["a1"]),
        ]);
        plan.actions[0].status = ActionStatus::Completed;
        plan.actions[1].status = ActionStatus::InProgress;
        plan.actions[2].status = ActionStatus::Skipped {
            reason: "dependency failed".into(),
        };

        let rendered = render_plan(&plan);
        assert!(rendered.contains("✅"));
        assert!(rendered.contains("🔄"));
        assert!(rendered.contains("⏭️"));
    }
}
