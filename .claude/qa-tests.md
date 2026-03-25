# QA Test Cases

Default test suite consumed by the `/qa` skill. Each test case specifies:
- **name**: unique identifier
- **description**: what the test verifies
- **turns**: ordered list of messages to send and expectations to check
- **flags**: optional flags like `--json`

---

## simple_greeting
**Description**: Basic single-turn greeting to verify the agent responds.
- **Send**: `"Hello, please respond with exactly: PONG"`
- **Expect**: Output contains `PONG`

## bash_tool
**Description**: Verify the bash tool executes commands and returns output.
- **Send**: `"Use the bash tool to run: echo TOOLTEST_42. Include the exact output in your response."`
- **Expect**: Output contains `TOOLTEST_42`

## read_file_tool
**Description**: Verify the read_file tool reads files and returns content.
- **Send**: `"Use the read_file tool to read the file at Cargo.toml in the current directory. Return the first line of the file exactly."`
- **Pre-check**: Read `Cargo.toml` yourself to know the expected first line.
- **Expect**: Output contains the actual first line of `Cargo.toml`

## json_output
**Description**: Verify `--json` flag produces valid structured output.
- **Flags**: `--json`
- **Send**: `"Say hello"`
- **Expect**: Output is valid JSON with fields: `session_id`, `response`, `tool_calls`

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

## write_file_tool
**Description**: Verify the write_file tool creates files correctly.
- **Send**: `"Use the bash tool to create a file at /tmp/quine-qa-test.txt with the content QUINE_WRITE_TEST. Then use the read_file tool to read it back and include the contents in your response."`
- **Expect**: Output contains `QUINE_WRITE_TEST`

## multi_tool_chain
**Description**: Verify the agent can chain multiple tool calls in sequence.
- **Send**: `"First, use the bash tool to run 'echo hello_from_bash'. Then use the read_file tool to read Cargo.toml. Tell me both: the bash output and the first line of Cargo.toml."`
- **Expect**: Output contains `hello_from_bash`
