---
status: done
---

# Enhanced AskUser Tool with Multi-Select Options and TUI Support

## Overview

Extend the `AskUserTool` and `InteractionRequest` to support structured question types: free-form text input, single-select from a list of options, and multi-select from a list of options. Update the TUI to render these interaction types with arrow-key navigation for option selection.

## Requirements

### 1. Extended InteractionRequest (`quine-core/src/tool/mod.rs`)

Extend `InteractionKind` and `InteractionRequest`:

```rust
pub enum InteractionKind {
    /// Free-form text question.
    Question,
    /// Yes/no confirmation.
    Confirmation,
    /// Select exactly one option from a list.
    SingleSelect,
    /// Select one or more options from a list.
    MultiSelect,
}

pub struct InteractionRequest {
    pub prompt: String,
    pub kind: InteractionKind,
    /// Available options for SingleSelect/MultiSelect.
    /// Ignored for Question/Confirmation.
    pub options: Vec<SelectOption>,
    /// Default value or pre-selected option index.
    pub default: Option<String>,
    /// Whether to allow free-form input in addition to options ("Other" option).
    pub allow_freeform: bool,
}

pub struct SelectOption {
    /// Display label for the option.
    pub label: String,
    /// Optional description shown below the label.
    pub description: Option<String>,
}

pub struct InteractionResponse {
    pub response: String,
    /// For MultiSelect: indices of selected options (0-based).
    pub selected_indices: Vec<usize>,
    /// Whether the user cancelled the interaction.
    pub cancelled: bool,
}
```

### 2. Enhanced AskUserTool (`quine-core/src/tool/ask_user.rs`)

Update the tool to accept richer parameters:

- **Name**: `ask_user`
- **Parameters**:
  ```json
  {
    "question": "string (required)",
    "options": ["array of strings (optional)"],
    "multi_select": "boolean (default false)",
    "allow_freeform": "boolean (default true)",
    "default": "string (optional)"
  }
  ```
- When `options` is provided and `multi_select` is false → `SingleSelect`
- When `options` is provided and `multi_select` is true → `MultiSelect`
- When `options` is empty/absent → `Question` (free-form text, same as today)
- Response format:
  - SingleSelect: returns the selected option label as a string
  - MultiSelect: returns selected option labels joined by `", "`
  - Question: returns the user's text input

### 3. TUI Changes (`quine-cli/src/tui/`)

#### App State (`app.rs`)

Add selection state for option-based interactions:

```rust
struct OptionSelectState {
    /// Available options.
    options: Vec<SelectOption>,
    /// Currently highlighted option index.
    cursor: usize,
    /// Selected indices (for MultiSelect).
    selected: HashSet<usize>,
    /// Whether multi-select is enabled.
    multi_select: bool,
    /// Whether freeform "Other" is available.
    allow_freeform: bool,
}
```

Add `option_select: Option<OptionSelectState>` to `App`.

When a `SingleSelect` or `MultiSelect` interaction arrives:
- Populate `OptionSelectState` from the `InteractionRequest`
- Switch the input mode to option selection

#### UI Rendering (`ui.rs`)

Render the option selector in the interaction area:

**SingleSelect display:**
```
❓ Which approach should we use?

  › Option A - Description of option A
    Option B - Description of option B
    Option C - Description of option C

  [Enter] to select | [↑↓] to navigate
```

**MultiSelect display:**
```
❓ Which features do you want?

  [x] Feature A - Description
  › [ ] Feature B - Description
  [x] Feature C - Description

  [Space] to toggle | [Enter] to confirm | [↑↓] to navigate
```

**Freeform fallback:**
If `allow_freeform` is true, show an "Other..." option at the bottom that switches to text input when selected.

#### Key Bindings

- **Up/Down** (or **k/j**): navigate options
- **Enter**: confirm selection (SingleSelect: select highlighted; MultiSelect: submit all selected)
- **Space** (MultiSelect only): toggle selection on highlighted option
- **Esc**: cancel interaction
- **o** or selecting "Other...": switch to freeform text input (if `allow_freeform`)

### 4. Plain Chat Mode (`chat.rs`)

For non-TUI mode, render options as numbered list:

```
❓ Which approach should we use?
  1. Option A - Description
  2. Option B - Description
  3. Option C - Description
Enter number (or text for custom answer):
```

Parse the response: if it's a number, map to the option. Otherwise, treat as freeform.

### 5. Protocol Updates

- `InteractionRequest` is already serialized via serde in the JSON-RPC protocol
- The new fields (`options`, `default`, `allow_freeform`) are added to the serialized form
- `InteractionResponse` gains `selected_indices` and `cancelled` fields
- Backward compatible: old requests with no `options` field work as before (Question kind)

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Unit tests for `AskUserTool`: question mode (no options), single-select, multi-select, freeform fallback, cancellation.
- Unit tests for `InteractionRequest` serialization with options.
- TUI renders single-select with arrow navigation and Enter to confirm.
- TUI renders multi-select with Space to toggle and Enter to submit.
- TUI shows "Other..." option when `allow_freeform` is true.
- Plain chat mode renders numbered list and accepts number or text input.
- Existing interaction tests continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
{
  "name": "ask_user_with_options",
  "description": "Verify ask_user tool can present options to the user",
  "turns": [
    {
      "message": "Use the ask_user tool to ask me 'Pick a color' with options: red, green, blue. I will answer.",
      "expect_contains": "Pick a color"
    }
  ]
}
```

```json
{
  "name": "ask_user_freeform",
  "description": "Verify ask_user tool still works for free-form questions",
  "turns": [
    {
      "message": "Use the ask_user tool to ask me 'What is your name?'. I will answer.",
      "expect_contains": "What is your name"
    }
  ]
}
```

## Non-Goals (Deferred)

- Hierarchical/nested option groups.
- Option filtering/search.
- Option icons or rich formatting.
- Validated text input (regex patterns, numeric ranges).
- Multi-field forms (multiple questions in one interaction).
