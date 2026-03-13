use crossterm::style::{Color, SetForegroundColor, ResetColor};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;

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
                print!("\r\x1b[90m{} {}\x1b[0m", frames[i % frames.len()], label);
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

    pub fn stop(mut self) {
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

pub struct Renderer {
    syntax_set: SyntaxSet,
    theme_set: ThemeSet,
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            syntax_set: SyntaxSet::load_defaults_newlines(),
            theme_set: ThemeSet::load_defaults(),
        }
    }

    /// Print a streamed content delta (no newline, flush immediately)
    pub fn print_delta(&self, text: &str) {
        print!("{}", text);
        std::io::stdout().flush().ok();
    }

    /// Print a complete assistant message with markdown/syntax highlighting
    pub fn print_assistant_message(&self, content: &str) {
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
            println!("\x1b[1;36m{}\x1b[0m", line);
        } else if line.starts_with("## ") {
            println!("\x1b[1;33m{}\x1b[0m", line);
        } else if line.starts_with("### ") {
            println!("\x1b[1;32m{}\x1b[0m", line);
        } else if line.starts_with("- ") || line.starts_with("* ") {
            println!("  {}{}{}", SetForegroundColor(Color::Cyan), "•", ResetColor);
            print!(" {}\n", &line[2..]);
        } else if line.starts_with("> ") {
            println!("\x1b[3;37m  │ {}\x1b[0m", &line[2..]);
        } else {
            println!("{}", line);
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

        println!("\x1b[90m┌─ {} ─┐\x1b[0m", if lang.is_empty() { "code" } else { lang });
        for line in code.lines() {
            if let Ok(ranges) = highlighter.highlight_line(line, &self.syntax_set) {
                let escaped = as_24_bit_terminal_escaped(&ranges, false);
                println!("\x1b[90m│\x1b[0m {}\x1b[0m", escaped);
            } else {
                println!("\x1b[90m│\x1b[0m {}", line);
            }
        }
        println!("\x1b[90m└──────┘\x1b[0m");
    }

    pub fn print_tool_call(&self, name: &str, args: &serde_json::Value) {
        println!(
            "\n\x1b[1;33m⚙ Tool: {}\x1b[0m",
            name
        );
        if let Some(obj) = args.as_object() {
            for (key, value) in obj {
                let display = match value {
                    serde_json::Value::String(s) => {
                        if s.len() > 80 {
                            format!("{}...", &s[..80])
                        } else {
                            s.clone()
                        }
                    }
                    other => {
                        let s = other.to_string();
                        if s.len() > 80 {
                            format!("{}...", &s[..80])
                        } else {
                            s
                        }
                    }
                };
                println!("  \x1b[90m{}: {}\x1b[0m", key, display);
            }
        }
    }

    pub fn print_tool_result(&self, success: bool, output: &str) {
        if success {
            println!("\x1b[32m  ✓ Success\x1b[0m");
        } else {
            println!("\x1b[31m  ✗ Failed\x1b[0m");
        }
        // Print first few lines of output
        let lines: Vec<&str> = output.lines().take(5).collect();
        for line in &lines {
            println!("  \x1b[90m{}\x1b[0m", line);
        }
        let total_lines = output.lines().count();
        if total_lines > 5 {
            println!("  \x1b[90m... ({} more lines)\x1b[0m", total_lines - 5);
        }
    }
}
