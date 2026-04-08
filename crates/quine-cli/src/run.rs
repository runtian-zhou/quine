use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::client::IpcClient;
use crate::interaction::{
    allow_freeform as interaction_allow_freeform, kind as interaction_kind,
    maybe_auto_approve, options as interaction_options, prompt as interaction_prompt,
    source_label,
};
use crate::session::resolve_resume_target;
use quine_harness::{
    protocol::{methods, notifications},
    PermissionPromptBehavior,
};

pub(crate) async fn connect_existing_client(socket_path: &Path) -> anyhow::Result<IpcClient> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let mut interval = std::time::Duration::from_millis(50);

    loop {
        match IpcClient::connect(socket_path).await {
            Ok(client) => return Ok(client),
            Err(error) if tokio::time::Instant::now() < deadline => {
                let _ = error;
                tokio::time::sleep(interval).await;
                interval = (interval * 2).min(std::time::Duration::from_millis(500));
            }
            Err(error) => return Err(error),
        }
    }
}

fn print_resume_command(socket_path: &Path, session_id: &str) {
    eprintln!(
        "Resume from this checkpoint with: `quine run --session {} --socket {} \"<message>\"`",
        session_id,
        socket_path.display()
    );
}

/// Structured JSON output for one-shot mode.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Interaction details when the turn pauses for input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interaction_needed: Option<InteractionNeededOutput>,
}

/// Interaction details captured when a turn stops early.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InteractionNeededOutput {
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_label: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub allow_freeform: bool,
    pub response: String,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Token usage in one-shot output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenUsageOutput {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A record of a single tool call during a one-shot run.
#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone)]
pub(crate) enum OneshotProgressEvent {
    Streaming,
    ToolRequested { tool_name: String },
    InteractionNeeded,
    TurnComplete,
}

pub(crate) async fn execute_oneshot(
    socket_path: &Path,
    message: &str,
    options: RunOneshotOptions<'_>,
    allow_daemon_launch: bool,
) -> anyhow::Result<OneshotOutput> {
    execute_oneshot_with_progress(socket_path, message, options, allow_daemon_launch, |_| {}).await
}

pub(crate) async fn execute_oneshot_with_progress<F>(
    socket_path: &Path,
    message: &str,
    options: RunOneshotOptions<'_>,
    allow_daemon_launch: bool,
    mut on_progress: F,
) -> anyhow::Result<OneshotOutput>
where
    F: FnMut(OneshotProgressEvent),
{
    let RunOneshotOptions {
        session_id,
        resume_checkpoint,
        json_output: _,
        skills,
        auto_approve,
        model_profile,
    } = options;
    let (mut client, _daemon_spawned) = if allow_daemon_launch {
        IpcClient::connect_or_launch(socket_path).await?
    } else {
        (connect_existing_client(socket_path).await?, false)
    };

    let resumed = resolve_resume_target(&mut client, resume_checkpoint).await?;

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

    let params = serde_json::json!({
        "session_id": session_id,
        "content": message,
    });
    let result = client.call(methods::SEND_MESSAGE, Some(params)).await?;
    if let Err(e) = result {
        anyhow::bail!("failed to send message: {e}");
    }

    let mut completed_text = String::new();
    let mut delta_buffer = String::new();
    let mut tool_calls = Vec::new();
    let mut turn_duration_us: Option<u64> = None;
    let mut turn_usage: Option<TokenUsageOutput> = None;
    let mut saw_stream_delta = false;

    loop {
        match client.recv_notification().await {
            Some(notif) => match notif.method.as_str() {
                notifications::STREAM_DELTA => {
                    if let Some(params) = &notif.params {
                        if let Some(delta) = params.get("delta").and_then(|v| v.as_str()) {
                            if !saw_stream_delta {
                                saw_stream_delta = true;
                                on_progress(OneshotProgressEvent::Streaming);
                            }
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
                        on_progress(OneshotProgressEvent::ToolRequested {
                            tool_name: tool_name.clone(),
                        });
                        tool_calls.push(ToolCallRecord {
                            tool_name,
                            tool_use_id,
                        });
                    }
                }
                notifications::INTERACTION_NEEDED => {
                    on_progress(OneshotProgressEvent::InteractionNeeded);
                    if maybe_auto_approve(&mut client, &session_id, &notif, auto_approve).await? {
                        continue;
                    }

                    let prompt = interaction_prompt(&notif).to_string();
                    let source_label = source_label(&notif).map(ToString::to_string);
                    let kind = interaction_kind(&notif).to_string();
                    let options = interaction_options(&notif);
                    let allow_freeform = interaction_allow_freeform(&notif);
                    let partial = if !completed_text.is_empty() {
                        completed_text.clone()
                    } else {
                        delta_buffer.clone()
                    };

                    return Ok(OneshotOutput {
                        session_id,
                        response: partial.clone(),
                        tool_calls: tool_calls.clone(),
                        duration_us: turn_duration_us,
                        usage: turn_usage,
                        interaction_needed: Some(InteractionNeededOutput {
                            prompt,
                            source_label,
                            kind,
                            options,
                            allow_freeform,
                            response: partial,
                            tool_calls,
                        }),
                    });
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
                        turn_usage = params.get("usage").and_then(|usage| {
                            Some(TokenUsageOutput {
                                input_tokens: usage.get("input_tokens")?.as_u64()?,
                                output_tokens: usage.get("output_tokens")?.as_u64()?,
                            })
                        });
                    }
                    on_progress(OneshotProgressEvent::TurnComplete);
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

    Ok(OneshotOutput {
        session_id,
        response: full_response,
        tool_calls,
        duration_us: turn_duration_us,
        usage: turn_usage,
        interaction_needed: None,
    })
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
    let json_output = options.json_output;
    let output = execute_oneshot(socket_path, message, options, true).await?;

    if json_output {
        let mut json_output = serde_json::to_value(&output)?;
        if let Some(interaction) = &output.interaction_needed {
            if let Some(object) = json_output.as_object_mut() {
                object.insert(
                    "resume_command".to_string(),
                    serde_json::Value::String(format!(
                        "quine respond --session {} --socket {} \"<response>\"",
                        output.session_id,
                        socket_path.display()
                    )),
                );
                object.insert(
                    "prompt".to_string(),
                    serde_json::Value::String(interaction.prompt.clone()),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&json_output)?);
    } else {
        if let Some(interaction) = output.interaction_needed.clone() {
            if let Some(label) = interaction.source_label.as_deref() {
                eprintln!("interaction needed [{label}]: {}", interaction.prompt);
            } else {
                eprintln!("interaction needed: {}", interaction.prompt);
            }
            eprintln!(
                "Resume with: `quine respond --session {} --socket {} \"<response>\"`",
                output.session_id,
                socket_path.display()
            );
            if !interaction.response.is_empty() {
                println!("{}", interaction.response);
            }
        } else {
            println!("{}", output.response);
            print_resume_command(socket_path, &output.session_id);
        }
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
    let output =
        execute_interaction_response(socket_path, session_id, response, Vec::new(), true).await?;

    if json_output {
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", output.response);
    }

    Ok(())
}

pub(crate) async fn execute_interaction_response(
    socket_path: &Path,
    session_id: &str,
    response: &str,
    selected_indices: Vec<usize>,
    allow_daemon_launch: bool,
) -> anyhow::Result<OneshotOutput> {
    let (mut client, _daemon_spawned) = if allow_daemon_launch {
        IpcClient::connect_or_launch(socket_path).await?
    } else {
        (connect_existing_client(socket_path).await?, false)
    };

    let params = serde_json::json!({
        "session_id": session_id,
        "response": response,
        "selected_indices": selected_indices,
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
                    let prompt = interaction_prompt(&notif).to_string();
                    let source_label = source_label(&notif).map(ToString::to_string);
                    let kind = interaction_kind(&notif).to_string();
                    let options = interaction_options(&notif);
                    let allow_freeform = interaction_allow_freeform(&notif);
                    let partial = if !completed_text.is_empty() {
                        completed_text.clone()
                    } else {
                        delta_buffer.clone()
                    };

                    return Ok(OneshotOutput {
                        session_id: session_id.to_string(),
                        response: partial.clone(),
                        tool_calls: tool_calls.clone(),
                        duration_us: turn_duration_us,
                        usage: turn_usage,
                        interaction_needed: Some(InteractionNeededOutput {
                            prompt,
                            source_label,
                            kind,
                            options,
                            allow_freeform,
                            response: partial,
                            tool_calls,
                        }),
                    });
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
    Ok(OneshotOutput {
        session_id: session_id.to_string(),
        response: full_response,
        tool_calls,
        duration_us: turn_duration_us,
        usage: turn_usage,
        interaction_needed: None,
    })
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
            interaction_needed: None,
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
