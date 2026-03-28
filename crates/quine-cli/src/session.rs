use crate::client::IpcClient;
use quine_harness::protocol::methods;
use quine_llm::Message;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedSession {
    pub(crate) session_id: String,
    pub(crate) max_context_window: Option<u64>,
}

pub(crate) fn build_create_session_params(
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
    initial_messages: &[Message],
) -> Option<serde_json::Value> {
    let mut session_params = serde_json::json!({});
    if !skills.is_empty() {
        session_params["skills"] = serde_json::json!(skills);
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

pub(crate) async fn create_session(
    client: &mut IpcClient,
    skills: &[String],
    plan_mode: bool,
    auto_approve_permissions: bool,
) -> anyhow::Result<CreatedSession> {
    create_session_with_initial_messages(client, skills, plan_mode, auto_approve_permissions, &[])
        .await
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
}
