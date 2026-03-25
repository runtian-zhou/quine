use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Metadata section of a test scenario.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct ScenarioMeta {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Timeout in seconds for the entire scenario. Defaults to 120.
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

fn default_timeout() -> u64 {
    120
}

/// JSON extraction configuration for an action.
#[derive(Debug, Clone, Deserialize)]
pub struct ExtractJson {
    /// JSON field to extract from stdout.
    pub field: String,
    /// Optional regex to apply to the field value.
    #[serde(default)]
    pub regex: Option<String>,
    /// Variable name to save the extracted value to.
    pub save: String,
}

/// A single action within a test scenario.
#[derive(Debug, Deserialize)]
pub struct Action {
    /// Action type: run, respond, spawn, signal, send, recv, ps, sleep, assert.
    #[serde(rename = "type")]
    pub action_type: String,
    /// Message to send (for `run`, `send`).
    #[serde(default)]
    pub message: Option<String>,
    /// Session variable reference (for `run`, `respond`, `signal`).
    #[serde(default)]
    pub session: Option<String>,
    /// Response text (for `respond`).
    #[serde(default)]
    pub response: Option<String>,
    /// Task description (for `spawn`).
    #[serde(default)]
    pub task: Option<String>,
    /// Parent session (for `spawn`).
    #[serde(default)]
    pub parent: Option<String>,
    /// System prompt (for `spawn`).
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Target session (for `send`).
    #[serde(default)]
    pub target: Option<String>,
    /// Source session (for `recv`).
    #[serde(default)]
    pub source: Option<String>,
    /// Non-blocking flag (for `recv`).
    #[serde(default)]
    pub non_blocking: Option<bool>,
    /// Signal type (for `signal`): term, kill, stop, continue.
    #[serde(default)]
    pub signal: Option<String>,
    /// Sleep duration in seconds (for `sleep`).
    #[serde(default)]
    pub seconds: Option<f64>,
    /// Variable name for assert.
    #[serde(default)]
    pub var: Option<String>,
    /// Assert: value contains this string.
    #[serde(default)]
    pub contains: Option<String>,
    /// Assert: value equals this string.
    #[serde(default)]
    pub equals: Option<String>,
    /// Expected substring in stdout.
    #[serde(default)]
    pub expect_contains: Option<String>,
    /// Expect an interaction_needed response.
    #[serde(default)]
    pub expect_interaction: Option<bool>,
    /// Save session ID to this variable name.
    #[serde(default)]
    pub save_session: Option<String>,
    /// Additional CLI flags.
    #[serde(default)]
    pub flags: Vec<String>,
    /// Extract a JSON field from stdout, optionally apply regex, save to variable.
    #[serde(default)]
    pub extract_json: Option<ExtractJson>,
}

/// A complete test scenario parsed from TOML.
#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub meta: ScenarioMeta,
    pub action: Vec<Action>,
}

/// Result of running a single action.
#[derive(Debug, Serialize)]
pub struct ActionResult {
    pub action_index: usize,
    pub action_type: String,
    pub passed: bool,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of running an entire scenario.
#[derive(Debug, Serialize)]
pub struct ScenarioResult {
    pub name: String,
    pub passed: bool,
    pub actions: Vec<ActionResult>,
    pub duration_ms: u64,
}

/// The test runner that executes scenarios against the daemon.
pub struct TestRunner {
    socket_path: Option<String>,
    variables: HashMap<String, String>,
    cargo_bin: PathBuf,
}

impl TestRunner {
    pub fn new(socket_path: Option<String>) -> Self {
        // Determine the cargo binary path. During tests or normal use,
        // we shell out to `cargo run --bin quine --`.
        Self {
            socket_path,
            variables: HashMap::new(),
            cargo_bin: PathBuf::from("cargo"),
        }
    }

    /// Interpolate variables in a string. Replaces `$varname` with the stored value.
    fn interpolate(&self, input: &str) -> String {
        let mut result = input.to_string();
        for (key, value) in &self.variables {
            let pattern = format!("${key}");
            result = result.replace(&pattern, value);
        }
        result
    }

    /// Run all scenario TOML files in a directory.
    pub async fn run_directory(&mut self, dir: &Path) -> anyhow::Result<Vec<ScenarioResult>> {
        let mut results = Vec::new();
        let mut entries: Vec<PathBuf> = Vec::new();

        let mut read_dir = tokio::fs::read_dir(dir).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                entries.push(path);
            }
        }
        entries.sort();

        for path in entries {
            let content = tokio::fs::read_to_string(&path).await?;
            let scenario: Scenario = toml::from_str(&content)?;
            // Reset variables between scenarios.
            self.variables.clear();
            let result = self.run_scenario(&scenario).await;
            results.push(result);
        }

        Ok(results)
    }

    /// Run a single scenario and return the result.
    pub async fn run_scenario(&mut self, scenario: &Scenario) -> ScenarioResult {
        let start = Instant::now();
        let timeout = std::time::Duration::from_secs(scenario.meta.timeout);
        let mut action_results = Vec::new();
        let mut all_passed = true;

        for (index, action) in scenario.action.iter().enumerate() {
            // Check scenario-level timeout.
            if start.elapsed() > timeout {
                action_results.push(ActionResult {
                    action_index: index,
                    action_type: action.action_type.clone(),
                    passed: false,
                    stdout: String::new(),
                    stderr: String::new(),
                    error: Some("Scenario timeout exceeded".to_string()),
                });
                all_passed = false;
                break;
            }

            let result = self.execute_action(index, action).await;
            if !result.passed {
                all_passed = false;
                action_results.push(result);
                break; // Stop on first failure as specified.
            }
            action_results.push(result);
        }

        ScenarioResult {
            name: scenario.meta.name.clone(),
            passed: all_passed,
            actions: action_results,
            duration_ms: start.elapsed().as_millis() as u64,
        }
    }

    /// Execute a single action and return the result.
    async fn execute_action(&mut self, index: usize, action: &Action) -> ActionResult {
        match action.action_type.as_str() {
            "run" => self.execute_run(index, action).await,
            "respond" => self.execute_respond(index, action).await,
            "spawn" => self.execute_spawn(index, action).await,
            "signal" => self.execute_signal(index, action).await,
            "send" => self.execute_send(index, action).await,
            "recv" => self.execute_recv(index, action).await,
            "ps" => self.execute_ps(index, action).await,
            "sleep" => self.execute_sleep(index, action).await,
            "assert" => self.execute_assert(index, action),
            other => ActionResult {
                action_index: index,
                action_type: other.to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(format!("Unknown action type: {other}")),
            },
        }
    }

    /// Build the base command: `cargo run --bin quine --`
    fn base_command(&self) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new(self.cargo_bin.as_os_str());
        cmd.args(["run", "--bin", "quine", "--"]);
        cmd
    }

    /// Add socket arg if configured.
    fn add_socket_arg(&self, cmd: &mut tokio::process::Command) {
        if let Some(ref socket) = self.socket_path {
            cmd.args(["--socket", socket]);
        }
    }

    /// Run the command and capture output with a per-action timeout of 120s.
    async fn run_command(
        &self,
        mut cmd: tokio::process::Command,
    ) -> Result<std::process::Output, String> {
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), cmd.output()).await;

        match result {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(e)) => Err(format!("Command execution error: {e}")),
            Err(_) => Err("Action timed out after 120 seconds".to_string()),
        }
    }

    /// Execute a `run` action.
    async fn execute_run(&mut self, index: usize, action: &Action) -> ActionResult {
        let message = action
            .message
            .as_deref()
            .map(|m| self.interpolate(m))
            .unwrap_or_default();

        let mut cmd = self.base_command();
        cmd.arg("run");

        if let Some(ref session) = action.session {
            let session = self.interpolate(session);
            cmd.args(["--session", &session]);
        }

        // Always use --json for programmatic parsing.
        let use_json = action.flags.contains(&"--json".to_string())
            || action.save_session.is_some()
            || action.extract_json.is_some();
        if use_json {
            cmd.arg("--json");
        }

        self.add_socket_arg(&mut cmd);
        cmd.arg(&message);

        self.run_and_evaluate(index, "run", action, cmd, use_json)
            .await
    }

    /// Execute a `respond` action.
    async fn execute_respond(&mut self, index: usize, action: &Action) -> ActionResult {
        let session = action
            .session
            .as_deref()
            .map(|s| self.interpolate(s))
            .unwrap_or_default();
        let response = action
            .response
            .as_deref()
            .map(|r| self.interpolate(r))
            .unwrap_or_default();

        let mut cmd = self.base_command();
        cmd.arg("respond");
        cmd.args(["--session", &session]);

        let use_json = action.flags.contains(&"--json".to_string());
        if use_json {
            cmd.arg("--json");
        }

        self.add_socket_arg(&mut cmd);
        cmd.arg(&response);

        self.run_and_evaluate(index, "respond", action, cmd, use_json)
            .await
    }

    /// Execute a `spawn` action.
    async fn execute_spawn(&mut self, index: usize, action: &Action) -> ActionResult {
        let task = action
            .task
            .as_deref()
            .map(|t| self.interpolate(t))
            .unwrap_or_default();

        let mut cmd = self.base_command();
        cmd.arg("spawn");

        if let Some(ref parent) = action.parent {
            let parent = self.interpolate(parent);
            cmd.args(["--parent", &parent]);
        }
        if let Some(ref system_prompt) = action.system_prompt {
            let sp = self.interpolate(system_prompt);
            cmd.args(["--system-prompt", &sp]);
        }

        cmd.arg("--json");
        self.add_socket_arg(&mut cmd);
        cmd.arg(&task);

        self.run_and_evaluate(index, "spawn", action, cmd, true)
            .await
    }

    /// Execute a `signal` action.
    async fn execute_signal(&mut self, index: usize, action: &Action) -> ActionResult {
        let session = action
            .session
            .as_deref()
            .map(|s| self.interpolate(s))
            .unwrap_or_default();
        let signal = action
            .signal
            .as_deref()
            .map(|s| self.interpolate(s))
            .unwrap_or_else(|| "term".to_string());

        let mut cmd = self.base_command();
        cmd.arg("signal");
        self.add_socket_arg(&mut cmd);
        cmd.args([&session, &signal]);

        let output = self.run_command(cmd).await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                ActionResult {
                    action_index: index,
                    action_type: "signal".to_string(),
                    passed: out.status.success(),
                    stdout,
                    stderr: stderr.clone(),
                    error: if out.status.success() {
                        None
                    } else {
                        Some(stderr)
                    },
                }
            }
            Err(e) => ActionResult {
                action_index: index,
                action_type: "signal".to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            },
        }
    }

    /// Execute a `send` action.
    async fn execute_send(&mut self, index: usize, action: &Action) -> ActionResult {
        let target = action
            .target
            .as_deref()
            .map(|t| self.interpolate(t))
            .unwrap_or_default();
        let message = action
            .message
            .as_deref()
            .map(|m| self.interpolate(m))
            .unwrap_or_default();

        let mut cmd = self.base_command();
        cmd.arg("send");
        self.add_socket_arg(&mut cmd);
        cmd.args([&target, &message]);

        let output = self.run_command(cmd).await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                ActionResult {
                    action_index: index,
                    action_type: "send".to_string(),
                    passed: out.status.success(),
                    stdout,
                    stderr: stderr.clone(),
                    error: if out.status.success() {
                        None
                    } else {
                        Some(stderr)
                    },
                }
            }
            Err(e) => ActionResult {
                action_index: index,
                action_type: "send".to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            },
        }
    }

    /// Execute a `recv` action.
    async fn execute_recv(&mut self, index: usize, action: &Action) -> ActionResult {
        let source = action
            .source
            .as_deref()
            .map(|s| self.interpolate(s))
            .unwrap_or_default();

        let mut cmd = self.base_command();
        cmd.arg("recv");
        self.add_socket_arg(&mut cmd);

        if action.non_blocking.unwrap_or(false) {
            cmd.arg("--non-blocking");
        }

        cmd.arg(&source);

        let output = self.run_command(cmd).await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();

                let mut passed = out.status.success();
                let mut error = None;

                // Check expect_contains.
                if let Some(ref expect) = action.expect_contains {
                    let expect = self.interpolate(expect);
                    if !stdout.contains(&expect) && !stderr.contains(&expect) {
                        passed = false;
                        error = Some(format!("Expected output to contain '{expect}'"));
                    }
                }

                ActionResult {
                    action_index: index,
                    action_type: "recv".to_string(),
                    passed,
                    stdout,
                    stderr,
                    error,
                }
            }
            Err(e) => ActionResult {
                action_index: index,
                action_type: "recv".to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            },
        }
    }

    /// Execute a `ps` action.
    async fn execute_ps(&mut self, index: usize, action: &Action) -> ActionResult {
        let mut cmd = self.base_command();
        cmd.arg("ps");
        self.add_socket_arg(&mut cmd);

        for flag in &action.flags {
            let flag = self.interpolate(flag);
            cmd.arg(&flag);
        }

        let output = self.run_command(cmd).await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();

                let mut passed = out.status.success();
                let mut error = None;

                if let Some(ref expect) = action.expect_contains {
                    let expect = self.interpolate(expect);
                    if !stdout.contains(&expect) && !stderr.contains(&expect) {
                        passed = false;
                        error = Some(format!("Expected ps output to contain '{expect}'"));
                    }
                }

                ActionResult {
                    action_index: index,
                    action_type: "ps".to_string(),
                    passed,
                    stdout,
                    stderr,
                    error,
                }
            }
            Err(e) => ActionResult {
                action_index: index,
                action_type: "ps".to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            },
        }
    }

    /// Execute a `sleep` action.
    async fn execute_sleep(&self, index: usize, action: &Action) -> ActionResult {
        let seconds = action.seconds.unwrap_or(1.0);
        tokio::time::sleep(std::time::Duration::from_secs_f64(seconds)).await;
        ActionResult {
            action_index: index,
            action_type: "sleep".to_string(),
            passed: true,
            stdout: String::new(),
            stderr: String::new(),
            error: None,
        }
    }

    /// Execute an `assert` action.
    fn execute_assert(&self, index: usize, action: &Action) -> ActionResult {
        let var_name = action.var.as_deref().unwrap_or("");
        let value = self.variables.get(var_name).cloned().unwrap_or_default();

        let mut passed = true;
        let mut error = None;

        if let Some(ref contains) = action.contains {
            let contains = self.interpolate(contains);
            if !value.contains(&contains) {
                passed = false;
                error = Some(format!(
                    "Variable '{var_name}' value '{value}' does not contain '{contains}'"
                ));
            }
        }

        if let Some(ref equals) = action.equals {
            let equals = self.interpolate(equals);
            if value != equals {
                passed = false;
                error = Some(format!(
                    "Variable '{var_name}' value '{value}' does not equal '{equals}'"
                ));
            }
        }

        ActionResult {
            action_index: index,
            action_type: "assert".to_string(),
            passed,
            stdout: value,
            stderr: String::new(),
            error,
        }
    }

    /// Common logic for run/respond/spawn: execute command, evaluate expectations, save variables.
    async fn run_and_evaluate(
        &mut self,
        index: usize,
        action_type: &str,
        action: &Action,
        cmd: tokio::process::Command,
        json_mode: bool,
    ) -> ActionResult {
        let output = self.run_command(cmd).await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                let combined = format!("{stdout}\n{stderr}");

                let mut passed = true;
                let mut error = None;

                // Check expect_interaction.
                if action.expect_interaction.unwrap_or(false) {
                    let has_interaction = stdout.contains("interaction_needed")
                        || stdout.contains("interaction needed")
                        || stderr.contains("interaction needed");
                    if !has_interaction {
                        passed = false;
                        error = Some(
                            "Expected interaction_needed but none found in output".to_string(),
                        );
                    }
                }

                // Check expect_contains.
                if let Some(ref expect) = action.expect_contains {
                    let expect = self.interpolate(expect);
                    if !combined.contains(&expect) {
                        passed = false;
                        error = Some(format!("Expected output to contain '{expect}'"));
                    }
                }

                // Save session_id if requested.
                if let Some(ref var_name) = action.save_session {
                    if json_mode {
                        // Try to extract session_id from JSON stdout.
                        if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                            if let Some(sid) = json_val.get("session_id").and_then(|v| v.as_str()) {
                                self.variables.insert(var_name.clone(), sid.to_string());
                            }
                        }
                    }
                    // Also try to extract from stderr line like "session: <id>"
                    for line in stderr.lines() {
                        if let Some(sid) = line.strip_prefix("session: ") {
                            self.variables
                                .insert(var_name.clone(), sid.trim().to_string());
                        }
                    }
                }

                // Extract JSON field if requested.
                if let Some(ref extract) = action.extract_json {
                    if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(&stdout) {
                        let field_value = json_val
                            .get(&extract.field)
                            .map(|v| {
                                if v.is_string() {
                                    v.as_str().unwrap().to_string()
                                } else {
                                    v.to_string()
                                }
                            })
                            .unwrap_or_default();

                        let extracted = if let Some(ref regex_pattern) = extract.regex {
                            match regex::Regex::new(regex_pattern) {
                                Ok(re) => {
                                    if let Some(caps) = re.captures(&field_value) {
                                        caps.get(1).map(|m| m.as_str().to_string()).unwrap_or(
                                            caps.get(0)
                                                .map(|m| m.as_str().to_string())
                                                .unwrap_or_default(),
                                        )
                                    } else {
                                        field_value
                                    }
                                }
                                Err(e) => {
                                    passed = false;
                                    error = Some(format!("Invalid regex '{}': {e}", regex_pattern));
                                    field_value
                                }
                            }
                        } else {
                            field_value
                        };

                        self.variables.insert(extract.save.clone(), extracted);
                    } else if passed {
                        // Only fail if we haven't already failed.
                        passed = false;
                        error = Some("Failed to parse stdout as JSON for extract_json".to_string());
                    }
                }

                // If command failed and we haven't set a specific error yet.
                if !out.status.success()
                    && error.is_none()
                    && !action.expect_interaction.unwrap_or(false)
                {
                    passed = false;
                    error = Some(format!(
                        "Command exited with status {}",
                        out.status.code().unwrap_or(-1)
                    ));
                }

                ActionResult {
                    action_index: index,
                    action_type: action_type.to_string(),
                    passed,
                    stdout,
                    stderr,
                    error,
                }
            }
            Err(e) => ActionResult {
                action_index: index,
                action_type: action_type.to_string(),
                passed: false,
                stdout: String::new(),
                stderr: String::new(),
                error: Some(e),
            },
        }
    }
}

/// Parse a scenario from a TOML string.
pub fn parse_scenario(toml_str: &str) -> Result<Scenario, toml::de::Error> {
    toml::from_str(toml_str)
}

/// Run a test scenario file or directory and print results.
pub async fn run_test(
    scenario_path: &str,
    socket: Option<String>,
    json_output: bool,
) -> anyhow::Result<()> {
    let path = Path::new(scenario_path);

    let mut runner = TestRunner::new(socket);

    if path.is_dir() {
        let results = runner.run_directory(path).await?;
        print_results(&results, json_output)?;

        let all_passed = results.iter().all(|r| r.passed);
        if !all_passed {
            std::process::exit(1);
        }
    } else if path.is_file() {
        let content = tokio::fs::read_to_string(path).await?;
        let scenario = parse_scenario(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse scenario {}: {e}", path.display()))?;
        let result = runner.run_scenario(&scenario).await;
        print_results(&[result], json_output)?;

        // We already printed; check the last result.
        // (re-read isn't needed, we can check the exit.)
    } else {
        anyhow::bail!("Scenario path does not exist: {}", path.display());
    }

    Ok(())
}

/// Print results to stdout.
fn print_results(results: &[ScenarioResult], json_output: bool) -> anyhow::Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(results)?);
        return Ok(());
    }

    println!("=== Test Results ===\n");
    println!(
        "{:<30} | {:<6} | {:<8} | Details",
        "Scenario", "Status", "Time(ms)"
    );
    println!("{:-<30}-+-{:-<6}-+-{:-<8}-+-{:-<40}", "", "", "", "");

    let mut total = 0;
    let mut passed = 0;

    for result in results {
        total += 1;
        let status = if result.passed {
            passed += 1;
            "PASS"
        } else {
            "FAIL"
        };

        let details = if result.passed {
            format!("{} actions OK", result.actions.len())
        } else {
            result
                .actions
                .iter()
                .find(|a| !a.passed)
                .and_then(|a| a.error.clone())
                .unwrap_or_else(|| "Unknown failure".to_string())
        };

        println!(
            "{:<30} | {:<6} | {:<8} | {}",
            result.name, status, result.duration_ms, details
        );
    }

    let failed = total - passed;
    println!("\nTotal: {total} | Passed: {passed} | Failed: {failed}");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_scenario_basic() {
        let toml_str = r#"
[meta]
name = "test_basic"
description = "A basic test"
timeout = 60

[[action]]
type = "run"
message = "Hello"
expect_contains = "world"

[[action]]
type = "sleep"
seconds = 1.5
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.meta.name, "test_basic");
        assert_eq!(scenario.meta.description, "A basic test");
        assert_eq!(scenario.meta.timeout, 60);
        assert_eq!(scenario.action.len(), 2);
        assert_eq!(scenario.action[0].action_type, "run");
        assert_eq!(scenario.action[0].message.as_deref(), Some("Hello"));
        assert_eq!(scenario.action[0].expect_contains.as_deref(), Some("world"));
        assert_eq!(scenario.action[1].action_type, "sleep");
        assert_eq!(scenario.action[1].seconds, Some(1.5));
    }

    #[test]
    fn test_parse_scenario_with_variables() {
        let toml_str = r#"
[meta]
name = "test_vars"
description = "Test variable interpolation"

[[action]]
type = "run"
message = "Do something"
save_session = "sid"
flags = ["--json"]

[[action]]
type = "respond"
session = "$sid"
response = "Yes"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.action.len(), 2);
        assert_eq!(scenario.action[0].save_session.as_deref(), Some("sid"));
        assert_eq!(scenario.action[1].session.as_deref(), Some("$sid"));
        assert_eq!(scenario.action[1].response.as_deref(), Some("Yes"));
    }

    #[test]
    fn test_parse_scenario_with_extract_json() {
        let toml_str = r#"
[meta]
name = "test_extract"
description = "Test extract_json"

[[action]]
type = "run"
message = "Spawn child"
flags = ["--json"]
save_session = "parent"

[action.extract_json]
field = "response"
regex = 'SessionId\(([^)]+)\)'
save = "child_id"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        let extract = scenario.action[0].extract_json.as_ref().unwrap();
        assert_eq!(extract.field, "response");
        assert_eq!(extract.regex.as_deref(), Some(r"SessionId\(([^)]+)\)"));
        assert_eq!(extract.save, "child_id");
    }

    #[test]
    fn test_parse_scenario_default_timeout() {
        let toml_str = r#"
[meta]
name = "no_timeout"
description = "No timeout specified"

[[action]]
type = "sleep"
seconds = 1.0
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.meta.timeout, 120);
    }

    #[test]
    fn test_variable_interpolation() {
        let mut runner = TestRunner::new(None);
        runner
            .variables
            .insert("sid".to_string(), "abc-123".to_string());
        runner
            .variables
            .insert("name".to_string(), "Alice".to_string());

        assert_eq!(runner.interpolate("$sid"), "abc-123");
        assert_eq!(runner.interpolate("session=$sid"), "session=abc-123");
        assert_eq!(
            runner.interpolate("Hello $name, session $sid"),
            "Hello Alice, session abc-123"
        );
        assert_eq!(runner.interpolate("no vars here"), "no vars here");
    }

    #[test]
    fn test_assert_action_contains() {
        let runner = TestRunner::new(None);
        let action = Action {
            action_type: "assert".to_string(),
            var: Some("missing".to_string()),
            contains: Some("hello".to_string()),
            message: None,
            session: None,
            response: None,
            task: None,
            parent: None,
            system_prompt: None,
            target: None,
            source: None,
            non_blocking: None,
            signal: None,
            seconds: None,
            equals: None,
            expect_contains: None,
            expect_interaction: None,
            save_session: None,
            flags: vec![],
            extract_json: None,
        };
        let result = runner.execute_assert(0, &action);
        assert!(!result.passed);
        assert!(result.error.as_ref().unwrap().contains("does not contain"));
    }

    #[test]
    fn test_assert_action_equals() {
        let mut runner = TestRunner::new(None);
        runner
            .variables
            .insert("val".to_string(), "exact".to_string());

        let action = Action {
            action_type: "assert".to_string(),
            var: Some("val".to_string()),
            equals: Some("exact".to_string()),
            contains: None,
            message: None,
            session: None,
            response: None,
            task: None,
            parent: None,
            system_prompt: None,
            target: None,
            source: None,
            non_blocking: None,
            signal: None,
            seconds: None,
            expect_contains: None,
            expect_interaction: None,
            save_session: None,
            flags: vec![],
            extract_json: None,
        };
        let result = runner.execute_assert(0, &action);
        assert!(result.passed);
        assert!(result.error.is_none());
    }

    #[test]
    fn test_scenario_result_serialization() {
        let result = ScenarioResult {
            name: "test".to_string(),
            passed: true,
            actions: vec![ActionResult {
                action_index: 0,
                action_type: "run".to_string(),
                passed: true,
                stdout: "hello".to_string(),
                stderr: String::new(),
                error: None,
            }],
            duration_ms: 100,
        };
        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("\"name\":\"test\""));
        assert!(json.contains("\"passed\":true"));
        assert!(json.contains("\"duration_ms\":100"));
    }

    #[test]
    fn test_parse_all_action_types() {
        let toml_str = r#"
[meta]
name = "all_types"
description = "Test all action types parse"

[[action]]
type = "run"
message = "hello"

[[action]]
type = "respond"
session = "$sid"
response = "yes"

[[action]]
type = "spawn"
task = "do something"
parent = "$pid"

[[action]]
type = "signal"
session = "$sid"
signal = "term"

[[action]]
type = "send"
target = "$tid"
message = "msg"

[[action]]
type = "recv"
source = "$src"
non_blocking = true

[[action]]
type = "ps"
flags = ["--tree"]
expect_contains = "child"

[[action]]
type = "sleep"
seconds = 2.0

[[action]]
type = "assert"
var = "myvar"
contains = "expected"
"#;
        let scenario = parse_scenario(toml_str).unwrap();
        assert_eq!(scenario.action.len(), 9);
        assert_eq!(scenario.action[0].action_type, "run");
        assert_eq!(scenario.action[1].action_type, "respond");
        assert_eq!(scenario.action[2].action_type, "spawn");
        assert_eq!(scenario.action[3].action_type, "signal");
        assert_eq!(scenario.action[4].action_type, "send");
        assert_eq!(scenario.action[5].action_type, "recv");
        assert_eq!(scenario.action[6].action_type, "ps");
        assert_eq!(scenario.action[7].action_type, "sleep");
        assert_eq!(scenario.action[8].action_type, "assert");
    }
}
