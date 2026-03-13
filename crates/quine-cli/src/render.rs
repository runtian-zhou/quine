/// Indent prefix for all LLM response output.
const INDENT: &str = "  ";

use crossterm::style::{Color, ResetColor, SetForegroundColor};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

use quine_core::dispatcher::{
    DispatcherUI, InputPromptState, SelectionPrompt, SpinnerHandle,
};
use quine_core::event::Event;

// ─── Spinner ─────────────────────────────────────────────────────────────────

pub struct Spinner {
    running: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    pub fn start(label: &str) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let running_clone = Arc::clone(&running);
        let label = label.to_string();
        let handle = std::thread::spawn(move || {
            let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
            let mut i = 0;
            while running_clone.load(Ordering::Relaxed) {
                print!(
                    "\r{}\x1b[90m{} {}\x1b[0m",
                    INDENT,
                    frames[i % frames.len()],
                    label
                );
                std::io::stdout().flush().ok();
                i += 1;
                std::thread::sleep(std::time::Duration::from_millis(80));
            }
            // Clear the spinner line
            print!("\r\x1b[2K");
            std::io::stdout().flush().ok();
        });
        Self {
            running,
            handle: Some(handle),
        }
    }
}

impl SpinnerHandle for Spinner {
    fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join().ok();
        }
    }
}

// ─── Renderer ────────────────────────────────────────────────────────────────

struct Renderer {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Renderer {
    fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    /// Print a streamed content delta (no newline, flush immediately)
    fn print_delta(&self, text: &str) {
        // Indent each new line in the delta
        let mut first = true;
        for part in text.split('\n') {
            if !first {
                print!("\n{}", INDENT);
            }
            print!("{}", part);
            first = false;
        }
        std::io::stdout().flush().ok();
    }

    /// Print the indent prefix at the start of a streaming response.
    fn print_stream_start(&self) {
        print!("{}", INDENT);
        std::io::stdout().flush().ok();
    }

    /// Print a complete assistant message with markdown/syntax highlighting
    fn print_assistant_message(&self, content: &str) {
        // Simple markdown rendering: detect code blocks and highlight them
        let mut in_code_block = false;
        let mut code_lang = String::new();
        let mut code_lines = Vec::new();

        for line in content.lines() {
            if line.starts_with("```") {
                if in_code_block {
                    // End of code block - render it
                    self.render_code_block(&code_lang, &code_lines);
                    code_lines.clear();
                    code_lang.clear();
                    in_code_block = false;
                } else {
                    // Start of code block
                    in_code_block = true;
                    code_lang = line.trim_start_matches('`').trim().to_string();
                }
            } else if in_code_block {
                code_lines.push(line.to_string());
            } else {
                // Regular text - print with basic formatting
                self.print_markdown_line(line);
            }
        }

        if in_code_block && !code_lines.is_empty() {
            self.render_code_block(&code_lang, &code_lines);
        }

        println!();
    }

    fn print_markdown_line(&self, line: &str) {
        if line.starts_with("# ") {
            println!("{}\x1b[1;36m{}\x1b[0m", INDENT, line);
        } else if line.starts_with("## ") {
            println!("{}\x1b[1;33m{}\x1b[0m", INDENT, line);
        } else if line.starts_with("### ") {
            println!("{}\x1b[1;32m{}\x1b[0m", INDENT, line);
        } else if line.starts_with("- ") || line.starts_with("* ") {
            println!(
                "{}  {}•{} {}",
                INDENT,
                SetForegroundColor(Color::Cyan),
                ResetColor,
                &line[2..]
            );
        } else if let Some(quoted) = line.strip_prefix("> ") {
            println!("{}\x1b[3;37m  │ {}\x1b[0m", INDENT, quoted);
        } else {
            println!("{}{}", INDENT, line);
        }
    }

    fn render_code_block(&self, lang: &str, lines: &[String]) {
        let code = lines.join("\n");

        let syntax = self
            .syntax_set
            .find_syntax_by_token(lang)
            .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text());

        let theme = &self.theme_set.themes["base16-ocean.dark"];
        let mut highlighter = HighlightLines::new(syntax, theme);

        println!(
            "{}\x1b[90m┌─ {} ─┐\x1b[0m",
            INDENT,
            if lang.is_empty() { "code" } else { lang }
        );
        for line in code.lines() {
            if let Ok(ranges) = highlighter.highlight_line(line, &self.syntax_set) {
                let escaped = as_24_bit_terminal_escaped(&ranges, false);
                println!("{}\x1b[90m│\x1b[0m {}\x1b[0m", INDENT, escaped);
            } else {
                println!("{}\x1b[90m│\x1b[0m {}", INDENT, line);
            }
        }
        println!("{}\x1b[90m└──────┘\x1b[0m", INDENT);
    }

    fn print_tool_call(&self, name: &str, args: &serde_json::Value) {
        println!("\n{}\x1b[1;33m⚙ Tool: {}\x1b[0m", INDENT, name);
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let display = match value {
                    serde_json::Value::String(s) => {
                        if s.len() > 80 {
                            let mut end = 80;
                            while end > 0 && !s.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}...", &s[..end])
                        } else {
                            s.clone()
                        }
                    }
                    other => {
                        let s = other.to_string();
                        if s.len() > 80 {
                            let mut end = 80;
                            while end > 0 && !s.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}...", &s[..end])
                        } else {
                            s
                        }
                    }
                };
                println!("{}  \x1b[90m{}: {}\x1b[0m", INDENT, key, display);
            }
        }
    }

    fn print_tool_result(&self, success: bool, output: &str) {
        if success {
            println!("{}\x1b[32m  ✓ Success\x1b[0m", INDENT);
        } else {
            println!("{}\x1b[31m  ✗ Failed\x1b[0m", INDENT);
        }
        // Print first few lines of output
        let lines: Vec<&str> = output.lines().take(5).collect();
        for line in &lines {
            println!("{}  \x1b[90m{}\x1b[0m", INDENT, line);
        }
        let total_lines = output.lines().count();
        if total_lines > 5 {
            println!(
                "{}  \x1b[90m... ({} more lines)\x1b[0m",
                INDENT,
                total_lines - 5
            );
        }
    }
}

// ─── CliUI: DispatcherUI implementation ──────────────────────────────────────

/// CLI implementation of the DispatcherUI trait using syntect, crossterm, etc.
pub struct CliUI {
    renderer: Renderer,
}

impl CliUI {
    pub fn new() -> Self {
        Self {
            renderer: Renderer::new(),
        }
    }
}

impl DispatcherUI for CliUI {
    fn print_assistant_message(&self, content: &str) {
        self.renderer.print_assistant_message(content);
    }

    fn print_delta(&self, text: &str) {
        self.renderer.print_delta(text);
    }

    fn print_stream_start(&self) {
        self.renderer.print_stream_start();
    }

    fn print_tool_call(&self, name: &str, args: &serde_json::Value) {
        self.renderer.print_tool_call(name, args);
    }

    fn print_tool_result(&self, success: bool, output: &str) {
        self.renderer.print_tool_result(success, output);
    }

    fn start_spinner(&self, label: &str) -> Box<dyn SpinnerHandle> {
        Box::new(Spinner::start(label))
    }
}

// ─── Input reader ────────────────────────────────────────────────────────────

const OTHER_OPTION: &str = "Other (type your own)...";
const FREEFORM_PROMPT: &str = "Type your response";

/// Spawn a background thread that reads user input and sends it as events.
/// Uses rustyline for readline and dialoguer for interactive selectors.
pub fn spawn_input_reader(
    tx: tokio::sync::mpsc::Sender<Event>,
    signal: Arc<(Mutex<InputPromptState>, Condvar)>,
) {
    tokio::task::spawn_blocking(move || {
        let mut rl = rustyline::DefaultEditor::new().unwrap();
        loop {
            // Wait until the dispatcher signals we should prompt for input.
            let (prompt, selection) = {
                let (lock, cvar) = &*signal;
                let mut state = lock.lock().unwrap();
                while !state.ready {
                    state = cvar.wait(state).unwrap();
                }
                state.ready = false;
                (state.prompt.clone(), state.selection.take())
            };

            // If we have a selection prompt, use dialoguer instead of readline.
            if let Some(sel) = selection {
                let result = run_selector(&sel);
                match result {
                    Some(text) => {
                        if tx.blocking_send(Event::UserInput(text)).is_err() {
                            break;
                        }
                    }
                    None => {
                        // User cancelled (Esc / Ctrl+C)
                        let _ = tx.blocking_send(Event::Shutdown);
                        break;
                    }
                }
                continue;
            }

            match rl.readline(&prompt) {
                Ok(line) => {
                    let trimmed = line.trim_end().to_string();
                    if !trimmed.is_empty() {
                        let _ = rl.add_history_entry(&trimmed);
                    }
                    if tx.blocking_send(Event::UserInput(trimmed)).is_err() {
                        break;
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted)
                | Err(rustyline::error::ReadlineError::Eof) => {
                    let _ = tx.blocking_send(Event::Shutdown);
                    break;
                }
                Err(_) => {
                    let _ = tx.blocking_send(Event::Shutdown);
                    break;
                }
            }
        }
    });
}

/// Run an interactive arrow-key selector. Returns the selected text,
/// or None if the user cancelled.
fn run_selector(sel: &SelectionPrompt) -> Option<String> {
    use dialoguer::{theme::ColorfulTheme, Input, MultiSelect, Select};

    let theme = ColorfulTheme::default();

    if sel.multi {
        let mut items = sel.options.clone();
        if sel.allow_text {
            items.push(OTHER_OPTION.to_string());
        }

        let chosen = MultiSelect::with_theme(&theme)
            .with_prompt(&sel.question)
            .items(&items)
            .interact_opt()
            .ok()?;

        let indices = chosen?;
        if indices.is_empty() {
            return Some("(no selection)".to_string());
        }

        let other_idx = items.len() - 1;
        let has_other = sel.allow_text && indices.contains(&other_idx);

        // Collect the real (non-"Other") selections
        let mut selected: Vec<String> = indices
            .iter()
            .filter(|&&i| !(sel.allow_text && i == other_idx))
            .map(|&i| sel.options[i].clone())
            .collect();

        // If "Other..." was checked, prompt for freeform and append it
        if has_other {
            let input = Input::<String>::with_theme(&theme)
                .with_prompt(FREEFORM_PROMPT)
                .interact_text()
                .ok()?;
            if !input.is_empty() {
                selected.push(input);
            }
        }

        Some(selected.join(", "))
    } else {
        let mut items = sel.options.clone();
        if sel.allow_text {
            items.push(OTHER_OPTION.to_string());
        }

        let chosen = Select::with_theme(&theme)
            .with_prompt(&sel.question)
            .default(0)
            .items(&items)
            .interact_opt()
            .ok()?;

        let idx = chosen?;

        if sel.allow_text && idx == items.len() - 1 {
            let input = Input::<String>::with_theme(&theme)
                .with_prompt(FREEFORM_PROMPT)
                .interact_text()
                .ok()?;
            return Some(input);
        }

        Some(sel.options[idx].clone())
    }
}
