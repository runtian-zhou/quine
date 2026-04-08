use crate::client::IpcClient;
use quine_harness::protocol::{methods, JsonRpcNotification};

pub(crate) const APPROVE_ONCE_RESPONSE: &str = "approve once";
const PERMISSION_SOURCE_PREFIX: &str = "permission:";

pub(crate) fn prompt(notif: &JsonRpcNotification) -> &str {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("prompt"))
        .and_then(|value| value.as_str())
        .unwrap_or("(interaction requested)")
}

pub(crate) fn options(notif: &JsonRpcNotification) -> Vec<String> {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("options"))
        .and_then(|value| value.as_array())
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.get("label")
                        .and_then(|label| label.as_str())
                        .map(ToString::to_string)
                })
                .collect()
        })
        .unwrap_or_default()
}

pub(crate) fn kind(notif: &JsonRpcNotification) -> &str {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("kind"))
        .and_then(|value| value.as_str())
        .unwrap_or("Question")
}

pub(crate) fn allow_freeform(notif: &JsonRpcNotification) -> bool {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("allow_freeform"))
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn source_label(notif: &JsonRpcNotification) -> Option<&str> {
    notif
        .params
        .as_ref()
        .and_then(|params| params.get("source_label"))
        .and_then(|value| value.as_str())
}

pub(crate) fn is_permission_interaction(notif: &JsonRpcNotification) -> bool {
    source_label(notif).is_some_and(|label| label.starts_with(PERMISSION_SOURCE_PREFIX))
}

pub(crate) async fn maybe_auto_approve(
    client: &mut IpcClient,
    session_id: &str,
    notif: &JsonRpcNotification,
    auto_approve: bool,
) -> anyhow::Result<bool> {
    if !auto_approve || !is_permission_interaction(notif) {
        return Ok(false);
    }

    let params = serde_json::json!({
        "session_id": session_id,
        "response": APPROVE_ONCE_RESPONSE,
    });
    let result = client
        .call(methods::SUBMIT_INTERACTION_RESPONSE, Some(params))
        .await?;
    if let Err(error) = result {
        anyhow::bail!("failed to submit auto-approval response: {error}");
    }

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_interaction(source_label: Option<&str>) -> JsonRpcNotification {
        let mut params = serde_json::json!({
            "prompt": "Permission approval required",
            "kind": "SingleSelect",
            "options": [
                {"label": "approve once"},
                {"label": "deny once"}
            ],
            "allow_freeform": false
        });
        if let Some(label) = source_label {
            params["source_label"] = serde_json::Value::String(label.to_string());
        }
        JsonRpcNotification {
            jsonrpc: "2.0".into(),
            method: "interaction_needed".into(),
            params: Some(params),
        }
    }

    #[test]
    fn permission_interactions_are_detected_from_source_label() {
        assert!(is_permission_interaction(&make_interaction(Some(
            "permission:1234"
        ))));
        assert!(!is_permission_interaction(&make_interaction(Some(
            "subagent:worker"
        ))));
        assert!(!is_permission_interaction(&make_interaction(None)));
    }

    #[test]
    fn interaction_metadata_extractors_read_notification_payload() {
        let notif = make_interaction(Some("permission:1234"));

        assert_eq!(prompt(&notif), "Permission approval required");
        assert_eq!(kind(&notif), "SingleSelect");
        assert!(!allow_freeform(&notif));
        assert_eq!(source_label(&notif), Some("permission:1234"));
        assert_eq!(
            options(&notif),
            vec!["approve once".to_string(), "deny once".to_string()]
        );
    }
}
