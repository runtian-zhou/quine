# QA Test Cases

Default test suite consumed by the `/qa` skill. For scripted multi-session scenarios,
use `quine test qa/scenarios/` or `/qa scenarios`.

Each test case specifies:
- **name**: unique identifier
- **description**: what the test verifies
- **turns**: ordered list of messages to send and expectations to check
- **flags**: optional flags like `--json`

Registered tools: `bash`, `read_file`, `write_file`, `ask_user`, `plan`, `subagent`, `spawn`, `wait_child`, `signal`, `send_message`, `recv_message`

Delegation policy: prefer `subagent` for normal task delegation where the caller wants the child result returned in the same turn. Use `spawn`, `wait_child`, `signal`, `send_message`, and `recv_message` only for advanced coordination scenarios such as concurrency, long-lived children, cancellation, or mailbox-based communication.

---

## simple_greeting
**Description**: Basic single-turn greeting to verify the agent responds.
- **Send**: `"Hello, please respond with exactly: PONG"`
- **Expect**: Output contains `PONG`

---

## bash_tool
**Description**: Verify the bash tool executes commands and returns output.
- **Send**: `"Use the bash tool to run: echo TOOLTEST_42. Include the exact output in your response."`
- **Expect**: Output contains `TOOLTEST_42`

## bash_timeout
**Description**: Verify bash tool respects the timeout parameter.
- **Send**: `"Use the bash tool with a timeout of 5 seconds to run: echo TIMEOUT_OK. Include the output in your response."`
- **Expect**: Output contains `TIMEOUT_OK`

## bash_stderr
**Description**: Verify bash tool captures stderr output.
- **Send**: `"Use the bash tool to run: echo STDERR_TEST >&2. Tell me what the stderr output was."`
- **Expect**: Output contains `STDERR_TEST`

## bash_exit_code
**Description**: Verify bash tool reports non-zero exit codes.
- **Send**: `"Use the bash tool to run: exit 42. Report the exit code you received."`
- **Expect**: Output contains `42`

---

## read_file_tool
**Description**: Verify the read_file tool reads files and returns content.
- **Send**: `"Use the read_file tool to read the file at Cargo.toml in the current directory. Return the first line of the file exactly."`
- **Pre-check**: Read `Cargo.toml` yourself to know the expected first line.
- **Expect**: Output contains the actual first line of `Cargo.toml`

## read_file_offset_limit
**Description**: Verify read_file with offset and limit parameters.
- **Send**: `"Use the read_file tool to read Cargo.toml starting from line 2 with a limit of 1 line. Return the exact content of that line."`
- **Pre-check**: Read line 2 of `Cargo.toml` yourself to know the expected value.
- **Expect**: Output contains the actual second line of `Cargo.toml`

## read_file_nonexistent
**Description**: Verify read_file handles missing files gracefully.
- **Send**: `"Use the read_file tool to read /tmp/quine-qa-does-not-exist-xyz.txt. Tell me exactly what error you got."`
- **Expect**: Output contains `not found` or `No such file` or `does not exist` (case-insensitive)

---

## write_file_tool
**Description**: Verify write_file creates files and read_file reads them back.
- **Send**: `"Use the write_file tool to write the text QUINE_WRITE_TEST_123 to /tmp/quine-qa-write-test.txt. Then use the read_file tool to read it back. Include the file contents in your response."`
- **Expect**: Output contains `QUINE_WRITE_TEST_123`

## write_file_creates_dirs
**Description**: Verify write_file creates parent directories automatically.
- **Send**: `"Use the write_file tool to write NESTED_DIR_TEST to /tmp/quine-qa-nested/subdir/test.txt. Then use the read_file tool to read it back. Include the contents."`
- **Expect**: Output contains `NESTED_DIR_TEST`

---

## plan_create
**Description**: Verify the plan tool can create an action plan.
- **Send**: `"Use the plan tool to create a plan titled 'Test Plan' with two actions: action id 'a1' titled 'First step' described as 'Do the first thing', and action id 'a2' titled 'Second step' described as 'Do the second thing' that depends on 'a1'. Report the plan_id you received."`
- **Expect**: Output contains `plan` (confirms plan was created and discussed)

## plan_create_and_update
**Description**: Verify creating a plan and updating action status.
- **Flags**: `--json`
- **Send**: `"Use the plan tool to create a plan titled 'QA Plan' with one action: id 'step1', title 'Test step', description 'A test action'. Then update that plan's action 'step1' status to 'completed' with result 'Done'. Tell me the final status."`
- **Expect**: Output contains `completed`

---

## subagent_preferred_for_delegation
**Description**: Verify the agent prefers `subagent` over `spawn` + `wait_child` for ordinary one-shot delegation.
- **Flags**: `--json`
- **Send**: `"Delegate this small task to another agent: use bash to run echo PREFER_SUBAGENT_17 and return the delegated result."`
- **Expect**: JSON `tool_calls` includes `subagent`; JSON `tool_calls` does not include `spawn` or `wait_child`; `response` contains `PREFER_SUBAGENT_17`

---

## subagent_tool
**Description**: Verify the subagent tool can spawn a child agent and return results.
- **Send**: `"Use the subagent tool to spawn a child agent with the task: 'Use the bash tool to run echo SUBAGENT_RESULT and return the output.' Include the child's result in your response."`
- **Expect**: Output contains `SUBAGENT_RESULT`

---

## json_output
**Description**: Verify --json flag produces valid structured output.
- **Flags**: `--json`
- **Send**: `"Say hello"`
- **Expect**: Output is valid JSON with fields: `session_id`, `response`, `tool_calls`

## json_with_tool_calls
**Description**: Verify --json output includes tool call records.
- **Flags**: `--json`
- **Send**: `"Use the bash tool to run: echo JSON_TOOL_CHECK"`
- **Expect**: Output is valid JSON; `tool_calls` array is non-empty; `response` contains `JSON_TOOL_CHECK`

---

## session_persistence
**Description**: Two-turn conversation verifying session persistence across messages.
- **Turn 1**:
  - **Flags**: `--json`
  - **Send**: `"Remember this number: 7742. Reply with OK."`
  - **Expect**: Output contains `OK`
  - **Extract**: `session_id` from JSON output
- **Turn 2**:
  - **Flags**: `--session <session_id from turn 1>`
  - **Send**: `"What number did I ask you to remember?"`
  - **Expect**: Output contains `7742`

## session_tool_state
**Description**: Verify tool state persists across session turns (file created in turn 1 readable in turn 2).
- **Turn 1**:
  - **Flags**: `--json`
  - **Send**: `"Use the write_file tool to write SESSION_STATE_OK to /tmp/quine-qa-session-state.txt. Reply with DONE."`
  - **Expect**: Output contains `DONE`
  - **Extract**: `session_id` from JSON output
- **Turn 2**:
  - **Flags**: `--session <session_id from turn 1>`
  - **Send**: `"Use the read_file tool to read /tmp/quine-qa-session-state.txt. Return the exact contents."`
  - **Expect**: Output contains `SESSION_STATE_OK`

---

## multi_tool_chain
**Description**: Verify the agent can chain multiple tool calls in sequence.
- **Send**: `"First, use the bash tool to run 'echo hello_from_bash'. Then use the read_file tool to read Cargo.toml. Tell me both: the bash output and the first line of Cargo.toml."`
- **Expect**: Output contains `hello_from_bash`

## bash_then_write_then_read
**Description**: Three-tool chain: bash creates data, write_file saves it, read_file verifies.
- **Send**: `"Use the bash tool to run 'echo CHAIN_TEST_VALUE'. Then use write_file to save that exact output to /tmp/quine-qa-chain.txt. Then use read_file to read it back. Include the final file contents in your response."`
- **Expect**: Output contains `CHAIN_TEST_VALUE`

---

## tui_tool_status_display
**Description**: Verify tool calls show status indicators in TUI output.
- **Flags**: `--json`
- **Send**: `"Use the bash tool to run: echo TOOL_STATUS_TEST"`
- **Expect**: JSON `tool_calls` array is non-empty; response contains `TOOL_STATUS_TEST`
- **Note**: Visual verification of colors requires manual TUI testing.

## tui_turn_timing
**Description**: Verify turn timing is reported.
- **Flags**: `--json`
- **Send**: `"Say hello"`
- **Expect**: JSON output contains `duration_ms` field with value > 0

---

## ask_user_interaction
**Description**: Verify ask_user pauses execution and resumes with user response.
- **Scenario**: `qa/scenarios/ask_user.toml`
- **Expect**: Scenario passes (agent uses the provided response)

## spawn_and_wait_child
**Description**: Verify spawn creates a child and wait_child collects the result.
- **Scenario**: `qa/scenarios/spawn_wait.toml`
- **Expect**: Scenario passes (child result is collected by parent)

## signal_child
**Description**: Verify signal terminates a running child session.
- **Scenario**: `qa/scenarios/spawn_signal.toml`
- **Expect**: Scenario passes (child is terminated)

## ipc_send_recv
**Description**: Verify send_message and recv_message between sessions.
- **Scenario**: `qa/scenarios/ipc_messaging.toml`
- **Expect**: Scenario passes (message content matches)
