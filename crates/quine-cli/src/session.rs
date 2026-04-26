use crate::client::IpcClient;
use quine_harness::protocol::methods;
use quine_harness::PermissionPromptBehavior;
use quine_llm::Message;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeTarget {
    pub(crate) session_id: String,
    pub(crate) plan_mode: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedSession {
    pub(crate) session_id: String,
    pub(crate) max_context_window: Option<u64>,
}

#[derive(Clone, Copy)]
struct SessionCreateOptions<'a> {
    working_directory: Option<&'a Path>,
    model_profile: Option<&'a str>,
    session_group: Option<&'a str>,
}

fn build_session_params(
    skills: &[String],
    plan_mode: bool,
    prompt_behavior: PermissionPromptBehavior,
    initial_messages: &[Message],
    system_prompt: Option<String>,
    options: SessionCreateOptions<'_>,
) -> Option<serde_json::Value> {
    let mut session_params = serde_json::json!({});
    if !skills.is_empty() {
        session_params["skills"] = serde_json::json!(skills);
    }
    if let Some(system_prompt) = system_prompt {
        session_params["system_prompt"] = serde_json::json!(system_prompt);
    }
    if let Some(working_directory) = options.working_directory {
        session_params["working_directory"] =
            serde_json::json!(working_directory.to_string_lossy().to_string());
    }
    if plan_mode {
        session_params["plan_mode"] = serde_json::json!(true);
    }
    if prompt_behavior != PermissionPromptBehavior::Interactive {
        session_params["prompt_behavior"] =
            serde_json::to_value(prompt_behavior).expect("prompt behavior should serialize");
    }
    if !initial_messages.is_empty() {
        session_params["initial_messages"] =
            serde_json::to_value(initial_messages).expect("initial messages should serialize");
    }
    if let Some(model_profile) = options.model_profile {
        session_params["model_profile"] = serde_json::json!(model_profile);
    }
    if let Some(session_group) = options.session_group {
        session_params["session_group"] = serde_json::json!(session_group);
    }

    (!session_params.as_object().unwrap().is_empty()).then_some(session_params)
}

pub(crate) fn build_create_session_params(
    skills: &[String],
    plan_mode: bool,
    prompt_behavior: PermissionPromptBehavior,
    initial_messages: &[Message],
    working_directory: Option<&Path>,
    model_profile: Option<&str>,
    session_group: Option<&str>,
) -> Option<serde_json::Value> {
    build_session_params(
        skills,
        plan_mode,
        prompt_behavior,
        initial_messages,
        None,
        SessionCreateOptions {
            working_directory,
            model_profile,
            session_group,
        },
    )
}

fn slash_skill_arguments_overlay(request: &str) -> Option<String> {
    let trimmed = request.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(format!(
        "This session was started from a slash skill command.\n\
         If the loaded skill refers to `$ARGUMENTS`, `the argument`, or `the user's feature description`, \
         treat that as the following text:\n\n{trimmed}"
    ))
}

pub(crate) async fn resolve_resume_target(
    client: &mut IpcClient,
    checkpoint: Option<&str>,
) -> anyhow::Result<Option<ResumeTarget>> {
    let Some(checkpoint) = checkpoint else {
        return Ok(None);
    };

    let result = client.call(methods::LIST_SESSIONS, None).await?;
    let value = result.map_err(|message| anyhow::anyhow!(message))?;
    let sessions = value.as_array().cloned().unwrap_or_default();

    let target = if checkpoint == "latest" {
        sessions
            .iter()
            .max_by_key(|session| {
                session
                    .get("first_event")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .cloned()
    } else {
        sessions
            .iter()
            .find(|session| {
                session.get("session_id").and_then(|value| value.as_str()) == Some(checkpoint)
            })
            .cloned()
    };

    match target {
        Some(session) => Ok(Some(ResumeTarget {
            session_id: session
                .get("session_id")
                .and_then(|value| value.as_str())
                .ok_or_else(|| anyhow::anyhow!("checkpoint session missing session_id"))?
                .to_string(),
            plan_mode: session
                .get("plan_mode")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        })),
        None => anyhow::bail!("checkpoint not found: {checkpoint}"),
    }
}

pub(crate) async fn create_session(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    model_profile: Option<&str>,
    session_group: Option<&str>,
) -> anyhow::Result<CreatedSession> {
    create_session_with_initial_messages(
        client,
        skills,
        plan_mode,
        PermissionPromptBehavior::Interactive,
        &[],
        model_profile,
        session_group,
    )
    .await
}

pub(crate) async fn create_slash_skill_session(
    client: &mut IpcClient,
    skill_name: &str,
    request: &str,
) -> anyhow::Result<CreatedSession> {
    let skills = [skill_name.to_string()];
    let working_directory = std::env::current_dir().ok();
    let result = client
        .call(
            methods::CREATE_SESSION,
            build_session_params(
                &skills,
                false,
                PermissionPromptBehavior::Interactive,
                &[],
                slash_skill_arguments_overlay(request),
                SessionCreateOptions {
                    working_directory: working_directory.as_deref(),
                    model_profile: None,
                    session_group: None,
                },
            ),
        )
        .await?;

    match result {
        Ok(value) => {
            if let Some(session_id) = value.as_str() {
                Ok(CreatedSession {
                    session_id: session_id.to_string(),
                    max_context_window: None,
                })
            } else {
                let session_id = value
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?
                    .to_string();
                let max_context_window = value.get("max_context_window").and_then(|v| v.as_u64());
                Ok(CreatedSession {
                    session_id,
                    max_context_window,
                })
            }
        }
        Err(e) => anyhow::bail!("failed to create session: {e}"),
    }
}

pub(crate) async fn create_session_with_initial_messages(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    prompt_behavior: PermissionPromptBehavior,
    initial_messages: &[Message],
    model_profile: Option<&str>,
    session_group: Option<&str>,
) -> anyhow::Result<CreatedSession> {
    let working_directory = std::env::current_dir().ok();
    let result = client
        .call(
            methods::CREATE_SESSION,
            build_create_session_params(
                skills,
                plan_mode,
                prompt_behavior,
                initial_messages,
                working_directory.as_deref(),
                model_profile,
                session_group,
            ),
        )
        .await?;

    match result {
        Ok(value) => {
            if let Some(session_id) = value.as_str() {
                Ok(CreatedSession {
                    session_id: session_id.to_string(),
                    max_context_window: None,
                })
            } else {
                let session_id = value
                    .get("session_id")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| anyhow::anyhow!("expected string session_id"))?
                    .to_string();
                let max_context_window = value.get("max_context_window").and_then(|v| v.as_u64());
                Ok(CreatedSession {
                    session_id,
                    max_context_window,
                })
            }
        }
        Err(e) => anyhow::bail!("failed to create session: {e}"),
    }
}

pub(crate) async fn exit_plan_mode(client: &mut IpcClient, session_id: &str) -> anyhow::Result<()> {
    let result = client
        .call(
            methods::EXIT_PLAN_MODE,
            Some(serde_json::json!({
                "session_id": session_id,
            })),
        )
        .await?;

    match result {
        Ok(_) => Ok(()),
        Err(e) => anyhow::bail!("failed to exit plan mode: {e}"),
    }
}

pub(crate) async fn set_session_model_profile(
    client: &mut IpcClient,
    session_id: &str,
    model_profile: &str,
) -> anyhow::Result<()> {
    let result = client
        .call(
            methods::SET_SESSION_MODEL_PROFILE,
            Some(serde_json::json!({
                "session_id": session_id,
                "model_profile": model_profile,
            })),
        )
        .await?;
    match result {
        Ok(_) => Ok(()),
        Err(error) => anyhow::bail!("failed to update session model profile: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn build_create_session_params_includes_plan_mode() {
        let params = build_create_session_params(
            &["feature-request".into()],
            true,
            PermissionPromptBehavior::Interactive,
            &[],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            params,
            serde_json::json!({
                "skills": ["feature-request"],
                "plan_mode": true
            })
        );
    }

    #[test]
    fn build_create_session_params_omits_empty_fields() {
        assert_eq!(
            build_create_session_params(
                &[],
                false,
                PermissionPromptBehavior::Interactive,
                &[],
                None,
                None,
                None,
            ),
            None
        );
    }

    #[test]
    fn build_create_session_params_includes_noninteractive_prompt_behavior() {
        let params = build_create_session_params(
            &[],
            false,
            PermissionPromptBehavior::Headless,
            &[],
            None,
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            params,
            serde_json::json!({
                "prompt_behavior": "headless"
            })
        );
    }

    #[test]
    fn build_create_session_params_includes_model_profile() {
        let params = build_create_session_params(
            &[],
            false,
            PermissionPromptBehavior::Interactive,
            &[],
            None,
            Some("claude-sonnet"),
            None,
        )
        .unwrap();

        assert_eq!(
            params,
            serde_json::json!({
                "model_profile": "claude-sonnet"
            })
        );
    }

    #[test]
    fn build_create_session_params_includes_working_directory() {
        let params = build_create_session_params(
            &[],
            false,
            PermissionPromptBehavior::Interactive,
            &[],
            Some(Path::new("/tmp/project")),
            None,
            None,
        )
        .unwrap();

        assert_eq!(
            params,
            serde_json::json!({
                "working_directory": "/tmp/project"
            })
        );
    }

    #[test]
    fn slash_skill_arguments_overlay_is_omitted_for_empty_request() {
        assert_eq!(slash_skill_arguments_overlay("   "), None);
    }

    #[test]
    fn resolve_resume_target_picks_latest() {
        let sessions = serde_json::json!([
            {"session_id": "older", "first_event": "2024-01-01T00:00:00Z"},
            {"session_id": "newer", "first_event": "2024-01-02T00:00:00Z"}
        ]);
        let picked = sessions
            .as_array()
            .unwrap()
            .iter()
            .max_by_key(|session| {
                session
                    .get("first_event")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .unwrap();
        assert_eq!(
            picked.get("session_id").and_then(|v| v.as_str()),
            Some("newer")
        );
    }

    #[test]
    fn resolve_resume_target_extracts_plan_mode() {
        let sessions = serde_json::json!([
            {"session_id": "normal", "first_event": "2024-01-01T00:00:00Z", "plan_mode": false},
            {"session_id": "planner", "first_event": "2024-01-02T00:00:00Z", "plan_mode": true}
        ]);
        let picked = sessions
            .as_array()
            .unwrap()
            .iter()
            .max_by_key(|session| {
                session
                    .get("first_event")
                    .and_then(|value| value.as_str())
                    .map(str::to_string)
            })
            .unwrap();
        let target = ResumeTarget {
            session_id: picked
                .get("session_id")
                .and_then(|value| value.as_str())
                .unwrap()
                .to_string(),
            plan_mode: picked
                .get("plan_mode")
                .and_then(|value| value.as_bool())
                .unwrap_or(false),
        };
        assert_eq!(
            target,
            ResumeTarget {
                session_id: "planner".into(),
                plan_mode: true
            }
        );
    }

    #[test]
    fn slash_skill_arguments_overlay_mentions_arguments_placeholder() {
        let overlay = slash_skill_arguments_overlay("implement a new feature").unwrap();
        assert!(overlay.contains("$ARGUMENTS"));
        assert!(overlay.contains("implement a new feature"));
    }
}
