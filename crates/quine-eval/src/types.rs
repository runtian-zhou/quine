use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestSuite {
    pub name: String,
    pub description: String,
    pub tests: Vec<TestCase>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Unique identifier for this test
    pub id: String,
    /// Human-readable description
    pub description: String,
    /// The prompt to send to both CLIs
    pub prompt: String,
    /// Files to pre-create in the working directory before running
    #[serde(default)]
    pub setup_files: Vec<SetupFile>,
    /// Files that should exist after execution (for verification)
    #[serde(default)]
    pub expected_files: Vec<ExpectedFile>,
    /// Custom evaluation criteria for the LLM judge
    #[serde(default)]
    pub eval_criteria: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetupFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExpectedFile {
    pub path: String,
    /// If set, the file content must contain this substring
    #[serde(default)]
    pub contains: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub suite_name: String,
    pub timestamp: DateTime<Utc>,
    pub quine_model: String,
    pub claude_model: String,
    pub judge_model: String,
    pub test_results: Vec<TestResult>,
    pub summary: Summary,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub test_id: String,
    pub description: String,
    pub quine: ExecutionResult,
    pub claude: ExecutionResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judge: Option<JudgeVerdict>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub duration_ms: u64,
    /// Files that exist in the working dir after execution
    pub output_files: Vec<OutputFile>,
    /// Whether all expected_files checks passed
    pub files_check_passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputFile {
    pub path: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JudgeVerdict {
    /// Overall winner: "quine", "claude", or "tie"
    pub winner: String,
    /// Score for Quine (0-10)
    pub quine_score: u32,
    /// Score for Claude Code (0-10)
    pub claude_score: u32,
    /// Detailed reasoning
    pub reasoning: String,
    /// Per-criterion scores if eval_criteria were specified
    #[serde(default)]
    pub criteria_scores: Vec<CriterionScore>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriterionScore {
    pub criterion: String,
    pub quine_score: u32,
    pub claude_score: u32,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub total_tests: usize,
    pub quine_wins: usize,
    pub claude_wins: usize,
    pub ties: usize,
    pub quine_avg_score: f64,
    pub claude_avg_score: f64,
    pub quine_file_checks_passed: usize,
    pub claude_file_checks_passed: usize,
}

impl TestSuite {
    pub fn example() -> Self {
        TestSuite {
            name: "Example Test Suite".to_string(),
            description: "Sample tests to compare Claude Code vs Quine".to_string(),
            tests: vec![
                TestCase {
                    id: "hello-world".to_string(),
                    description: "Create a hello world program".to_string(),
                    prompt: "Create a file called hello.py that prints 'Hello, World!'".to_string(),
                    setup_files: vec![],
                    expected_files: vec![ExpectedFile {
                        path: "hello.py".to_string(),
                        contains: Some("Hello, World!".to_string()),
                    }],
                    eval_criteria: vec![
                        "Code is correct and runnable".to_string(),
                        "File was created with the right name".to_string(),
                    ],
                },
                TestCase {
                    id: "read-and-summarize".to_string(),
                    description: "Read a file and answer a question about it".to_string(),
                    prompt: "Read config.toml and tell me what the project name is.".to_string(),
                    setup_files: vec![SetupFile {
                        path: "config.toml".to_string(),
                        content: "[package]\nname = \"my-awesome-project\"\nversion = \"1.0.0\"\n"
                            .to_string(),
                    }],
                    expected_files: vec![],
                    eval_criteria: vec![
                        "Correctly identifies the project name as 'my-awesome-project'".to_string(),
                    ],
                },
                TestCase {
                    id: "edit-file".to_string(),
                    description: "Edit an existing file".to_string(),
                    prompt: "In app.rs, change the greeting from 'Hello' to 'Goodbye'.".to_string(),
                    setup_files: vec![SetupFile {
                        path: "app.rs".to_string(),
                        content: "fn greet() -> &'static str {\n    \"Hello\"\n}\n".to_string(),
                    }],
                    expected_files: vec![ExpectedFile {
                        path: "app.rs".to_string(),
                        contains: Some("Goodbye".to_string()),
                    }],
                    eval_criteria: vec![
                        "The edit was applied correctly".to_string(),
                        "No other content was unnecessarily changed".to_string(),
                    ],
                },
            ],
        }
    }
}
