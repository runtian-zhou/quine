//! Integration tests for the --json flag.
//!
//! These tests would run the compiled binary via `assert_cmd` and verify
//! end-to-end behavior. Since we don't have a real Cargo project to compile
//! against, these are written as realistic examples showing what the tests
//! would look like.
//!
//! In a real project, add `assert_cmd` and `predicates` to `[dev-dependencies]`:
//!
//! ```toml
//! [dev-dependencies]
//! assert_cmd = "2"
//! predicates = "3"
//! serde_json = "1"
//! ```

#[cfg(test)]
mod tests {
    use assert_cmd::Command;
    use serde_json::Value;

    fn quine_cmd() -> Command {
        Command::cargo_bin("quine").expect("binary should be built")
    }

    // -----------------------------------------------------------------------
    // Human output (default)
    // -----------------------------------------------------------------------

    #[test]
    fn status_human_output_contains_version_line() {
        let output = quine_cmd()
            .arg("status")
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(
            stdout.starts_with("Version:"),
            "Human status output must start with 'Version:', got: {}",
            stdout
        );
        assert!(output.status.success(), "Exit code must be 0");
    }

    // -----------------------------------------------------------------------
    // JSON output
    // -----------------------------------------------------------------------

    #[test]
    fn status_json_output_is_valid_json_with_correct_envelope() {
        let output = quine_cmd()
            .args(["--json", "status"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout)
            .expect("output must be valid JSON");

        assert_eq!(json["status"], "ok", "Envelope status must be 'ok'");
        assert_eq!(
            json["data"]["version"], "0.1.0",
            "Version in JSON must be exactly '0.1.0'"
        );
        assert_eq!(
            json["data"]["items_count"], 42,
            "Items count must be exactly 42"
        );
        assert!(output.status.success(), "Exit code must be 0");
    }

    #[test]
    fn json_short_flag_works() {
        let output = quine_cmd()
            .args(["-j", "status"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout)
            .expect("-j flag must produce valid JSON");

        assert_eq!(json["status"], "ok", "Envelope status must be 'ok'");
    }

    #[test]
    fn json_flag_after_subcommand_works() {
        let output = quine_cmd()
            .args(["status", "--json"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout)
            .expect("--json after subcommand must produce valid JSON");

        assert_eq!(json["status"], "ok", "Envelope status must be 'ok'");
    }

    #[test]
    fn list_json_output_contains_items_array() {
        let output = quine_cmd()
            .args(["--json", "list"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout).unwrap();

        assert_eq!(json["status"], "ok", "Envelope status must be 'ok'");
        let items = json["data"]["items"].as_array()
            .expect("data.items must be an array");
        assert_eq!(items.len(), 3, "Unfiltered list must have exactly 3 items");
        assert_eq!(
            json["data"]["total"], 3,
            "Total must be exactly 3 for unfiltered list"
        );
    }

    #[test]
    fn list_json_with_filter_returns_subset() {
        let output = quine_cmd()
            .args(["--json", "list", "--filter", "widget"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout).unwrap();

        let items = json["data"]["items"].as_array().unwrap();
        assert_eq!(items.len(), 2, "Filtering by 'widget' must return exactly 2 items (alpha, gamma)");
        assert_eq!(json["data"]["total"], 2, "Total must be exactly 2");
    }

    #[test]
    fn show_nonexistent_item_produces_json_error() {
        let output = quine_cmd()
            .args(["--json", "show", "nonexistent"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout)
            .expect("Error must also be valid JSON");

        assert_eq!(
            json["error"]["code"], "not_found",
            "Error code must be 'not_found'"
        );
        assert_eq!(
            json["error"]["message"], "Item 'nonexistent' not found",
            "Error message must match exactly"
        );
    }

    // -----------------------------------------------------------------------
    // Pipe to jq (simulated)
    // -----------------------------------------------------------------------

    #[test]
    fn json_output_is_parseable_by_jq_style_extraction() {
        // Simulate what `quine --json list | jq '.data.items[0].name'` does.
        let output = quine_cmd()
            .args(["--json", "list"])
            .output()
            .expect("command should run");

        let stdout = String::from_utf8(output.stdout).unwrap();
        let json: Value = serde_json::from_str(&stdout).unwrap();

        // This is the equivalent of: jq '.data.items[0].name'
        let first_name = json
            .pointer("/data/items/0/name")
            .expect("JSON pointer /data/items/0/name must exist");
        assert_eq!(
            first_name, "alpha",
            "First item name extracted via JSON pointer must be 'alpha'"
        );
    }
}
