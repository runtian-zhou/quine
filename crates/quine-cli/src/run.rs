use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::client::IpcClient;
use crate::interaction::{maybe_auto_approve, prompt as interaction_prompt, source_label};
use crate::session::resolve_resume_target;
use quine_harness::{
    protocol::{methods, notifications},
    PermissionPromptBehavior,
};

fn print_resume_command(socket_path: &Path, session_id: &str) {
    eprintln!(
        "Resume from this checkpoint with: `quine run --session {} --socket {} \"<message>\"`",
        session_id,
        socket_path.display()
    );
}

/// Structured JSON output for one-shot mode.
#[derive(Debug, Serialize, Deserialize)]
pub struct OneshotOutput {
    /// The session ID used for this run.
    pub session_id: String,
    /// The full text response from the agent.
    pub response: String,
    /// Tool calls made during the turn.
    pub tool_calls: Vec<ToolCallRecord>,
    /// Duration of the agent turn in microseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_us: Option<u64>,
    /// Token usage for the turn (if available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsageOutput>,
}

/// Token usage in one-shot output.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenUsageOutput {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A record of a single tool call during a one-shot run.
#[derive(Debug, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// The tool name.
    pub tool_name: String,
    /// The tool use ID.
    pub tool_use_id: String,
}

pub struct RunOneshotOptions<'a> {
    pub session_id: Option<&'a str>,
    pub resume_checkpoint: Option<&'a str>,
    pub json_output: bool,
    pub skills: &'a [String],
    pub auto_approve: bool,
    pub model_profile: Option<&'a str>,
}

pub async fn fetch_available_skills(client: &mut IpcClient) -> anyhow::Result<Vec<String>> {
    let result = client.call(methods::LIST_SKILLS, None).await?;
    let value = result.map_err(|message| anyhow::anyhow!(message))?;
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|skill| skill.get("name").and_then(|name| name.as_str()))
        .map(ToString::to_string)
        .collect())
}

///
/// If `session_id` is `None`, creates a new session. If `json_output` is true,
/// prints structured JSON to stdout. Otherwise prints the text response to stdout
/// and the session ID to stderr.
pub async fn run_oneshot(
    socket_path: &Path,
    message: &str,
    options: RunOneshotOptions<'_>,
) -> anyhow::Result<()> {
    let RunOneshotOptions {
        session_id,
        resume_checkpoint,
        json_output,
        skills,
        auto_approve,
        model_profile,
    } = options;
    let (mut client, _daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;

    let resumed = resolve_resume_target(&mut client, resume_checkpoint).await?;

    // Create or reuse session.
    let session_id = match (session_id, resumed) {
        (Some(sid), _) => sid.to_string(),
        (None, Some(target)) => target.session_id,
        (None, None) => {
            let mut session_params = serde_json::json!({});
            if !skills.is_empty() {
                session_params["skills"] = serde_json::json!(skills);
            }
            if let Some(model_profile) = model_profile {
                session_params["model_profile"] = serde_json::json!(model_profile);
            }
            session_params["prompt_behavior"] = serde_json::json!(if auto_approve {
                PermissionPromptBehavior::Interactive
            } else {
                PermissionPromptBehavior::Headless
            });
            let params = if session_params.as_object().unwrap().is_empty() {
                None
            } else {
                Some(session_params)
            };
            let result = client.call(methods::CREATE_SESSION, params).await?;
            match result {
                Ok(value) => {
                    let sid = value
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| {
                            value
                                .get("session_id")
                                .and_then(|v| v.as_str())
                                .map(str::to_string)
                        })
                        .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?;
                    eprintln!("session: {sid}");
                    sid
                }
                Err(e) => anyhow::bail!("failed to create session: {e}"),
            }
        }
    };

    // Send message.
    let params = serde_json::json!({
        "session_id": session_id,
        "content": message,
    });
    let result = client.call(methods::SEND_MESSAGE, Some(params)).await?;
    if let Err(e) = result {
        anyhow::bail!("failed to send message: {e}");
    }

    // Collect response notifications until TurnComplete.
    // `completed_text` accumulates authoritative TextComplete events (one per LLM call).
    // `delta_buffer` accumulates streaming deltas for real-time display.
    // TextComplete is the source of truth for the final response.
    let mut completed_text = String::new();
    let mut delta_buffer = String::new();
    let mut tool_calls = Vec::new();
    let mut turn_duration_us: Option<u64> = None;
    let mut turn_usage: Option<TokenUsageOutput> = None;

    loop {
        match client.recv_notification().await {
            Some(notif) => match notif.method.as_str() {
                notifications::STREAM_DELTA => {
                    if let Some(params) = &notif.params {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            delta_buffer.push_str(delta);
                        }
                    }
                }
                notifications::TEXT_COMPLETE => {
                    // Append non-empty text from each LLM call. Multiple TextComplete
                    // events fire in multi-tool turns. Clear delta_buffer since
                    // TextComplete is authoritative for its LLM call.
                    if let Some(params) = &notif.params {
                        if let Some(full_text) = params.get("full_text").and_then(|v| v.as_str()) {
                            if !full_text.trim().is_empty() {
                                if !completed_text.is_empty() {
                                    completed_text.push('\n');
                                }
                                completed_text.push_str(full_text);
                            }
                        }
                    }
                    delta_buffer.clear();
                }
                notifications::TOOL_REQUEST => {
                    if let Some(params) = &notif.params {
                        let tool_name = params
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let tool_use_id = params
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        tool_calls.push(ToolCallRecord {
                            tool_name,
                            tool_use_id,
                        });
                    }
                }
                notifications::INTERACTION_NEEDED => {
                    if maybe_auto_approve(&mut client, &session_id, &notif, auto_approve).await? {
                        continue;
                    }

                    // Output the interaction prompt so the caller can respond
                    // via `quine respond --session <id> "answer"`.
                    let prompt = interaction_prompt(&notif);
                    let source_label = source_label(&notif);

                    let partial = if !completed_text.is_empty() {
                        &completed_text
                    } else {
                        &delta_buffer
                    };

                    if json_output {
                        let mut output = serde_json::json!({
                            "session_id": session_id,
                            "interaction_needed": true,
                            "prompt": prompt,
                            "response": partial,
                            "tool_calls": tool_calls,
                            "resume_command": format!(
                                "quine respond --session {} --socket {} \"<response>\"",
                                session_id,
                                socket_path.display()
                            ),
                        });
                        if let Some(label) = source_label {
                            output["source_label"] = serde_json::Value::String(label.to_string());
                        }
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    } else if let Some(label) = source_label {
                        eprintln!("interaction needed [{label}]: {prompt}");
                        eprintln!(
                            "Resume with: `quine respond --session {} --socket {} \"<response>\"`",
                            session_id,
                            socket_path.display()
                        );
                        // Print any partial response accumulated so far.
                        if !partial.is_empty() {
                            println!("{partial}");
                        }
                    } else {
                        eprintln!("interaction needed: {prompt}");
                        eprintln!(
                            "Resume with: `quine respond --session {} --socket {} \"<response>\"`",
                            session_id,
                            socket_path.display()
                        );
                        // Print any partial response accumulated so far.
                        if !partial.is_empty() {
                            println!("{partial}");
                        }
                    }
                    // Exit so the caller can use `quine respond` to continue.
                    return Ok(());
                }
                notifications::SESSION_ERROR => {
                    if let Some(params) = &notif.params {
                        if let Some(error) = params.get("error").and_then(|v| v.as_str()) {
                            anyhow::bail!("session error: {error}");
                        }
                    }
                }
                notifications::TURN_COMPLETE => {
                    if let Some(params) = &notif.params {
                        turn_duration_us = params.get("duration_us").and_then(|v| v.as_u64());
                        turn_usage = params.get("usage").and_then(|v| {
                            Some(TokenUsageOutput {
                                input_tokens: v.get("input_tokens")?.as_u64()?,
                                output_tokens: v.get("output_tokens")?.as_u64()?,
                            })
                        });
                    }
                    break;
                }
                _ => {}
            },
            None => {
                anyhow::bail!("connection to daemon lost");
            }
        }
    }

    // Use completed_text (from TextComplete) if available, fall back to deltas.
    let full_response = if !completed_text.is_empty() {
        completed_text
    } else {
        delta_buffer
    };

    // Output results.
    if json_output {
        let output = OneshotOutput {
            session_id,
            response: full_response,
            tool_calls,
            duration_us: turn_duration_us,
            usage: turn_usage,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{full_response}");
        print_resume_command(socket_path, &session_id);
    }

    Ok(())
}

/// Submit an interaction response to a session and collect the remaining turn output.
///
/// Used after `quine run` exits with an `interaction_needed` status.
/// Sends the response via `submit_interaction_response`, then waits for TurnComplete.
pub async fn run_respond(
    socket_path: &Path,
    session_id: &str,
    response: &str,
    json_output: bool,
) -> anyhow::Result<()> {
    let (mut client, _daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;

    // Submit the interaction response.
    let params = serde_json::json!({
        "session_id": session_id,
        "response": response,
    });
    let result = client
        .call(methods::SUBMIT_INTERACTION_RESPONSE, Some(params))
        .await?;
    if let Err(e) = result {
        anyhow::bail!("failed to submit response: {e}");
    }

    // Collect remaining notifications until TurnComplete.
    let mut completed_text = String::new();
    let mut delta_buffer = String::new();
    let mut tool_calls = Vec::new();
    let mut turn_duration_us: Option<u64> = None;
    let mut turn_usage: Option<TokenUsageOutput> = None;

    loop {
        match client.recv_notification().await {
            Some(notif) => match notif.method.as_str() {
                notifications::STREAM_DELTA => {
                    if let Some(params) = &notif.params {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            delta_buffer.push_str(delta);
                        }
                    }
                }
                notifications::TEXT_COMPLETE => {
                    if let Some(params) = &notif.params {
                        if let Some(full_text) = params.get("full_text").and_then(|v| v.as_str()) {
                            if !full_text.trim().is_empty() {
                                if !completed_text.is_empty() {
                                    completed_text.push('\n');
                                }
                                completed_text.push_str(full_text);
                            }
                        }
                    }
                    delta_buffer.clear();
                }
                notifications::TOOL_REQUEST => {
                    if let Some(params) = &notif.params {
                        let tool_name = params
                            .get("tool_name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let tool_use_id = params
                            .get("tool_use_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        tool_calls.push(ToolCallRecord {
                            tool_name,
                            tool_use_id,
                        });
                    }
                }
                notifications::INTERACTION_NEEDED => {
                    // Another interaction needed — output and exit for caller to respond again.
                    let prompt = interaction_prompt(&notif);
                    let source_label = source_label(&notif);

                    let partial = if !completed_text.is_empty() {
                        &completed_text
                    } else {
                        &delta_buffer
                    };

                    if json_output {
                        let mut output = serde_json::json!({
                            "session_id": session_id,
                            "interaction_needed": true,
                            "prompt": prompt,
                            "response": partial,
                            "tool_calls": tool_calls,
                        });
                        if let Some(label) = source_label {
                            output["source_label"] = serde_json::Value::String(label.to_string());
                        }
                        println!("{}", serde_json::to_string_pretty(&output)?);
                    } else if let Some(label) = source_label {
                        eprintln!("interaction needed [{label}]: {prompt}");
                        if !partial.is_empty() {
                            println!("{partial}");
                        }
                    } else {
                        eprintln!("interaction needed: {prompt}");
                        if !partial.is_empty() {
                            println!("{partial}");
                        }
                    }
                    return Ok(());
                }
                notifications::SESSION_ERROR => {
                    if let Some(params) = &notif.params {
                        if let Some(error) = params.get("error").and_then(|v| v.as_str()) {
                            anyhow::bail!("session error: {error}");
                        }
                    }
                }
                notifications::TURN_COMPLETE => {
                    if let Some(params) = &notif.params {
                        turn_duration_us = params.get("duration_us").and_then(|v| v.as_u64());
                        turn_usage = params.get("usage").and_then(|v| {
                            Some(TokenUsageOutput {
                                input_tokens: v.get("input_tokens")?.as_u64()?,
                                output_tokens: v.get("output_tokens")?.as_u64()?,
                            })
                        });
                    }
                    break;
                }
                _ => {}
            },
            None => {
                anyhow::bail!("connection to daemon lost");
            }
        }
    }

    let full_response = if !completed_text.is_empty() {
        completed_text
    } else {
        delta_buffer
    };

    if json_output {
        let output = OneshotOutput {
            session_id: session_id.to_string(),
            response: full_response,
            tool_calls,
            duration_us: turn_duration_us,
            usage: turn_usage,
        };
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{full_response}");
    }

    Ok(())
}

/// List all available skills.
pub async fn run_skills_list(socket_path: &Path, json_output: bool) -> anyhow::Result<()> {
    let (mut client, _daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;

    let result = client.call(methods::LIST_SKILLS, None).await?;
    match result {
        Ok(value) => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&value)?);
            } else {
                let skills = value.as_array().cloned().unwrap_or_default();
                if skills.is_empty() {
                    println!("No skills found.");
                } else {
                    println!("{:<20}| {:<10}| Description", "Name", "Version");
                    println!("{:-<20}|{:-<10}|{:-<40}", "", "", "");
                    for skill in &skills {
                        let name = skill.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let version = skill.get("version").and_then(|v| v.as_str()).unwrap_or("?");
                        let desc = skill
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        println!("{:<20}| {:<9}| {}", name, version, desc);
                    }
                }
            }
        }
        Err(e) => anyhow::bail!("failed to list skills: {e}"),
    }

    Ok(())
}

/// Show details of a specific skill.
pub async fn run_skills_show(socket_path: &Path, name: &str) -> anyhow::Result<()> {
    let (mut client, _daemon_spawned) = IpcClient::connect_or_launch(socket_path).await?;

    let params = serde_json::json!({ "name": name });
    let result = client.call(methods::GET_SKILL, Some(params)).await?;
    match result {
        Ok(value) => {
            let skill_name = value.get("name").and_then(|v| v.as_str()).unwrap_or("?");
            let version = value.get("version").and_then(|v| v.as_str()).unwrap_or("?");
            let description = value
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let source_path = value
                .get("source_path")
                .and_then(|v| v.as_str())
                .unwrap_or("?");

            println!("Skill: {skill_name} (v{version})");
            println!("Description: {description}");
            println!("Source: {source_path}");

            if let Some(prompt) = value.get("system_prompt").and_then(|v| v.as_str()) {
                println!("\nSystem Prompt:");
                for line in prompt.lines() {
                    println!("  {line}");
                }
            }

            if let Some(tools) = value.get("tools").and_then(|v| v.as_array()) {
                if !tools.is_empty() {
                    println!("\nTools ({}):", tools.len());
                    for tool in tools {
                        let tool_name = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                        let tool_desc = tool
                            .get("description")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let handler = tool.get("handler").and_then(|v| v.as_str()).unwrap_or("?");

                        println!("  - {tool_name}: {tool_desc}");

                        // Show parameter names.
                        if let Some(params) = tool.get("parameters") {
                            if let Some(props) = params.get("properties") {
                                if let Some(obj) = props.as_object() {
                                    let required: Vec<&str> = params
                                        .get("required")
                                        .and_then(|v| v.as_array())
                                        .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect())
                                        .unwrap_or_default();

                                    let param_strs: Vec<String> = obj
                                        .iter()
                                        .map(|(k, v)| {
                                            let typ = v
                                                .get("type")
                                                .and_then(|t| t.as_str())
                                                .unwrap_or("any");
                                            let req = if required.contains(&k.as_str()) {
                                                "required"
                                            } else {
                                                "optional"
                                            };
                                            format!("{k} ({typ}, {req})")
                                        })
                                        .collect();
                                    println!("    Parameters: {}", param_strs.join(", "));
                                }
                            }
                        }
                        println!("    Handler: {handler}");
                    }
                }
            }
        }
        Err(e) => anyhow::bail!("skill not found: {e}"),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_resume_command_includes_session_flag() {
        let session_id = "session-123";
        let socket = std::path::Path::new("/tmp/quine.sock");
        let command = format!(
            "quine run --session {} --socket {} \"<message>\"",
            session_id,
            socket.display()
        );
        assert!(command.contains("--session session-123"));
        assert!(command.contains("--socket /tmp/quine.sock"));
    }

    #[test]
    fn oneshot_output_serialization() {
        let output = OneshotOutput {
            session_id: "test-123".to_string(),
            response: "Hello!".to_string(),
            tool_calls: vec![ToolCallRecord {
                tool_name: "bash".to_string(),
                tool_use_id: "call_1".to_string(),
            }],
            duration_us: Some(1500),
            usage: Some(TokenUsageOutput {
                input_tokens: 100,
                output_tokens: 50,
            }),
        };

        let json = serde_json::to_string(&output).unwrap();
        let parsed: OneshotOutput = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.session_id, "test-123");
        assert_eq!(parsed.response, "Hello!");
        assert_eq!(parsed.tool_calls.len(), 1);
        assert_eq!(parsed.tool_calls[0].tool_name, "bash");
    }

    #[test]
    fn tool_call_record_serialization() {
        let record = ToolCallRecord {
            tool_name: "read".to_string(),
            tool_use_id: "id_1".to_string(),
        };
        let json = serde_json::to_string(&record).unwrap();
        assert!(json.contains("read"));
        assert!(json.contains("id_1"));
    }
}
