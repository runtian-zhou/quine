You are creating a feature request for the quine project. Follow this workflow exactly:

## Step 1: Understand the Request

Read the user's feature description from the argument: $ARGUMENTS

If the description is vague, ask clarifying questions before proceeding.

## Step 2: Research the Codebase

Before writing the feature request, explore the codebase to understand:
- Which crates and files are affected
- Existing patterns and types that should be reused
- Current architecture constraints from CLAUDE.md

Use Explore agents to search for relevant code. Read CLAUDE.md for conventions.

## Step 3: Write the Feature Request

Create a feature request markdown file at `features/<kebab-case-name>.md` with this format:

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

## QA Test Cases (add to `qa/test_cases.json`)
Concrete test cases in JSON format that exercise the feature.

## Non-Goals (Deferred)
What is explicitly out of scope.
```

**Rules for the feature doc:**
- Be precise enough that an agent reading only CLAUDE.md and this file could implement it
- Reference existing types and patterns by file path
- Include concrete Rust type sketches where it clarifies the design
- Include QA test cases per the development workflow in CLAUDE.md
- Keep it focused — one logical feature per file

## Step 4: Create a PR

1. Create a feature branch: `feature-request-<name>`
2. Stage ONLY the markdown file (`features/<name>.md`). Do NOT stage any other files.
3. Commit with message: `Add feature request: <title>`
4. Verify the commit only contains the .md file: `git diff --stat HEAD~1`
5. Push and create a PR with title `Feature request: <title>` and a brief summary body.

## Step 5: Merge

1. Merge the PR via `gh pr merge <number> --merge`
2. Switch back to main and pull.

**CRITICAL: The PR must contain ONLY the feature request markdown file. No code changes, no Cargo.lock updates, no other files. If `git status` shows other modified files, stash them first.**
