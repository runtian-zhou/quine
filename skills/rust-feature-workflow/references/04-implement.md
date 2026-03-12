# Phase 4: Implement the Feature

Write the implementation following the approved design doc. This phase produces the actual code and tests.

## Guidelines

- **Follow existing project conventions.** Before writing code, read nearby files to understand the project's style — naming, module organization, error handling patterns, use of `Result` vs `unwrap`, etc. Match what's already there.

- **Write tests alongside the implementation.** Don't leave tests for later. Each new public function or behavior change should have corresponding tests. Prefer unit tests in `#[cfg(test)]` modules; use integration tests in `tests/` for cross-module behavior.

- **Keep commits logical.** Each commit should represent a coherent change. Don't lump unrelated changes together.

- **Update the design doc status** to "Implemented" once the code is written — both in the doc's `## Status` field and in the `docs/design/README.md` index table.

## Test Expectations

- Always assert on exact values with explanatory messages, not range checks.
- Cover both happy paths and error paths.
- Test edge cases identified in the design doc's Testing Plan.

## Output

Working code on the feature branch with tests, ready for the Review Loop.
