use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};
use crate::persistence::PersistedPlanStore;
use crate::planner::scheduler::{render_plan, skip_dependents, validate_dag};
use crate::planner::{Action, ActionId, ActionPlan, ActionStatus, PlanId};

/// Shared plan store for a session, mapping plan IDs to action plans.
pub type PlanStore = Arc<Mutex<HashMap<PlanId, ActionPlan>>>;

/// Create a new empty plan store.
pub fn new_plan_store() -> PlanStore {
    Arc::new(Mutex::new(HashMap::new()))
}

pub async fn snapshot_plan_store(plan_store: &PlanStore) -> PersistedPlanStore {
    let store = plan_store.lock().await;
    PersistedPlanStore::from_plans(store.values().cloned())
}

pub async fn restore_plan_store(snapshot: PersistedPlanStore) -> PlanStore {
    let store = new_plan_store();
    {
        let mut locked = store.lock().await;
        for plan in snapshot.plans {
            locked.insert(plan.plan_id, plan);
        }
    }
    store
}

/// Tool for creating and updating action plans.
pub struct PlanTool {
    plan_store: PlanStore,
}

impl PlanTool {
    pub fn new(plan_store: PlanStore) -> Self {
        Self { plan_store }
    }
}

/// Input for the create_plan operation.
#[derive(serde::Deserialize)]
struct CreatePlanInput {
    title: String,
    actions: Vec<CreatePlanAction>,
}

#[derive(serde::Deserialize)]
struct CreatePlanAction {
    id: String,
    title: String,
    description: String,
    #[serde(default)]
    depends_on: Vec<String>,
}

/// Input for the update_plan operation.
#[derive(serde::Deserialize)]
struct UpdatePlanInput {
    plan_id: String,
    action_id: String,
    status: String,
    #[serde(default)]
    result: Option<String>,
    /// Error message when status is "failed".
    #[serde(default)]
    error: Option<String>,
    /// Reason when status is "skipped".
    #[serde(default)]
    reason: Option<String>,
}

#[async_trait]
impl Tool for PlanTool {
    fn name(&self) -> &str {
        "plan"
    }

    fn description(&self) -> &str {
        "Create or update an action plan with DAG-based dependencies. \
         Use operation 'create_plan' to create a new plan, or 'update_plan' \
         to update the status of an action in an existing plan."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["create_plan", "update_plan"],
                    "description": "The operation to perform"
                },
                "title": {
                    "type": "string",
                    "description": "Title for the plan (create_plan only)"
                },
                "actions": {
                    "type": "array",
                    "description": "Array of actions (create_plan only)",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "title": { "type": "string" },
                            "description": { "type": "string" },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "string" },
                                "default": []
                            }
                        },
                        "required": ["id", "title", "description"]
                    }
                },
                "plan_id": {
                    "type": "string",
                    "description": "Plan ID (update_plan only)"
                },
                "action_id": {
                    "type": "string",
                    "description": "Action ID to update (update_plan only)"
                },
                "status": {
                    "type": "string",
                    "enum": ["in_progress", "completed", "failed", "skipped"],
                    "description": "New status for the action (update_plan only)"
                },
                "result": {
                    "type": "string",
                    "description": "Result summary (update_plan only, optional)"
                },
                "error": {
                    "type": "string",
                    "description": "Error message when status is failed (update_plan only)"
                },
                "reason": {
                    "type": "string",
                    "description": "Reason when status is skipped (update_plan only)"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        _context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let operation = arguments
            .get("operation")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidArguments {
                message: "missing 'operation' field".into(),
            })?;

        match operation {
            "create_plan" => self.handle_create_plan(&arguments).await,
            "update_plan" => self.handle_update_plan(&arguments).await,
            other => Err(ToolError::InvalidArguments {
                message: format!("unknown operation: {other}"),
            }),
        }
    }
}

impl PlanTool {
    async fn handle_create_plan(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let input: CreatePlanInput =
            serde_json::from_value(arguments.clone()).map_err(|e| ToolError::InvalidArguments {
                message: format!("invalid create_plan arguments: {e}"),
            })?;

        let plan_id = PlanId::new();
        let actions: Vec<Action> = input
            .actions
            .into_iter()
            .map(|a| Action {
                action_id: ActionId::new(&a.id),
                title: a.title,
                description: a.description,
                depends_on: a.depends_on.into_iter().map(ActionId::new).collect(),
                status: ActionStatus::Pending,
                result: None,
            })
            .collect();

        let plan = ActionPlan {
            plan_id,
            title: input.title,
            actions,
        };

        // Validate the DAG
        validate_dag(&plan).map_err(|e| ToolError::InvalidArguments {
            message: format!("invalid plan: {e}"),
        })?;

        let rendered = render_plan(&plan);

        let mut store = self.plan_store.lock().await;
        store.insert(plan_id, plan);

        Ok(ToolOutput::success(format!(
            "Plan created (ID: {plan_id})\nplan_id: {plan_id}\n\n{rendered}"
        )))
    }

    async fn handle_update_plan(
        &self,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        let input: UpdatePlanInput =
            serde_json::from_value(arguments.clone()).map_err(|e| ToolError::InvalidArguments {
                message: format!("invalid update_plan arguments: {e}"),
            })?;

        let raw_plan_id = input.plan_id.trim();
        if raw_plan_id.starts_with("call_") {
            return Err(ToolError::InvalidArguments {
                message: format!(
                    "invalid plan_id: got tool call id `{raw_plan_id}`; use the UUID returned by `create_plan` in the tool result, not the `call_...` invocation id"
                ),
            });
        }

        let plan_id: PlanId = raw_plan_id.parse().map_err(|e| ToolError::InvalidArguments {
            message: format!(
                "invalid plan_id: {e}. Use the UUID returned by `create_plan`, not the `call_...` tool invocation id"
            ),
        })?;

        let action_id = ActionId::new(&input.action_id);

        let mut store = self.plan_store.lock().await;
        let plan = store
            .get_mut(&plan_id)
            .ok_or_else(|| ToolError::InvalidArguments {
                message: format!("plan not found: {plan_id}"),
            })?;

        let action =
            plan.get_action_mut(&action_id)
                .ok_or_else(|| ToolError::InvalidArguments {
                    message: format!("action not found: {}", input.action_id),
                })?;

        // Update the action status
        let new_status = match input.status.as_str() {
            "in_progress" => ActionStatus::InProgress,
            "completed" => ActionStatus::Completed,
            "failed" => ActionStatus::Failed {
                error: input.error.unwrap_or_else(|| "unknown error".into()),
            },
            "skipped" => ActionStatus::Skipped {
                reason: input.reason.unwrap_or_else(|| "skipped".into()),
            },
            other => {
                return Err(ToolError::InvalidArguments {
                    message: format!("invalid status: {other}"),
                });
            }
        };

        action.status = new_status.clone();
        if let Some(result) = input.result {
            action.result = Some(result);
        }

        // If failed, propagate skips to dependents
        if matches!(new_status, ActionStatus::Failed { .. }) {
            skip_dependents(plan, &action_id);
        }

        let rendered = render_plan(plan);

        Ok(ToolOutput::success(format!(
            "Action [{action_id}] updated to {}\n\n{rendered}",
            new_status.label()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::scheduler::get_ready_actions;
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::session::SessionId;

    fn make_context() -> ExecutionContext {
        ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(crate::filesystem::NullFilesystem),
            working_directory: PathBuf::from("/tmp"),
            interaction_channel: None,
            plan_store: new_plan_store(),
            session_group: String::new(),
            python_runtime: crate::python::PythonRuntime::new(),
            core_input: None,
            permission_runtime: None,
            cancellation: crate::tool::CancellationChannel::never(),
        }
    }

    #[tokio::test]
    async fn create_plan_success() {
        let store = new_plan_store();
        let tool = PlanTool::new(store.clone());
        let ctx = make_context();

        let args = serde_json::json!({
            "operation": "create_plan",
            "title": "Test Plan",
            "actions": [
                {"id": "a1", "title": "Step 1", "description": "Do thing 1", "depends_on": []},
                {"id": "a2", "title": "Step 2", "description": "Do thing 2", "depends_on": ["a1"]}
            ]
        });

        let result = tool.execute(args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("Plan created"));
        let created_plan_id = {
            let store_locked = store.lock().await;
            let plan_id = store_locked.keys().next().unwrap().to_string();
            drop(store_locked);
            plan_id
        };
        assert!(result
            .content
            .contains(&format!("plan_id: {created_plan_id}")));
        assert!(result.content.contains("Test Plan"));
        let store_locked = store.lock().await;
        assert_eq!(store_locked.len(), 1);
    }

    #[tokio::test]
    async fn update_plan_with_tool_call_id_gives_helpful_error() {
        let tool = PlanTool::new(new_plan_store());
        let ctx = make_context();

        let args = serde_json::json!({
            "operation": "update_plan",
            "plan_id": "call_abc123",
            "action_id": "a1",
            "status": "completed"
        });

        let result = tool.execute(args, &ctx).await;
        match result {
            Err(ToolError::InvalidArguments { message }) => {
                assert!(message.contains("got tool call id `call_abc123`"));
                assert!(message.contains("UUID returned by `create_plan`"));
            }
            other => panic!("expected InvalidArguments error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn create_plan_with_cycle_fails() {
        let store = new_plan_store();
        let tool = PlanTool::new(store);
        let ctx = make_context();

        let args = serde_json::json!({
            "operation": "create_plan",
            "title": "Cyclic",
            "actions": [
                {"id": "a1", "title": "One", "description": "d", "depends_on": ["a2"]},
                {"id": "a2", "title": "Two", "description": "d", "depends_on": ["a1"]}
            ]
        });

        let result = tool.execute(args, &ctx).await;
        match result {
            Err(ToolError::InvalidArguments { message }) => {
                assert!(message.contains("cycle"));
            }
            other => panic!("expected InvalidArguments, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_plan_success() {
        let store = new_plan_store();
        let tool = PlanTool::new(store.clone());
        let ctx = make_context();

        // Create a plan first
        let create_args = serde_json::json!({
            "operation": "create_plan",
            "title": "Test",
            "actions": [
                {"id": "a1", "title": "Step 1", "description": "d", "depends_on": []},
                {"id": "a2", "title": "Step 2", "description": "d", "depends_on": ["a1"]}
            ]
        });
        tool.execute(create_args, &ctx).await.unwrap();

        // Get the plan ID
        let plan_id = {
            let s = store.lock().await;
            s.keys().next().unwrap().to_string()
        };

        // Update a1 to completed
        let update_args = serde_json::json!({
            "operation": "update_plan",
            "plan_id": plan_id,
            "action_id": "a1",
            "status": "completed",
            "result": "Done successfully"
        });

        let result = tool.execute(update_args, &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("completed"));

        // Verify a2 is now ready
        let s = store.lock().await;
        let plan = s.values().next().unwrap();
        let ready = get_ready_actions(plan);
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].action_id.as_str(), "a2");
    }

    #[tokio::test]
    async fn update_plan_failure_propagates_skips() {
        let store = new_plan_store();
        let tool = PlanTool::new(store.clone());
        let ctx = make_context();

        let create_args = serde_json::json!({
            "operation": "create_plan",
            "title": "Test",
            "actions": [
                {"id": "a1", "title": "Step 1", "description": "d", "depends_on": []},
                {"id": "a2", "title": "Step 2", "description": "d", "depends_on": ["a1"]},
                {"id": "a3", "title": "Step 3", "description": "d", "depends_on": ["a2"]}
            ]
        });
        tool.execute(create_args, &ctx).await.unwrap();

        let plan_id = {
            let s = store.lock().await;
            s.keys().next().unwrap().to_string()
        };

        // Fail a1
        let update_args = serde_json::json!({
            "operation": "update_plan",
            "plan_id": plan_id,
            "action_id": "a1",
            "status": "failed",
            "error": "something broke"
        });
        tool.execute(update_args, &ctx).await.unwrap();

        // Verify a2 and a3 are skipped
        let s = store.lock().await;
        let plan = s.values().next().unwrap();
        assert!(matches!(
            plan.actions[1].status,
            ActionStatus::Skipped { .. }
        ));
        assert!(matches!(
            plan.actions[2].status,
            ActionStatus::Skipped { .. }
        ));
    }

    #[tokio::test]
    async fn unknown_operation_fails() {
        let store = new_plan_store();
        let tool = PlanTool::new(store);
        let ctx = make_context();

        let args = serde_json::json!({ "operation": "delete_plan" });
        let result = tool.execute(args, &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }

    #[tokio::test]
    async fn update_nonexistent_plan_fails() {
        let store = new_plan_store();
        let tool = PlanTool::new(store);
        let ctx = make_context();

        let args = serde_json::json!({
            "operation": "update_plan",
            "plan_id": "00000000-0000-0000-0000-000000000000",
            "action_id": "a1",
            "status": "completed"
        });
        let result = tool.execute(args, &ctx).await;
        assert!(matches!(result, Err(ToolError::InvalidArguments { .. })));
    }
}
