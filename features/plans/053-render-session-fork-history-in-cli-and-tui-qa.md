# Render Session Fork History in CLI and TUI

Short summary: verify that session fork ancestry is preserved by the CLI-visible session model and rendered consistently in both the textual CLI and the interactive TUI.

## Open Questions

- None at this time.

## Agreement Status

agreed — reviewed against the implementation plan; both docs align on the additive ancestry metadata shape, shared CLI/TUI tree reconstruction, real-daemon validation, and compatibility coverage, with no unresolved open questions.

## Test Strategy

- Validate the feature at three layers: protocol/state unit coverage for ancestry metadata, CLI rendering coverage for tree output, and end-to-end daemon-driven verification that real forked sessions appear correctly in both CLI and TUI flows.
- Prefer real local daemon scenarios that create actual parent/child session relationships instead of only fixture-based rendering tests.
- Compare CLI and TUI behavior against the same forked-session data set so regressions in one surface are visible.
- Include negative/compatibility coverage for sessions without parents, missing ancestors, and sibling ordering so rendering remains stable for older or partial state.

## Scenarios

- **Scenario 1 — Unit: session ancestry reconstruction**
  - Start/use local daemon: not required.
  - Exact command: `cargo test -p quine-core session_tree -- --nocapture`
  - Expected result: tests covering fork ancestry serialization/reconstruction pass; output contains no failures and shows that root, parent, and sibling relationships are reconstructed deterministically.

- **Scenario 2 — Unit: CLI tree rendering helper**
  - Start/use local daemon: not required.
  - Exact command: `cargo test -p quine-cli session -- --nocapture`
  - Expected result: tests covering tree formatting pass and assert exact rendered lines for a root session, at least two fork children, and a deeper grandchild branch; output contains no failures.

- **Scenario 3 — One-off CLI: list fork history from real daemon state**
  - Start local daemon: use the project’s existing local daemon startup flow for CLI/manual testing, for example one terminal running the harness daemon command already used by the repo for local development.
  - Create forked sessions: use the CLI flow that creates or resumes one session, then forks it into at least two child sessions, with one of those children forked again into a grandchild. Record the resulting session IDs.
  - Exact command(s): run the existing non-interactive session-list/session-log CLI command(s) that should expose fork history after implementation.
  - Exact text/messages to send: none beyond the CLI invocations needed to create the sessions and request the session listing.
  - Expected result: CLI output includes the root session and its descendants in a visible tree/history form, with indentation or branch markers showing parent-child relationships; siblings appear in deterministic order; the current or selected session is clearly marked if the command already supports that concept; no sessions are omitted from the fork chain.

- **Scenario 4 — Multi-round daemon chat: fork history survives real conversation branches**
  - Start local daemon and connect: run the standard local daemon command for the harness, then connect with the CLI chat command, e.g. the same `cargo run --bin quine -- chat` flow used for manual daemon-backed chat in this repo.
  - Round 1 message: send `Remember the token ALPHA and reply only with ALPHA acknowledged.`
  - Expected round 1 result: final response text is exactly `ALPHA acknowledged.` or the closest existing deterministic equivalent produced by the harness prompt constraints; session status indicates success; no tool activity is required; no error text is present.
  - Round 2 action: create a fork from that session using the CLI/session-management flow introduced or already available for forking.
  - Round 2 message on fork A: send `What token did I ask you to remember? Reply in one line.`
  - Expected round 2 result: final response text includes `ALPHA`; session status indicates success; no unexpected errors are present.
  - Round 3 action: create a second fork from the original parent session.
  - Round 3 message on fork B: send `Reply with BRANCH-B.`
  - Expected round 3 result: final response text is `BRANCH-B.` or the exact repo-appropriate equivalent; session status indicates success; no unexpected errors are present.
  - Verification command: run the CLI session history/list command after the above rounds.
  - Expected verification result: output shows the original session as the parent of both fork A and fork B, and shows any subsequent descendant under the correct branch; statuses remain successful and no lineage is swapped.

- **Scenario 5 — TUI manual verification: fork tree is browsable**
  - Start/use local daemon: reuse the daemon state created in Scenario 4.
  - Exact command: run `cargo run --bin quine -- chat` (or the existing TUI entry flow if separate) and open the session browser/sidebar that lists sessions.
  - Exact interactions: navigate to the session list, inspect the root session, expand/select its forked descendants if the UI is collapsible, and switch selection between sibling branches.
  - Expected result: the TUI displays the same fork hierarchy seen in CLI output, with clear indentation/branch affordances, stable ordering, and selection/highlight behavior that makes the active branch obvious; navigation does not panic or redraw incorrectly.

- **Scenario 6 — Compatibility: orphan or legacy sessions render safely**
  - Start/use local daemon: optional; may be a unit/integration fixture.
  - Exact command: run the targeted test covering a session whose parent metadata references a missing ancestor.
  - Expected result: rendering still succeeds, the affected session appears in a fallback top-level position or documented degraded form, and the command/test produces no panic or crash.

## Required Evidence

- Test logs for targeted `quine-core` and `quine-cli` test commands.
- CLI transcript or captured output showing a real forked session tree from the daemon-backed session-list/history command.
- Manual verification notes for the TUI view, including which sessions were created and how the tree appeared.
- If deterministic exact output is difficult because commands differ from assumptions, the implementation must update this plan with the final exact commands and expected output before execution begins.

## Implementation Feedback

- The QA plan should verify the exact session-management commands present in the repo before execution and then lock those commands into the evidence runbook so the manual daemon scenarios are fully reproducible.
- The real-daemon scenarios correctly cover both branch creation and a deeper descendant; that matches the implementation plan’s requirement to preserve root, parent, and sibling ordering through the harness protocol.
- The compatibility scenario for missing ancestors is important and should stay in scope because the implementation intends to render degraded data safely instead of failing.
