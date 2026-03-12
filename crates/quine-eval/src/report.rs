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
    println!("\n\x1b[1;36m--- Summary ---\x1b[0m");
    println!("Suite:         {}", result.suite_name);
    println!("Timestamp:     {}", result.timestamp);
    println!("Quine model:   {}", result.quine_model);
    println!("Claude model:  {}", result.claude_model);
    println!("Judge model:   {}", result.judge_model);
    println!();
    println!("Total tests:   {}", s.total_tests);
    println!(
        "File checks:   Quine {}/{} | Claude {}/{}",
        s.quine_file_checks_passed, s.total_tests, s.claude_file_checks_passed, s.total_tests
    );

    if s.quine_wins + s.claude_wins + s.ties > 0 {
        println!();
        println!(
            "Wins:          Quine {} | Claude {} | Tie {}",
            s.quine_wins, s.claude_wins, s.ties
        );
        println!(
            "Avg score:     Quine {:.1} | Claude {:.1}",
            s.quine_avg_score, s.claude_avg_score
        );

        // Scoreboard bar
        let total_judged = s.quine_wins + s.claude_wins + s.ties;
        if total_judged > 0 {
            println!();
            let bar_width = 40;
            let q_width = (s.quine_wins as f64 / total_judged as f64 * bar_width as f64) as usize;
            let c_width =
                (s.claude_wins as f64 / total_judged as f64 * bar_width as f64) as usize;
            let t_width = bar_width - q_width - c_width;
            print!("  [");
            print!("\x1b[32m{}\x1b[0m", "Q".repeat(q_width));
            print!("\x1b[90m{}\x1b[0m", "=".repeat(t_width));
            print!("\x1b[34m{}\x1b[0m", "C".repeat(c_width));
            println!("]");
            println!(
                "   \x1b[32mQuine\x1b[0m {:>3}% {:>3}% \x1b[34mClaude\x1b[0m",
                (s.quine_wins as f64 / total_judged as f64 * 100.0) as u32,
                (s.claude_wins as f64 / total_judged as f64 * 100.0) as u32,
            );
        }
    }
}

fn print_details(result: &RunResult) {
    println!("\n\x1b[1;36m--- Per-Test Details ---\x1b[0m");
    for r in &result.test_results {
        println!(
            "\n\x1b[1;33m{}\x1b[0m: {}",
            r.test_id, r.description
        );
        println!(
            "  Quine:  exit={} time={}ms files_ok={} files={}",
            r.quine.exit_code,
            r.quine.duration_ms,
            r.quine.files_check_passed,
            r.quine.output_files.len()
        );
        println!(
            "  Claude: exit={} time={}ms files_ok={} files={}",
            r.claude.exit_code,
            r.claude.duration_ms,
            r.claude.files_check_passed,
            r.claude.output_files.len()
        );
        if let Some(ref j) = r.judge {
            println!(
                "  Judge:  winner=\x1b[1m{}\x1b[0m quine={}/10 claude={}/10",
                j.winner, j.quine_score, j.claude_score
            );
            println!("  Reasoning: {}", j.reasoning);
            for cs in &j.criteria_scores {
                println!(
                    "    - {}: quine={} claude={} ({})",
                    cs.criterion, cs.quine_score, cs.claude_score, cs.note
                );
            }
        }
    }
}
