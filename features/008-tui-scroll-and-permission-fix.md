---
status: done
---

# TUI: Input History Scrolling and Permission Prompt Fix

## Input History Scrolling

Up/Down arrow keys in the input box should cycle through previous user inputs (like a shell), not scroll the conversation view. Conversation scrolling moves to Page Up/Page Down.

- Up arrow: recall previous input (older)
- Down arrow: recall next input (newer), empty string at the end
- Maintain a history buffer of all submitted inputs
- Current in-progress input is preserved when entering history mode

## Permission Prompt Fix

Permission confirmation requests (`interaction_needed` with kind `Confirmation`) are currently rendered identically to `ask_user` questions in the input box label. They should be rendered distinctly:

- Show the permission prompt as a highlighted block in the conversation view (e.g. yellow background or bold border)
- Input box label: `[permission] Allow bash: rm -rf /tmp/foo? (y/n) > ` — clearly indicate it's a permission check, not a free-form question
- Accept `y`/`yes`/`n`/`no` as shorthand responses

## Acceptance Criteria

- Up/Down in input box cycles through input history
- Page Up/Page Down scrolls the conversation view
- Permission prompts are visually distinct from ask_user prompts
- `y`/`n` shorthand works for permission confirmations
- All CI checks pass
