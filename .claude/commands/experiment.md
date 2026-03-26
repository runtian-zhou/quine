You are a QA engineer designing and running experiments against the quine agent harness.

## Goal

Design your own comprehensive test suite that exercises all implemented tools, run it via the one-shot API, and record any failures.

## Step 1: Build and Start Daemon

1. `cargo build`
2. `rm -f /tmp/quine-experiment.sock`
3. Start daemon in background:
   ```
   source .env && cargo run --bin quine -- daemon start --socket /tmp/quine-experiment.sock
   ```
   Wait 3 seconds. Verify socket exists.
4. `mkdir -p qa/reports`
5. Generate UTC timestamp: `date -u +%Y%m%dT%H%M%SZ`

## Step 2: Discover Tools

Read `crates/quine-core/src/tool/mod.rs` and scan `crates/quine-core/src/tool/` to find every tool registered in the harness. Build a list of tool names and what each does.

## Step 3: Design Experiments

For each tool (and for interesting multi-tool combinations), design **your own** test cases. Be creative — the goal is to find bugs, not confirm happy paths.

Each test case is a one-shot message sent via:
```
cargo run --bin quine -- run --socket /tmp/quine-experiment.sock [--json] [--session $SID] "<message>"
```

**Design principles:**
- Every tool must appear in at least one test
- At least 3 cases should require **multiple tools in a single turn** (e.g., bash + write_file + read_file)
- At least 1 case should test **multi-turn session persistence** (use `--json` + `--session`)
- Include edge cases: empty input, large output, error paths, non-existent files
- Each case needs a clear **expected output** you can check (a magic string, a known value, etc.)
- Timeout each command at 120 seconds

**Example** (for inspiration only — design your own):
```
Message: "Use bash to run 'echo HELLO_42' and return the output"
Expect: stdout contains "HELLO_42"
```

## Step 4: Run Experiments

Execute each test case. For each:
1. Run the command with Bash tool (120s timeout)
2. Check stdout/stderr against your expected criteria
3. Record: case name, tools tested, pass/fail, stdout snippet, error if any

Use `--json` and save session_id when needed for multi-turn tests.

## Step 5: Record Results

Write a report to `qa/reports/experiment-<TIMESTAMP>.md`:

```markdown
# Experiment Report — <TIMESTAMP>

## Summary
- **Total**: N
- **Passed**: X
- **Failed**: Y

## Tool Coverage
(Table showing which tools were tested and whether they passed)

## Failures

(For EACH failed case, include:)

### <case_name> — FAIL
- **Tools**: ...
- **Message**: the exact message sent
- **Expected**: what you looked for
- **Stdout** (first 500 chars): ...
- **Stderr** (first 200 chars): ...
- **Root cause**: your analysis (LLM error, tool bug, timeout, etc.)

## Environment
- Commit: ...
- Branch: ...
```

If all cases pass, still write a brief success report.

## Step 6: Commit Report

1. Branch: `experiment-<TIMESTAMP>`
2. `git add qa/reports/experiment-<TIMESTAMP>.md`
3. Commit, push, PR, merge. Back to main.

## Step 7: Cleanup

```
cargo run --bin quine -- daemon stop --socket /tmp/quine-experiment.sock
rm -f /tmp/quine-experiment.sock
```

## Rules

- ALWAYS use `--socket /tmp/quine-experiment.sock` for every command
- ALWAYS clean up daemon when done, even on failure
- Do NOT modify project code — observation only
- Continue on failure — run ALL cases
- Be creative with test design — the point is to find real bugs
