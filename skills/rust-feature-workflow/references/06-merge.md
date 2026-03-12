# Phase 6: Merge

Once the user approves the implementation, create a PR via `gh`, merge it, and clean up.

All commands in this phase run from inside the worktree directory unless noted otherwise.

## Steps

1. **Rebase onto latest main:**
   ```bash
   git fetch origin
   git rebase origin/main
   ```
   Resolve any conflicts if they arise.

2. **Re-run the review loop** after rebase (format, clippy, test) to make sure nothing broke.

3. **Update the design doc to Merged status:**
   - Set `## Status` to `Merged` in `docs/design/<feature-name>.md`.
   - Update the corresponding row in `docs/design/README.md` to `Merged`.
   - Commit this as the final commit on the feature branch.

4. **Push and create a PR:**
   ```bash
   git push -u origin feat/<feature-name>
   gh pr create --title "<short title>" --body "$(cat <<'EOF'
   ## Summary
   <1-3 bullet points from the design doc summary>

   ## Design Doc
   See `docs/design/<feature-name>.md`

   ## Test Plan
   <key test scenarios from the design doc testing plan>
   EOF
   )"
   ```
   Report the PR URL to the user.

5. **Wait for CI** (if the project has a CI pipeline):
   ```bash
   gh pr checks feat/<feature-name> --watch
   ```
   If checks fail, diagnose and fix, then push again.

6. **Merge the PR:**
   ```bash
   gh pr merge feat/<feature-name> --squash --delete-branch
   ```
   Use `--squash` to keep main's history clean. The `--delete-branch` flag removes the remote branch automatically.

7. **Clean up locally:**
   ```bash
   cd <main-repo-root>
   git fetch origin
   git pull origin main
   git worktree remove ../worktrees/<feature-name>
   git branch -D feat/<feature-name>
   ```

8. **Delete `.workflow-state.json`** — the workflow is complete.

## Post-Merge Checklist

- Confirm the design doc status is `Merged` in both the doc and the index.
- Confirm the worktree has been removed (`git worktree list` should not show it).
- Confirm the PR is merged: `gh pr view feat/<feature-name> --json state`
- If the project has a `CHANGELOG.md`, add an entry for the new feature.
- If the project has a `README.md` that documents features, update it if appropriate.

## Output

PR merged on GitHub, design doc marked as Merged, worktree removed, local branch deleted, user notified with PR URL.
