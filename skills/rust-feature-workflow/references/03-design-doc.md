# Phase 3: Write the Design Doc

This phase runs inside the feature's worktree (created in Phase 2). All file paths below are relative to the worktree root.

Every feature gets a design document persisted in the repo. These docs form a living record of architectural decisions — they stay in the repo permanently so future contributors can understand why things were built the way they were.

## Design Index

The project maintains a design index at `docs/design/README.md`. This file lists all design docs and their current status. If it doesn't exist yet, create it:

```markdown
# Design Documents

| Feature | Status | Date | Doc |
|---------|--------|------|-----|
```

When creating a new design doc, add a row to this table with status `Draft`.

## Individual Design Doc

Create the design document at `docs/design/<feature-name>.md` using the template below.

### Template

```markdown
# Design: <Feature Name>

## Status
Draft

## Date
<YYYY-MM-DD>

## Summary
One paragraph describing what this feature does and why.

## Motivation
Why is this feature needed? What problem does it solve?

## Design

### Public API Changes
Describe any new or modified public types, traits, functions, or modules.

### Internal Design
How the feature works internally. Key data structures, algorithms,
and module interactions.

### Error Handling
What errors can occur and how they're reported to the caller.

## Alternatives Considered
At least one alternative approach and why it was rejected.

## Testing Plan
What tests will be written — unit tests, integration tests,
doc tests. Describe the key scenarios to cover.

## Unresolved Questions
Open questions that can be resolved during implementation.
```

## Status Lifecycle

Design docs move through these statuses:

| Status | Meaning | When to set |
|--------|---------|-------------|
| **Draft** | Initial write-up, under discussion | Phase 3: when first created |
| **Approved** | User has signed off on the design | Phase 3: after user approval |
| **Implemented** | Code is written, tests pass | Phase 4: after implementation is complete |
| **Merged** | Feature has landed on main | Phase 6: after merge |

Update both the individual doc's `## Status` field and the corresponding row in `docs/design/README.md` whenever the status changes.

## Process

1. Read `docs/design/README.md` to see existing designs (if it exists). This gives you context on prior decisions and avoids conflicts.
2. Fill in the template based on the confirmed feature summary from Phase 1.
3. Add a row to the design index with status `Draft`.
4. Present the design doc to the user.
5. Wait for approval before moving to implementation.
6. If the user requests changes, update the doc and re-present.
7. Once approved, update status to `Approved` in both the doc and the index.

## Output

- An approved `docs/design/<feature-name>.md` file persisted in the repo.
- An updated `docs/design/README.md` index with the new entry.
