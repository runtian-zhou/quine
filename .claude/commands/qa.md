You are running QA tests for the quine project. Follow this workflow exactly.

## Input

The user provides a test description via: $ARGUMENTS

If no arguments are provided, run all default test cases (see Step 3).

## Step 1: Build and Start Daemon

1. Build the project: `cargo build`
2. Kill any existing daemon on the QA socket: `rm -f /tmp/quine-qa.sock`
3. Start a daemon on a dedicated QA socket in the background:
   ```
   cargo run --bin quine -- daemon start --socket /tmp/quine-qa.sock
   ```
   Run this in the background. Wait 3 seconds for it to start.
4. Verify the daemon is running by checking the socket exists: `ls -la /tmp/quine-qa.sock`

## Step 2: Parse Test Cases

Parse the user's argument to determine what to test. The argument can be:

- **A natural language test description**, e.g.: "send a message asking to read CLAUDE.md, expect the full file contents returned"
- **"all"** or empty — run the default test suite (Step 3)
- **A specific test name** from the defaults, e.g.: "bash_tool"

For natural language descriptions, extract:
- The message to send to the agent
- The expected output criteria (contains string, JSON fields, file contents match, etc.)

## Step 3: Default Test Suite

If running "all" or no argument, execute these test cases in order:

### Test 1: simple_greeting
- **Send**: `cargo run --bin quine -- run --socket /tmp/quine-qa.sock "Hello, please respond with exactly: PONG"`
- **Expect**: Output contains "PONG"

### Test 2: bash_tool
- **Send**: `cargo run --bin quine -- run --socket /tmp/quine-qa.sock "Use the bash tool to run: echo TOOLTEST_42. Include the exact output in your response."`
- **Expect**: Output contains "TOOLTEST_42"

### Test 3: read_file_tool
- **Send**: `cargo run --bin quine -- run --socket /tmp/quine-qa.sock "Use the read_file tool to read the file at Cargo.toml in the current directory. Return the first line of the file exactly."`
- **Read** `Cargo.toml` yourself to know the expected first line.
- **Expect**: Output contains the actual first line of Cargo.toml

### Test 4: json_output
- **Send**: `cargo run --bin quine -- run --json --socket /tmp/quine-qa.sock "Say hello"`
- **Expect**: Output is valid JSON with fields: `session_id`, `response`, `tool_calls`

### Test 5: session_persistence
- **Send first message**: `cargo run --bin quine -- run --json --socket /tmp/quine-qa.sock "Remember this number: 7742. Reply with OK."`
- **Extract** `session_id` from the JSON output
- **Send second message**: `cargo run --bin quine -- run --socket /tmp/quine-qa.sock --session <session_id> "What number did I ask you to remember?"`
- **Expect**: Output contains "7742"

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

After all tests complete, print a summary table:

```
=== QA Results ===
Test                  | Status | Details
----------------------|--------|--------
simple_greeting       | PASS   | Output contained "PONG"
bash_tool             | FAIL   | Expected "TOOLTEST_42" not found in output
...

Total: X | Passed: Y | Failed: Z
```

If any test failed, show the relevant output snippet for debugging.

## Step 6: Cleanup

Stop the daemon and clean up:
```
cargo run --bin quine -- daemon stop --socket /tmp/quine-qa.sock
rm -f /tmp/quine-qa.sock
```

## Rules

- ALWAYS start a fresh daemon for QA — never reuse an existing one
- ALWAYS clean up the daemon when done, even if tests fail
- Use `--json` mode when you need to extract session IDs or structured data
- For file content comparisons, read the expected file yourself first, then compare
- Treat the agent as a black box — only observe its stdout/stderr output
- If the daemon fails to start (e.g., missing LLM env vars), report the error clearly and stop
- Do NOT modify any project code during QA — this is read-only testing
