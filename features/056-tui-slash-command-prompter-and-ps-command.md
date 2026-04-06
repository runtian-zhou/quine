---
status: done
---

# TUI Slash Command Prompter and `ps` Command

Add slash-command prompting in the TUI composer and support the `ps` session-listing command from the CLI and TUI chat flow.

## Requirements

- Show slash-command suggestions in the TUI composer when the input begins with `/`.
- Allow keyboard navigation and acceptance of the selected slash command.
- Support executing `/ps` from chat/TUI using the existing session-listing output paths.
- Keep feature tracking aligned with the existing implementation and QA planning documents under `features/plans/`.

## Acceptance Criteria

- Entering `/` in the TUI composer opens slash-command suggestions.
- Selecting a suggestion inserts the slash command into the composer.
- Running `/ps` from chat displays the current session list without sending a normal user message.
- The feature request file exists and is marked `done` because the implementation is already present.
