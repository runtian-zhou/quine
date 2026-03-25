You are running QA tests for the quine project. Follow this workflow exactly.

## Input

The user provides a test description via: $ARGUMENTS

If no arguments are provided, run all default test cases (see Step 3).

## Step 1: Build and Start Daemon

1. Build the project: `cargo build`
2. Kill any existing daemon on the QA socket: `rm -f /tmp/quine-qa.sock`
3. Start a daemon on a dedicated QA socket in the background. Source `.env` first for LLM config:
   ```
   source .env && cargo run --bin quine -- daemon start --socket /tmp/quine-qa.sock
   ```
   Run this in the background. Wait 3 seconds for it to start.
4. Verify the daemon is running by checking the socket exists: `ls -la /tmp/quine-qa.sock`
5. Create the report directory: `mkdir -p qa/reports`
6. Generate a timestamp for this run: `YYYY-MM-DDTHH:MM:SSZ` format (UTC)

## Step 2: Parse Test Cases

Parse the user's argument to determine what to test. The argument can be:

- **A natural language test description**, e.g.: "send a message asking to read CLAUDE.md, expect the full file contents returned"
- **"all"** or empty — run the default test suite (Step 3)
- **A specific test name** from the defaults, e.g.: "bash_tool"
- **"scenarios"** — run all TOML scenario files from `qa/scenarios/` using `cargo run --bin quine -- test qa/scenarios/ --socket /tmp/quine-qa.sock`
- **"scenario:NAME"** — run a specific scenario, e.g.: `cargo run --bin quine -- test qa/scenarios/NAME.toml --socket /tmp/quine-qa.sock`

For natural language descriptions, extract:
- The message to send to the agent
- The expected output criteria (contains string, JSON fields, file contents match, etc.)

## Step 3: Load Test Suite

Read the test case definitions from `.claude/qa-tests.md` in the project root.

If the user specified **"all"** or no argument, run every test case in that file in order.
If the user specified a **test name** (e.g., "bash_tool"), run only that test.

For each test case, construct the appropriate `cargo run --bin quine -- run` command:
- Always include `--socket /tmp/quine-qa.sock`
- Add `--json` if the test specifies the `--json` flag
- Add `--session <id>` for multi-turn tests after extracting the session ID from the previous turn
- For tests with a **Pre-check** step (e.g., read_file_tool), read the expected file yourself first to know the expected value

## Step 4: Execute Each Test

For each test case:

1. Print the test name and what you're testing
2. Run the command using Bash tool, with a **120 second timeout**
3. Capture stdout (the agent response) and stderr
4. Evaluate whether the output meets the expected criteria
5. Record PASS or FAIL with details

**Important:**
- Use `--socket /tmp/quine-qa.sock` for all commands
- For commands that may take a while (tool use), use the 120s timeout
- If a test fails, continue running the remaining tests — do not stop early
- For custom tests from user arguments, construct the appropriate `cargo run --bin quine -- run` command

## Step 5: Report Results

After all tests complete:

### 5a. Print summary to the user

Print a summary table in the conversation:

```
=== QA Results ===
Test                  | Status | Details
----------------------|--------|--------
simple_greeting       | PASS   | Output contained "PONG"
bash_tool             | FAIL   | Expected "TOOLTEST_42" not found in output
...

Total: X | Passed: Y | Failed: Z
```

### 5b. Write report file to `qa/reports/`

Write a markdown report to `qa/reports/qa-<TIMESTAMP>.md` with the following structure:

```markdown
# QA Report — <TIMESTAMP>

## Summary
- **Total**: X
- **Passed**: Y
- **Failed**: Z
- **Pass rate**: Y/X (percentage)

## Environment
- **LLM Provider**: (Anthropic / OpenAI-compat — read from daemon stderr or .env)
- **Model**: (model name from .env)
- **Commit**: (output of `git rev-parse --short HEAD`)
- **Branch**: (output of `git branch --show-current`)

## Results

| Test | Status |
|------|--------|
| simple_greeting | PASS |
| bash_tool | FAIL |
| ... | ... |

## Failures

(Only include sections for FAILED tests. Omit passing tests.)

### bash_tool — FAIL
- **Message sent**: "Use the bash tool to run: echo TOOLTEST_42..."
- **Expected**: Output contains "TOOLTEST_42"
- **Actual output**: (full stdout, truncated to 500 chars if longer)
- **Stderr**: (relevant error output if any)

## Analysis

(Write 2-5 sentences analyzing the results. For failures, identify:
- Root cause category: LLM error, tool error, conversation history bug, timeout, etc.
- Whether the failure is a regression or a known issue
- Suggested next steps to fix)
```

### 5c. Commit and merge the report

1. Create a branch: `qa-report-<TIMESTAMP>`
2. Stage only the report file: `git add qa/reports/qa-<TIMESTAMP>.md`
3. Commit: `QA report: <TIMESTAMP> — X/Y passed`
4. Push and create a PR with title `QA report: <TIMESTAMP>` and the summary table as the body
5. Merge the PR via `gh pr merge <number> --merge`
6. Switch back to main and pull

## Step 6: Cleanup

Stop the daemon and clean up:
```
cargo run --bin quine -- daemon stop --socket /tmp/quine-qa.sock
rm -f /tmp/quine-qa.sock
```

Switch back to main branch if not already on it.

## Rules

- ALWAYS start a fresh daemon for QA — never reuse an existing one
- ALWAYS clean up the daemon when done, even if tests fail
- Use `--json` mode when you need to extract session IDs or structured data
- For file content comparisons, read the expected file yourself first, then compare
- Treat the agent as a black box — only observe its stdout/stderr output
- If the daemon fails to start (e.g., missing LLM env vars), report the error clearly and stop
- Do NOT modify any project code during QA — this is read-only testing
