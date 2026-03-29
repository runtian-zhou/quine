You are creating a feature request for the quine project. Follow this workflow exactly:

## Step 1: Understand the Request

Read the user's feature description from the argument: $ARGUMENTS

If the description is vague, ask clarifying questions before proceeding.

## Step 2: Research the Codebase

Before writing any docs, explore the codebase to understand:
- Which crates and files are affected
- Existing patterns and types that should be reused
- Current architecture constraints from CLAUDE.md

Use Explore agents to search for relevant code. Read CLAUDE.md for conventions.

## Step 3: Create the Documentation Set

Determine the next feature number by finding the highest existing number in `features/` and adding 1. Use the same zero-padded `<NNN>` prefix and kebab-case `<name>` for all three files:

- Feature request: `features/<NNN>-<name>.md`
- Implementation plan: `features/plans/<NNN>-<name>-implementation.md`
- QA plan: `features/plans/<NNN>-<name>-qa.md`

Create `features/plans/` if it does not already exist.

### 3a. Write the feature request

Use this format:

```markdown
---
status: pending
---

# Feature Title

## Overview
Brief description of the feature and why it's needed.

## Requirements
### 1. Component/File Changes
Detailed requirements with code sketches where helpful.

## Acceptance Criteria
- cargo build/test/clippy/fmt must pass
- List specific unit tests required
- List integration tests required
- Existing tests must continue to pass

## QA Test Cases
Concrete QA cases that should prove the feature works end-to-end.

## Non-Goals (Deferred)
What is explicitly out of scope.
```

**Rules for the feature doc:**
- Be precise enough that an agent reading only CLAUDE.md and this file could implement it
- Reference existing types and patterns by file path
- Include concrete Rust type sketches where it clarifies the design
- Keep it focused to one logical feature

### 3b. Seed the two planning docs

Both planning docs must begin with:
- The feature title
- The feature request path
- A short summary of the request
- A `## Open Questions` section
- A `## Agreement Status` section

The implementation plan must also include:
- `## Proposed Design`
- `## File-by-File Changes`
- `## Validation Plan`
- `## QA Feedback`

The QA plan must also include:
- `## Test Strategy`
- `## Scenarios`
- `## Required Evidence`
- `## Implementation Feedback`

**Rules for the QA plan:**
- The QA agent must come up with concrete executable test scenarios, not only abstract validation bullets.
- Prefer scenarios that run against a real local daemon.
- When a scenario uses the one-off chat daemon or one-off chat CLI flow, the QA plan must explicitly list:
  - the exact command or commands to start the daemon and run the scenario
  - the exact text, prompt, or sequence of messages to send
  - the expected output for each command or interaction, including any expected final response text, status text, or error text
- For each scenario, specify:
  - how to start or use the local daemon
  - the exact one-off CLI invocation or message to send
  - the expected result, or the expected multi-round conversation when the scenario spans multiple interactions
- Scenarios should be concrete enough that another agent can execute them without inventing missing details.

## Step 4: Spawn Two Empty-Context Agents

After the docs exist, spawn exactly two agents:

- One implementor agent responsible for the implementation plan
- One QA agent responsible for the QA plan

**Critical coordination rules:**
- Spawn both agents with empty context. Do not fork or pass along the current conversation history.
- The only shared state between the two agents is the two markdown docs they maintain.
- Each agent may inspect the repository on its own, but any feedback to the other agent must be written into the docs.
- Do not summarize one agent's work to the other in chat. Route all coordination through the docs.

### Implementor agent responsibilities

- Read the feature request and both planning docs
- Produce a detailed implementation plan in `features/plans/<NNN>-<name>-implementation.md`
- Review the QA plan and leave concrete feedback in that file's `## Implementation Feedback` section
- Update the implementation doc's `## Agreement Status` section with either `pending` or `agreed`

### QA agent responsibilities

- Read the feature request and both planning docs
- Produce a detailed QA plan in `features/plans/<NNN>-<name>-qa.md`
- Review the implementation plan and leave concrete feedback in that file's `## QA Feedback` section
- Update the QA doc's `## Agreement Status` section with either `pending` or `agreed`

## Step 5: Check Agent Agreement and Drive It to Completion

Explicitly check whether the two agents agreed with each other by reading both planning docs and, if needed, the agents' latest outputs.

Do not proceed until agreement is complete.

Agreement is complete only when:
- The implementation doc says the QA plan is agreed
- The QA doc says the implementation plan is agreed
- Neither doc has unresolved open questions
- The current document state shows both agents are aligned on the same plan

If either agent raises a gap, wait for that agent to update its doc, then have the other agent review again. Keep the coordination loop going through the docs until both are agreed.

## Step 6: Create a PR

Only after Step 5 is complete and both agents have agreed with each other:

1. Create a feature branch: `feature-request-<name>`
2. Stage only the three markdown files:
   - `features/<NNN>-<name>.md`
   - `features/plans/<NNN>-<name>-implementation.md`
   - `features/plans/<NNN>-<name>-qa.md`
3. Commit with message: `Add feature request: <title>`
4. Verify the commit only contains the three markdown docs: `git diff --stat HEAD~1`
5. Push and create a PR with title `Feature request: <title>` and a brief summary body that mentions both agreed planning docs and states that agent agreement was verified before PR creation

## Step 7: Merge

Only after the PR is created from the agreed docs:

1. Merge the PR via `gh pr merge <number> --merge`
2. Switch back to main and pull.

**CRITICAL: Do not commit, create a PR, or merge anything until you have explicitly checked that both planning docs record agreement and that the two agents agreed with each other. Wait until agreement is visible in the docs before proceeding. The PR must contain only the feature request markdown and the two planning markdown docs. No code changes, no Cargo.lock updates, no other files. If `git status` shows unrelated modified files, do not stage them.**
