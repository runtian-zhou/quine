use std::path::{Path, PathBuf};
use std::time::Instant;

use crate::report;
use crate::types::*;

pub async fn run_suite(suite_path: &Path, model: &str, output_dir: &Path) -> anyhow::Result<()> {
    let suite_json = std::fs::read_to_string(suite_path)?;
    let suite: TestSuite = serde_json::from_str(&suite_json)?;

    println!("\x1b[1;36m=== Quine Eval ===\x1b[0m");
    println!("Suite: {}", suite.name);
    println!("Tests: {}", suite.tests.len());
    println!("Model: {}", model);
    println!();

    let quine_bin = find_quine_binary()?;
    let mut test_results = Vec::new();

    for (i, test) in suite.tests.iter().enumerate() {
        println!(
            "\x1b[1;33m[{}/{}] {}\x1b[0m: {}",
            i + 1,
            suite.tests.len(),
            test.id,
            test.description
        );

        print!("  Running...");
        let result = run_quine(&quine_bin, model, test).await?;
        let status = if result.files_check_passed {
            "\x1b[32mPASS\x1b[0m"
        } else {
            "\x1b[31mFAIL\x1b[0m"
        };
        println!(
            " {} ({}ms, files_ok={})",
            status, result.duration_ms, result.files_check_passed
        );

        test_results.push(TestResult {
            test_id: test.id.clone(),
            description: test.description.clone(),
            result,
        });
    }

    let summary = compute_summary(&test_results);

    let run_result = RunResult {
        suite_name: suite.name.clone(),
        timestamp: chrono::Utc::now(),
        model: model.to_string(),
        test_results,
        summary,
    };

    // Save results
    std::fs::create_dir_all(output_dir)?;
    let result_path = output_dir.join(format!(
        "eval_{}.json",
        run_result.timestamp.format("%Y%m%d_%H%M%S")
    ));
    let json = serde_json::to_string_pretty(&run_result)?;
    std::fs::write(&result_path, &json)?;

    println!();
    println!("\x1b[1;36m=== Results ===\x1b[0m");
    report::print_summary(&run_result);
    println!("\nFull results: {}", result_path.display());

    Ok(())
}

fn find_quine_binary() -> anyhow::Result<PathBuf> {
    let candidates = [
        PathBuf::from("target/release/quine"),
        PathBuf::from("target/debug/quine"),
    ];
    for c in &candidates {
        if c.exists() {
            return Ok(std::fs::canonicalize(c)?);
        }
    }
    if let Ok(output) = std::process::Command::new("which").arg("quine").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Ok(PathBuf::from(path));
            }
        }
    }
    anyhow::bail!("Could not find 'quine' binary. Run 'cargo build' first or install it on PATH.")
}

async fn run_quine(
    quine_bin: &Path,
    model: &str,
    test: &TestCase,
) -> anyhow::Result<ExecutionResult> {
    let tmp = tempfile::TempDir::new()?;
    setup_working_dir(tmp.path(), &test.setup_files)?;

    let start = Instant::now();
    let output = tokio::process::Command::new(quine_bin)
        .args([
            "chat",
            "--print",
            &test.prompt,
            "--model",
            model,
            "--output-format",
            "json",
            "--working-dir",
            &tmp.path().to_string_lossy(),
        ])
        .current_dir(tmp.path())
        .output()
        .await?;
    let duration_ms = start.elapsed().as_millis() as u64;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = output.status.code().unwrap_or(-1);

    let output_files = collect_output_files(tmp.path())?;
    let files_check_passed = check_expected_files(tmp.path(), &test.expected_files);

    Ok(ExecutionResult {
        stdout,
        stderr,
        exit_code,
        duration_ms,
        output_files,
        files_check_passed,
    })
}

fn setup_working_dir(dir: &Path, setup_files: &[SetupFile]) -> anyhow::Result<()> {
    for f in setup_files {
        let path = dir.join(&f.path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, &f.content)?;
    }
    Ok(())
}

fn collect_output_files(dir: &Path) -> anyhow::Result<Vec<OutputFile>> {
    let mut files = Vec::new();
    collect_files_recursive(dir, dir, &mut files)?;
    Ok(files)
}

fn collect_files_recursive(
    base: &Path,
    current: &Path,
    files: &mut Vec<OutputFile>,
) -> anyhow::Result<()> {
    if !current.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        // Skip hidden dirs (.quine, .git, etc)
        if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with('.') {
                continue;
            }
        }
        if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .to_string();
            // Only read text files (skip large/binary)
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.len() <= 50_000 {
                    files.push(OutputFile { path: rel, content });
                }
            }
        } else if path.is_dir() {
            collect_files_recursive(base, &path, files)?;
        }
    }
    Ok(())
}

fn check_expected_files(dir: &Path, expected: &[ExpectedFile]) -> bool {
    for ef in expected {
        let path = dir.join(&ef.path);
        if !path.exists() {
            return false;
        }
        if let Some(substr) = &ef.contains {
            match std::fs::read_to_string(&path) {
                Ok(content) => {
                    if !content.contains(substr.as_str()) {
                        return false;
                    }
                }
                Err(_) => return false,
            }
        }
    }
    true
}

fn compute_summary(results: &[TestResult]) -> Summary {
    let total = results.len();
    let passed = results
        .iter()
        .filter(|r| r.result.files_check_passed)
        .count();

    Summary {
        total_tests: total,
        passed,
        failed: total - passed,
    }
}
