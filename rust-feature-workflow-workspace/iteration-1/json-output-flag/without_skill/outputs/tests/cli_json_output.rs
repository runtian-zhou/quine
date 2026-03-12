//! Integration tests verifying that the --json flag produces valid, structured
//! JSON output for every subcommand.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

fn cmd() -> Command {
    Command::cargo_bin("quine-cli").expect("binary should be built")
}

// ---------------------------------------------------------------------------
// `list` subcommand
// ---------------------------------------------------------------------------

#[test]
fn list_human_output_does_not_contain_braces() {
    cmd()
        .args(["list", "."])
        .assert()
        .success()
        .stdout(predicate::str::contains("{").not());
}

#[test]
fn list_json_output_is_valid_json_array() {
    let output = cmd()
        .args(["--json", "list", "."])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(&stdout)
        .expect("stdout should be valid JSON");
    assert!(parsed.is_array(), "list --json should produce a JSON array");
}

#[test]
fn list_json_items_have_required_fields() {
    let output = cmd()
        .args(["--json", "list", "."])
        .output()
        .expect("command should run");

    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .unwrap();

    if let Some(arr) = parsed.as_array() {
        for item in arr {
            assert!(item.get("name").is_some(), "each item must have 'name'");
            assert!(item.get("kind").is_some(), "each item must have 'kind'");
            assert!(
                item.get("size_bytes").is_some(),
                "each item must have 'size_bytes'"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// `info` subcommand
// ---------------------------------------------------------------------------

#[test]
fn info_json_output_is_valid_json_object() {
    let output = cmd()
        .args(["--json", "info", "Cargo.toml"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .expect("stdout should be valid JSON");

    assert!(parsed.is_object(), "info --json should produce a JSON object");
    assert_eq!(parsed["name"], "Cargo.toml");
    assert_eq!(parsed["exists"], true);
    assert_eq!(parsed["is_file"], true);
    assert_eq!(parsed["is_directory"], false);
}

#[test]
fn info_json_nonexistent_file_reports_exists_false() {
    let output = cmd()
        .args(["--json", "info", "no-such-file-abc123"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .unwrap();

    assert_eq!(parsed["exists"], false);
    assert_eq!(parsed["size_bytes"], Value::Null);
}

// ---------------------------------------------------------------------------
// `search` subcommand
// ---------------------------------------------------------------------------

#[test]
fn search_json_output_has_expected_structure() {
    let output = cmd()
        .args(["--json", "search", "Cargo", "--limit", "5"])
        .output()
        .expect("command should run");

    assert!(output.status.success());
    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .expect("stdout should be valid JSON");

    assert!(parsed.is_object());
    assert_eq!(parsed["query"], "Cargo");
    assert!(parsed["total_results"].is_number());
    assert!(parsed["results"].is_array());
}

#[test]
fn search_json_results_have_path_and_score() {
    let output = cmd()
        .args(["--json", "search", "Cargo"])
        .output()
        .expect("command should run");

    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .unwrap();

    let results = parsed["results"].as_array().unwrap();
    assert!(
        !results.is_empty(),
        "searching for 'Cargo' in a Rust project should find at least 1 result"
    );
    for r in results {
        assert!(r.get("path").is_some(), "each result must have 'path'");
        assert!(r.get("score").is_some(), "each result must have 'score'");
        assert!(
            r["score"].as_f64().unwrap() > 0.0,
            "score must be a positive number"
        );
    }
}

// ---------------------------------------------------------------------------
// Global flag positioning
// ---------------------------------------------------------------------------

#[test]
fn json_flag_works_before_subcommand() {
    // --json before subcommand
    let output = cmd()
        .args(["--json", "info", "Cargo.toml"])
        .output()
        .unwrap();
    let parsed: Value = serde_json::from_str(
        &String::from_utf8_lossy(&output.stdout),
    )
    .unwrap();
    assert_eq!(parsed["name"], "Cargo.toml");
}

// ---------------------------------------------------------------------------
// Pipe-friendliness
// ---------------------------------------------------------------------------

#[test]
fn json_output_is_single_valid_document() {
    // The entire stdout must parse as exactly one JSON value (no trailing
    // garbage, no multiple objects concatenated).
    let output = cmd()
        .args(["--json", "list", "."])
        .output()
        .unwrap();

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut de = serde_json::Deserializer::from_str(&stdout).into_iter::<Value>();
    let first = de.next().expect("should have at least one JSON value");
    assert!(first.is_ok(), "first value should parse successfully");
    assert!(
        de.next().is_none(),
        "there should be exactly one top-level JSON value (pipe-safe)"
    );
}
