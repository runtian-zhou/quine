You are creating feature planning docs for the quine project. Follow this workflow exactly:

## Step 1: Understand the Request

Read the user's feature description from the argument: $ARGUMENTS

If the description is vague, ask clarifying questions before proceeding.

## Step 2: Research the Codebase

Before writing any docs, explore the codebase to understand:
- Which crates and files are affected
- Which existing patterns and types should be reused
- Which architecture constraints from `CLAUDE.md` apply

Use Explore agents to search for relevant code. Read `CLAUDE.md` for conventions.

If research reveals any remaining clarification questions for the user, stop and ask them before proceeding. Do not continue to Step 3, and do not make assumptions after Step 2 when user clarification is still needed.

## Step 3: Create the Planning Docs and Spawn Two Context-Inheriting Agents

Determine the next feature number by finding the highest existing number in `features/` and adding 1. Use the same zero-padded `<NNN>` prefix and kebab-case `<name>` for both files:

- Implementation plan: `features/plans/<NNN>-<name>-implementation.md`
- QA plan: `features/plans/<NNN>-<name>-qa.md`

Create `features/plans/` if it does not already exist.

Create the two planning docs first. Then spawn exactly two separate agents that both inherit the current context:

- One implementer agent for the implementation plan
- One QA agent for the QA plan

**Critical coordination rules:**
- Spawn both agents with the current conversation context so they can use the user's request, research, and clarified constraints directly.
- The two agents must develop their plans independently.
- The implementer agent owns the implementation plan doc, and the QA agent owns the QA plan doc.
- Each agent may inspect the repository on its own, but feedback to the other agent must be written into the docs.
- Do not summarize one agent's work to the other in chat. Route coordination through the docs.

Both planning docs must begin with:
- The feature title
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
- The QA agent must define concrete, executable test scenarios, not just abstract validation bullets.
- Prefer scenarios that run against a real local daemon.
- If the change affects `quine-core`, the QA plan must include at least one multi-round test that interacts with a local chat daemon.
- For any required multi-round daemon test, the QA plan must explicitly specify:
  - the exact command or commands to start the local daemon and connect to it
  - the exact chat messages to send, round by round
  - the exact expected response for each round, including final response text, status text, tool activity, and error text where applicable
- When a scenario uses the one-off chat daemon or one-off chat CLI flow, the QA plan must explicitly list:
  - the exact command or commands to start the daemon and run the scenario
  - the exact text, prompt, or sequence of messages to send
  - the expected output for each command or interaction, including any expected final response text, status text, or error text
- For each scenario, specify:
  - how to start or use the local daemon
  - the exact one-off CLI invocation or message to send
  - the expected result, or the expected multi-round conversation when the scenario spans multiple interactions
- Scenarios must be concrete enough that another agent can execute them without inventing missing details.

### Implementer agent responsibilities

- Produce a detailed implementation plan in `features/plans/<NNN>-<name>-implementation.md`
- Read the QA plan and leave concrete feedback in its `## Implementation Feedback` section
- Coordinate with the QA agent through the planning docs and actively drive the discussion until both agents agree
- Revisit the QA plan after each material update, resolve open questions, and update the implementation doc's `## Agreement Status` section to either `pending` or `agreed`

### QA agent responsibilities

- Produce a detailed QA plan in `features/plans/<NNN>-<name>-qa.md`
- Read the implementation plan and leave concrete feedback in its `## QA Feedback` section
- Coordinate with the implementer agent through the planning docs and actively drive the discussion until both agents agree
- Revisit the implementation plan after each material update, resolve open questions, and update the QA doc's `## Agreement Status` section to either `pending` or `agreed`

## Step 4: Check Agreement and Drive It to Completion

Confirm whether the two agents have reached agreement by reading both planning docs and, if needed, their latest outputs.

Do not proceed until agreement is complete.

Agreement is complete only when:
- The implementation doc says the QA plan is agreed
- The QA doc says the implementation plan is agreed
- Neither doc has unresolved open questions
- Both docs show the same agreed plan
- Each agent has reviewed the other planning doc's latest revision before marking its own doc as `agreed`

If either agent identifies a gap, wait for that agent to update its doc, then have the other agent review again. Keep the coordination loop going through the docs until both are agreed. Both agents must continue the back-and-forth until alignment is reached. Neither agent may mark `agreed` until it has reviewed the other doc's latest revision.

## Step 5: Create a PR

Only after Step 4 is complete and both agents have agreed with each other:

1. Create a feature branch: `feature-request-<name>`
2. Stage only the two markdown files:
   - `features/plans/<NNN>-<name>-implementation.md`
   - `features/plans/<NNN>-<name>-qa.md`
3. Commit with message: `Add feature planning docs: <title>`
4. Verify the commit only contains the two markdown docs: `git diff --stat HEAD~1`
5. Push and create a PR with title `Feature planning docs: <title>` and a brief summary body that mentions both agreed planning docs and states that agent agreement was verified before PR creation

## Step 6: Merge

Only after the PR is created from the agreed docs:

1. Merge the PR via `gh pr merge <number> --merge`
2. Switch back to main and pull.

**CRITICAL: Do not commit, create a PR, or merge anything until you have explicitly checked that both planning docs record agreement and that the two agents agreed with each other. Wait until agreement is visible in the docs before proceeding. The PR must contain only the two planning markdown docs. No code changes, no Cargo.lock updates, no other files. If `git status` shows unrelated modified files, do not stage them.**
