use super::outcome::PermissionOutcome;
use super::types::ApprovalRequestId;
use crate::tool::{InteractionKind, InteractionRequest, InteractionResponse, SelectOption};

pub(crate) const APPROVE_ONCE_LABEL: &str = "approve once";
pub(crate) const DENY_ONCE_LABEL: &str = "deny once";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingPermissionApproval {
    pub request_id: ApprovalRequestId,
    pub tool_name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PermissionApprovalChoice {
    ApproveOnce,
    DenyOnce,
}

pub(crate) fn build_permission_approval_request(
    outcome: &PermissionOutcome,
) -> (PendingPermissionApproval, InteractionRequest) {
    let pending = PendingPermissionApproval {
        request_id: ApprovalRequestId(uuid::Uuid::new_v4()),
        tool_name: outcome.request.tool_name.clone(),
        reason: outcome.reason.clone(),
    };
    let request = InteractionRequest {
        prompt: format!(
            "Permission approval required for `{}`: {}",
            outcome.request.tool_name, outcome.reason
        ),
        kind: InteractionKind::SingleSelect,
        options: vec![
            SelectOption {
                label: APPROVE_ONCE_LABEL.to_string(),
                description: Some("Run this action once.".into()),
            },
            SelectOption {
                label: DENY_ONCE_LABEL.to_string(),
                description: Some("Do not run this action.".into()),
            },
        ],
        allow_freeform: false,
        source_label: Some(format!("permission:{}", pending.request_id.0)),
    };
    (pending, request)
}

pub(crate) fn parse_permission_approval_response(
    response: &InteractionResponse,
) -> Option<PermissionApprovalChoice> {
    match response.response.trim().to_ascii_lowercase().as_str() {
        APPROVE_ONCE_LABEL => Some(PermissionApprovalChoice::ApproveOnce),
        DENY_ONCE_LABEL => Some(PermissionApprovalChoice::DenyOnce),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::outcome::{PermissionOutcome, PermissionOutcomeKind};
    use crate::permission::request::{
        MatchedPermissionSource, PermissionMatchKind, PermissionRequest, PermissionResource,
        PermissionScope,
    };
    use crate::permission::types::PermissionDecision;

    fn sample_outcome() -> PermissionOutcome {
        PermissionOutcome {
            kind: PermissionOutcomeKind::RequiresApproval,
            final_decision: PermissionDecision::Ask,
            source: MatchedPermissionSource {
                kind: PermissionMatchKind::ModeDefault,
                rule_source: None,
            },
            reason: "permission resolved by Default mode".into(),
            request: PermissionRequest {
                tool_name: "apply_patch".into(),
                action: None,
                scope: PermissionScope::Write,
                resource: PermissionResource::None,
            },
        }
    }

    #[test]
    fn builds_permission_approval_request_with_permission_source_label() {
        let (pending, request) = build_permission_approval_request(&sample_outcome());

        assert_eq!(pending.tool_name, "apply_patch");
        assert_eq!(request.kind, InteractionKind::SingleSelect);
        assert_eq!(request.options.len(), 2);
        assert!(request
            .source_label
            .as_deref()
            .is_some_and(|label| label.starts_with("permission:")));
    }

    #[test]
    fn parses_permission_approval_choices() {
        assert_eq!(
            parse_permission_approval_response(&InteractionResponse {
                response: "approve once".into(),
                selected_indices: vec![0],
            }),
            Some(PermissionApprovalChoice::ApproveOnce)
        );
        assert_eq!(
            parse_permission_approval_response(&InteractionResponse {
                response: "deny once".into(),
                selected_indices: vec![1],
            }),
            Some(PermissionApprovalChoice::DenyOnce)
        );
        assert_eq!(
            parse_permission_approval_response(&InteractionResponse {
                response: "something else".into(),
                selected_indices: Vec::new(),
            }),
            None
        );
    }
}
