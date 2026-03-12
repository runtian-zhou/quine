---
name: rust-feature-workflow
description: >
  End-to-end workflow for implementing features in open-source Rust projects.
  Takes a feature request from the user, produces a design doc, implements
  the feature on a branch, runs code review and tests in a loop until
  everything passes, then merges. Use this skill whenever the user wants to
  add a feature to a Rust/Cargo project and wants a structured process —
  even if they just say "add X" or "implement Y" without mentioning a
  workflow. Also triggers for phrases like "feature request", "new feature",
  "design and implement", or "full workflow" in the context of a Rust codebase.
---

# Rust Feature Workflow

A structured, repeatable workflow for taking a feature request from idea to merged code in an open-source Rust project. Each phase is defined in its own reference file so it can be reviewed, tested, and improved independently.

## Phases

Execute these in order. Read the reference file for the current phase before starting it.

| Phase | Reference | Purpose |
|-------|-----------|---------|
| 1. Clarify | `references/01-clarify.md` | Understand the feature request, confirm scope with user |
| 2. Branch | `references/02-branch.md` | Create a feature branch and worktree |
| 3. Design | `references/03-design-doc.md` | Write and persist a design doc, get user approval |
| 4. Implement | `references/04-implement.md` | Write the code and tests |
| 5. Review | `references/05-review-loop.md` | Lint, test, self-review, fix; repeat until clean |
| 6. Merge | `references/06-merge.md` | Merge the branch into main |

## How to Use

1. **Check for existing state first.** Look for `.workflow-state.json` — either in the current directory or in any active worktree (`git worktree list`). If one exists for the requested feature, resume from the recorded phase and step. Tell the user what you're resuming.
2. If no state file exists, start at Phase 1. Read `references/01-clarify.md` and follow its instructions.
3. When a phase produces its output (each reference file defines what that is), update `.workflow-state.json` and move to the next phase.
4. Phases 3 and 5 involve user approval gates — do not proceed until the user confirms.
5. If the user wants to skip a phase (e.g., "just implement it, no design doc needed"), respect that, but mention what they're skipping.
6. When the workflow completes (Phase 6 done), delete `.workflow-state.json` and remove the worktree.

## Workflow State File

The file `.workflow-state.json` tracks progress so the workflow can resume after an interruption. During Phase 1 (clarify), it lives in the main repo root. After Phase 2 (worktree creation), it lives in the worktree directory. Create it at the start of Phase 1, update it at every phase/step transition, and delete it when the workflow completes. Add `.workflow-state.json` to `.gitignore` — it's local working state, not something to commit.

```json
{
  "feature_name": "json-output-flag",
  "branch": "feat/json-output-flag",
  "worktree_path": "../worktrees/json-output-flag",
  "current_phase": 5,
  "current_step": "review-loop:fmt-lint",
  "review_iteration": 2,
  "design_doc_path": "docs/design/json-output-flag.md",
  "review_findings": [
    "clippy: unnecessary clone on line 42",
    "/review: missing doc comment on OutputWriter::new"
  ],
  "updated_at": "2026-03-12T10:30:00Z"
}
```

Field reference:
- **feature_name**: Kebab-case name of the feature.
- **branch**: The git branch for this feature.
- **worktree_path**: Path to the git worktree for this feature (relative to repo root).
- **current_phase**: Which phase (1-6) the workflow is on.
- **current_step**: More specific position within a phase. Use phase-name for simple phases (e.g., `"clarify"`, `"design-doc"`, `"branch"`, `"implement"`). For the review loop, use `"review-loop:<step>"` where step is `fmt-lint`, `test`, `code-review`, or `present-to-user`.
- **review_iteration**: How many times the review loop has cycled. Starts at 1.
- **design_doc_path**: Path to the design doc for this feature.
- **review_findings**: List of unresolved findings from the current review iteration. Clear this list when all findings are fixed.
- **updated_at**: ISO 8601 timestamp of the last update.

## Parallel Features via Worktrees

Phase 2 creates a git worktree for each feature, so multiple features can be in-flight simultaneously without conflicts. Each worktree is an independent working directory with its own branch checked out. The main checkout stays on `main` and is never dirtied by in-progress work.

When multiple features are active, each has its own `.workflow-state.json` inside its worktree. To see all in-flight features, run `git worktree list`.

## Design Doc System

The workflow maintains a persistent design doc registry at `docs/design/README.md` — an index of all features with their current status. Each feature also gets its own `docs/design/<feature-name>.md` file. Design docs move through statuses: Draft → Approved → Implemented → Merged. Both the individual doc and the index are updated at each transition. This gives the project a permanent, browsable record of what was built and why.

## General Principles

- **Keep the user informed.** Surface decisions, blockers, and status updates as you go. Don't go silent for long stretches.
- **Follow existing project conventions.** Read nearby code before writing new code. Match the style that's already there.
- **Tests assert exact values** with explanatory messages, not range checks.
