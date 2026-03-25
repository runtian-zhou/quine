---
status: done
---

# Action Planner Tool with DAG-based Execution

## Overview

Add an action planner tool that allows the LLM to decompose complex tasks into a directed acyclic graph (DAG) of actions with explicit dependencies. Once a plan is created, the engine prompts the agent to execute each action in dependency order, enabling parallel execution of independent actions and structured progress tracking.

## Requirements

### 1. Plan Data Model (`quine-core/src/planner/mod.rs`)

```rust
pub struct ActionPlan {
    pub plan_id: PlanId,
    pub title: String,
    pub actions: Vec<Action>,
}

pub struct PlanId(Uuid);

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

pub struct ActionId(String);  // human-readable like "a1", "a2", "a3"

pub enum ActionStatus {
    Pending,
    InProgress,
    Completed,
    Failed { error: String },
    Skipped { reason: String },
}
```

### 2. DAG Validation and Scheduling (`quine-core/src/planner/scheduler.rs`)

- **`validate_dag(plan: &ActionPlan) -> Result<(), PlanError>`**: Check for cycles (topological sort), missing dependency references, duplicate action IDs.
- **`get_ready_actions(plan: &ActionPlan) -> Vec<&Action>`**: Return actions whose dependencies are all `Completed` and whose own status is `Pending`. These are the next actions eligible for execution.
- **`PlanError`**: thiserror enum — `CycleDetected`, `MissingDependency { action_id, depends_on }`, `DuplicateActionId`.

### 3. PlanTool (`quine-core/src/tool/plan.rs`)

A tool the LLM can invoke to create or update a plan:

**`create_plan` tool**: LLM provides a JSON plan:
```json
{
  "title": "Implement feature X",
  "actions": [
    { "id": "a1", "title": "Read existing code", "description": "Read foo.rs to understand current implementation", "depends_on": [] },
    { "id": "a2", "title": "Read tests", "description": "Read foo_test.rs", "depends_on": [] },
    { "id": "a3", "title": "Implement changes", "description": "Modify foo.rs to add bar()", "depends_on": ["a1", "a2"] },
    { "id": "a4", "title": "Write tests", "description": "Add tests for bar()", "depends_on": ["a3"] },
    { "id": "a5", "title": "Run tests", "description": "cargo test", "depends_on": ["a4"] }
  ]
}
```

The tool validates the DAG, stores the plan in `SessionContext`, and returns a formatted summary showing the dependency graph.

**`update_plan` tool**: LLM can mark actions as completed/failed/skipped and add new actions to an existing plan.

### 4. Plan Execution Loop (Engine Integration)

After the LLM creates a plan via `create_plan`, the engine enters a **plan execution mode** for that session:

1. Call `get_ready_actions()` to find executable actions.
2. For each ready action, inject a system-level message to the LLM: _"Execute the next action from your plan: [{action_id}] {title} — {description}. When done, use the `update_plan` tool to mark it as completed with a result summary."_
3. The LLM executes the action using available tools (read, write, bash, etc.) and calls `update_plan` to mark completion.
4. After each `update_plan` call, check for newly ready actions and repeat.
5. When all actions are `Completed`, `Failed`, or `Skipped`, emit a plan completion summary.
6. If an action fails and downstream actions depend on it, those dependents are automatically marked `Skipped { reason: "dependency failed" }`.

**Important**: The plan execution loop is driven by prompting the LLM, not by hard-coded logic. The engine just manages the scheduling and injects the right prompts. The LLM decides how to execute each action.

### 5. Plan Status in CoreOutput

Add a new `CoreOutput` variant for plan progress:

```rust
CoreOutput::PlanProgress {
    session_id: SessionId,
    plan_id: PlanId,
    action_id: ActionId,
    status: ActionStatus,
    /// Summary of remaining actions.
    remaining: usize,
    total: usize,
}
```

### 6. Plan Display

The `create_plan` tool output should render the DAG in a readable format:

```
Plan: Implement feature X (5 actions)

  [a1] Read existing code          (ready)
  [a2] Read tests                  (ready)
  [a3] Implement changes           (blocked by: a1, a2)
  [a4] Write tests                 (blocked by: a3)
  [a5] Run tests                   (blocked by: a4)
```

### 7. File Structure

```
crates/quine-core/src/
  planner/
    mod.rs          # ActionPlan, Action, ActionId, PlanId, ActionStatus
    scheduler.rs    # DAG validation, topological sort, get_ready_actions
  tool/
    plan.rs         # PlanTool (create_plan, update_plan)
```

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Unit tests for DAG validation: valid DAG passes, cycle detected, missing dependency.
- Unit tests for `get_ready_actions`: returns correct actions based on completion state.
- Unit tests for `PlanTool`: create plan, update action status.
- Unit tests for dependency failure propagation (skipping downstream actions).
- Existing tests continue to pass.
