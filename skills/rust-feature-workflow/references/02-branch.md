# Phase 2: Create a Feature Branch and Worktree

Each feature gets its own git worktree so multiple features can be developed in parallel without interfering with each other. Worktrees let different branches be checked out simultaneously in separate directories.

## Steps

1. **Create the feature branch** from the latest main:
   ```bash
   git fetch origin
   git branch feat/<feature-name> origin/main
   ```

2. **Create a worktree** for the branch:
   ```bash
   git worktree add ../worktrees/<feature-name> feat/<feature-name>
   ```
   This creates a separate working directory at `../worktrees/<feature-name>` with the feature branch checked out. All subsequent work for this feature (implementation, review, etc.) happens in this directory.

3. **Record the worktree path** in `.workflow-state.json`:
   ```json
   {
     "worktree_path": "../worktrees/<feature-name>"
   }
   ```

## Conventions

- Use lowercase kebab-case for the feature name (e.g., `feat/json-output-flag`).
- Worktrees live in `../worktrees/` relative to the main repo root. This keeps them out of the repo itself while staying nearby.
- All phases after this one (Design, Implement, Review, Merge) operate inside the worktree directory, not the main checkout.

## Why Worktrees

- Multiple features can be in-flight at the same time — each in its own worktree on its own branch.
- The main checkout stays on `main` and is never in a dirty state from in-progress work.
- Each worktree has its own `.workflow-state.json` for independent progress tracking.

## Output

- A feature branch `feat/<feature-name>` based on latest `origin/main`.
- A worktree at `../worktrees/<feature-name>` with the branch checked out.
- `.workflow-state.json` updated with `worktree_path`.
