# Spawn vs Subagent Smoke Guide

Feature: `features/032-spawn-subagent-qa-smoke.md`

Use this note when you need a fast manual check of the intended split between the two delegation tools.

## What to verify

- `subagent` runs a delegated task inline and returns the child result in the same tool call.
- `spawn` returns a child session identifier and leaves follow-up coordination to later steps such as wait, signal, or message passing.

## Suggested prompts

### Inline delegation with `subagent`

Ask the agent to delegate a small task and report the final result, for example:

```text
Use the subagent tool to delegate this task: "Use bash to run echo SUBAGENT_SMOKE_OK and report the output." Return only the delegated result.
```

Expected outcome:

- The parent response includes the delegated result directly.
- There is no intermediate child session identifier in the final answer.

### Child-session creation with `spawn`

Ask the agent to create a child for a simple task and report the returned handle, for example:

```text
Use the spawn tool to create a child agent with this task: "Say exactly SPAWN_SMOKE_OK". Return the child session identifier you receive.
```

Expected outcome:

- The tool returns a child session identifier rather than the final child result.
- Any later observation of child completion must happen through separate coordination steps.

## Automated evidence in this change

- `cargo test -p quine-core spawn_`
- `cargo test -p quine-core subagent_records_assistant_tool_use_before_tool_result`

These cover the `spawn` success and missing-channel failure paths plus the subagent tool-call history contract that previously blocked delegated execution.
