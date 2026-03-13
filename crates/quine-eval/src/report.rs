use std::path::Path;

use crate::types::RunResult;

pub fn show_results(path: &Path) -> anyhow::Result<()> {
    let json = std::fs::read_to_string(path)?;
    let result: RunResult = serde_json::from_str(&json)?;
    print_summary(&result);
    print_details(&result);
    Ok(())
}

pub fn print_summary(result: &RunResult) {
    let s = &result.summary;
    println!("\x1b[1;36m--- Summary ---\x1b[0m");
    println!("Suite:    {}", result.suite_name);
    println!("Model:    {}", result.model);
    println!("Time:     {}", result.timestamp);
    println!();
    println!("Total:    {}", s.total_tests);
    println!("Passed:   \x1b[32m{}\x1b[0m / {}", s.passed, s.total_tests);
    if s.failed > 0 {
        println!("Failed:   \x1b[31m{}\x1b[0m / {}", s.failed, s.total_tests);
    }

    // Progress bar
    let bar_width = 40;
    let pass_width = if s.total_tests > 0 {
        (s.passed as f64 / s.total_tests as f64 * bar_width as f64) as usize
    } else {
        0
    };
    let fail_width = bar_width - pass_width;
    print!("  [");
    print!("\x1b[32m{}\x1b[0m", "█".repeat(pass_width));
    print!("\x1b[31m{}\x1b[0m", "░".repeat(fail_width));
    println!("]");
}

fn print_details(result: &RunResult) {
    println!("\n\x1b[1;36m--- Per-Test Details ---\x1b[0m");
    for r in &result.test_results {
        let status = if r.result.files_check_passed {
            "\x1b[32mPASS\x1b[0m"
        } else {
            "\x1b[31mFAIL\x1b[0m"
        };
        println!(
            "\n  {} \x1b[1m{}\x1b[0m: {}",
            status, r.test_id, r.description
        );
        println!(
            "       exit={} time={}ms files={}",
            r.result.exit_code,
            r.result.duration_ms,
            r.result.output_files.len()
        );
        if !r.result.files_check_passed && r.result.exit_code != 0 {
            let stderr_preview = r.result.stderr.lines().next().unwrap_or("(no stderr)");
            println!("       stderr: {}", stderr_preview);
        }
    }
}
