use crate::types::*;
use serde::Deserialize;

pub async fn evaluate(
    model: &str,
    test: &TestCase,
    quine: &ExecutionResult,
    claude: &ExecutionResult,
) -> anyhow::Result<JudgeVerdict> {
    let api_key = std::env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY required for judge evaluation"))?;

    let criteria_section = if test.eval_criteria.is_empty() {
        "No specific criteria provided. Judge based on overall quality, correctness, and completeness.".to_string()
    } else {
        format!(
            "Evaluate against these criteria:\n{}",
            test.eval_criteria
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{}. {}", i + 1, c))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };

    let quine_files_summary = format_files(&quine.output_files);
    let claude_files_summary = format_files(&claude.output_files);

    let prompt = format!(
        r#"You are an impartial judge comparing two AI coding assistants (A and B) on the same task.

## Task
Description: {description}
Prompt given to both: "{prompt}"

## Evaluation Criteria
{criteria}

## Assistant A (Quine) Results
Exit code: {q_exit}
Duration: {q_dur}ms
Files check passed: {q_files_ok}

### Stdout:
{q_stdout}

### Files created/modified:
{q_files}

## Assistant B (Claude Code) Results
Exit code: {c_exit}
Duration: {c_dur}ms
Files check passed: {c_files_ok}

### Stdout:
{c_stdout}

### Files created/modified:
{c_files}

## Instructions
Compare the two results. Respond ONLY with a JSON object (no markdown, no extra text):
{{
  "winner": "quine" or "claude" or "tie",
  "quine_score": <0-10>,
  "claude_score": <0-10>,
  "reasoning": "<2-4 sentences explaining your judgment>",
  "criteria_scores": [
    {{
      "criterion": "<criterion text>",
      "quine_score": <0-10>,
      "claude_score": <0-10>,
      "note": "<brief note>"
    }}
  ]
}}"#,
        description = test.description,
        prompt = test.prompt,
        criteria = criteria_section,
        q_exit = quine.exit_code,
        q_dur = quine.duration_ms,
        q_files_ok = quine.files_check_passed,
        q_stdout = truncate_output(&quine.stdout, 3000),
        q_files = quine_files_summary,
        c_exit = claude.exit_code,
        c_dur = claude.duration_ms,
        c_files_ok = claude.files_check_passed,
        c_stdout = truncate_output(&claude.stdout, 3000),
        c_files = claude_files_summary,
    );

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.anthropic.com/v1/messages")
        .header("x-api-key", &api_key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .json(&serde_json::json!({
            "model": model,
            "max_tokens": 2048,
            "messages": [{"role": "user", "content": prompt}],
        }))
        .send()
        .await?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        anyhow::bail!("Judge API error ({}): {}", status, body);
    }

    let api_resp: ApiResponse = response.json().await?;

    let text = api_resp
        .content
        .iter()
        .filter_map(|b| {
            if b.r#type == "text" {
                b.text.as_deref()
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("");

    // Parse the JSON from the response, handling potential markdown wrapping
    let json_str = extract_json(&text);
    let verdict: JudgeVerdict = serde_json::from_str(json_str)
        .map_err(|e| anyhow::anyhow!("Failed to parse judge verdict: {}. Raw: {}", e, text))?;

    Ok(verdict)
}

fn format_files(files: &[OutputFile]) -> String {
    if files.is_empty() {
        return "(no files)".to_string();
    }
    files
        .iter()
        .map(|f| {
            let content = truncate_output(&f.content, 500);
            format!("--- {} ---\n{}", f.path, content)
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn truncate_output(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...\n(truncated, {} total chars)", &s[..max], s.len())
    }
}

fn extract_json(text: &str) -> &str {
    // Try to find JSON object in the response (handles markdown code blocks)
    let trimmed = text.trim();
    if trimmed.starts_with('{') {
        return trimmed;
    }
    // Look for ```json ... ``` or ``` ... ```
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }
    trimmed
}

#[derive(Deserialize)]
struct ApiResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    r#type: String,
    text: Option<String>,
}
