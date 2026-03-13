# Phase 5: Review Loop

This is the core quality gate. Run the following cycle and repeat until everything is clean.

Before starting, read `.workflow-state.json` to check if a previous attempt was interrupted mid-loop. If `current_step` is `"review-loop:<step>"`, resume from that step instead of starting over. If `review_findings` has unresolved items, address those first.

Update `.workflow-state.json` at each step transition so progress is never lost.

## Step 1: Format and Lint

Update state: `current_step` → `"review-loop:fmt-lint"`

```bash
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
```

Fix any issues before proceeding. Clippy warnings should be resolved by fixing the code, not by adding `#[allow(...)]` attributes (unless there's a genuine false positive — explain why in a comment).

Record any findings in `review_findings` in the state file.

## Step 2: Run Tests

Update state: `current_step` → `"review-loop:test"`

```bash
cargo test --all-features
```

All tests must pass. If a test fails, diagnose and fix it. If a test is flaky, investigate the root cause rather than re-running and hoping it passes.

Record any failures in `review_findings`.

## Step 3: Code Review

Update state: `current_step` → `"review-loop:code-review"`

Use the `/review` command to perform a code review of the changes on the feature branch. This gives a structured, independent review of the diff rather than relying on self-assessment.

After running `/review`, record its findings in `review_findings` in the state file. Fix any issues it surfaces. If fixes are made, go back to Step 1 to re-run format, lint, and tests. Increment `review_iteration`.

## Exit Criteria

The loop ends when ALL of these are true:

- `cargo fmt` produces no changes
- `cargo clippy` produces no warnings
- `cargo test` passes
- `/review` surfaces no issues (or all issues have been addressed)

When exit criteria are met, clear `review_findings` in the state file.

## Step 4: Present to User

Update state: `current_step` → `"review-loop:present-to-user"`

Once the review loop is clean, present a summary to the user:

- What was implemented (brief)
- What tests were added
- Any deviations from the original design and why
- Any open questions or follow-ups

Wait for the user's feedback. If they request changes, make them and re-enter the loop from Step 1. Repeat until the user approves.

## Output

User-approved code that passes all quality checks. State file updated with `current_phase` → `6`.
