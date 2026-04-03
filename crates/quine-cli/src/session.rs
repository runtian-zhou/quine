use crate::client::IpcClient;
use quine_harness::protocol::methods;
use quine_llm::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeTarget {
    pub(crate) session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedSession {
    pub(crate) session_id: String,
    pub(crate) max_context_window: Option<u64>,
}

fn build_session_params(
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
    initial_messages: &[Message],
    system_prompt: Option<String>,
) -> Option<serde_json::Value> {
    let mut session_params = serde_json::json!({});
    if !skills.is_empty() {
        session_params["skills"] = serde_json::json!(skills);
    }
    if let Some(system_prompt) = system_prompt {
        session_params["system_prompt"] = serde_json::json!(system_prompt);
    }
    if plan_mode {
        session_params["plan_mode"] = serde_json::json!(true);
    }
    if auto_approve_permissions {
        session_params["auto_approve_permissions"] = serde_json::json!(true);
    }
    if !initial_messages.is_empty() {
        session_params["initial_messages"] =
            serde_json::to_value(initial_messages).expect("initial messages should serialize");
    }

    (!session_params.as_object().unwrap().is_empty()).then_some(session_params)
}

pub(crate) fn build_create_session_params(
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
    initial_messages: &[Message],
) -> Option<serde_json::Value> {
    build_session_params(
        skills,
        plan_mode,
        auto_approve_permissions,
        initial_messages,
        None,
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
        })),
        None => anyhow::bail!("checkpoint not found: {checkpoint}"),
    }
}

pub(crate) async fn create_session(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<CreatedSession> {
    create_session_with_initial_messages(client, skills, plan_mode, auto_approve_permissions, &[])
        .await
}

pub(crate) async fn create_slash_skill_session(
    client: &mut IpcClient,
    skill_name: &str,
    request: &str,
    auto_approve_permissions: bool,
) -> anyhow::Result<CreatedSession> {
    let skills = [skill_name.to_string()];
    let result = client
        .call(
            methods::CREATE_SESSION,
            build_session_params(
                &skills,
                false,
                auto_approve_permissions,
                &[],
                slash_skill_arguments_overlay(request),
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
    auto_approve_permissions: bool,
    initial_messages: &[Message],
) -> anyhow::Result<CreatedSession> {
    let result = client
        .call(
            methods::CREATE_SESSION,
            build_create_session_params(
                skills,
                plan_mode,
                auto_approve_permissions,
                initial_messages,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_create_session_params_includes_plan_mode() {
        let params =
            build_create_session_params(&["feature-request".into()], true, true, &[]).unwrap();

        assert_eq!(
            params,
            serde_json::json!({
                "skills": ["feature-request"],
                "plan_mode": true,
                "auto_approve_permissions": true
            })
        );
    }

    #[test]
    fn build_create_session_params_omits_empty_fields() {
        assert_eq!(build_create_session_params(&[], false, false, &[]), None);
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
    fn slash_skill_arguments_overlay_mentions_arguments_placeholder() {
        let overlay = slash_skill_arguments_overlay("implement a new feature").unwrap();
        assert!(overlay.contains("$ARGUMENTS"));
        assert!(overlay.contains("implement a new feature"));
    }
}
