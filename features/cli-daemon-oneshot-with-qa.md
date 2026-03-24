---
status: done
---

# CLI Daemon One-Shot Mode with QA

## One-shot CLI mode (`quine run`)

Add a `run` subcommand to quine-cli:
- `quine run "message"` — connect to daemon, send one message, collect full response, print to stdout, exit
- `quine run --session <id> "message"` — resume an existing session
- `quine run --json "message"` — output structured JSON with session_id, response, tool_calls
- New session: print session ID to stderr
- Session persists after CLI disconnects

## Session persistence in daemon

Sessions survive after CLI disconnects (already basically true since LocalHarness holds sessions in memory). Ensure session stays alive after one-shot client disconnects.

## Daemon log dump

- Record session events in structured JSONL: `~/.quine/logs/<session_id>.jsonl`
- `quine log <session_id>` — dump session log
- `quine log --list` — list sessions with timestamps
- Log entries: timestamp, event type, session ID, direction, payload

## QA agent (`qa/`)

- `qa/test_cases.json` — multi-turn test conversations
- `qa/run_qa.sh` — spawns daemon, runs tests, collects reports
- `qa/reports/.gitkeep` — gitignored output dir
- Add `qa/reports/` to `.gitignore`
