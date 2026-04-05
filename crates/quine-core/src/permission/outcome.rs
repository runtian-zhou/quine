use serde::{Deserialize, Serialize};

use super::request::{MatchedPermissionSource, PermissionRequest};
use super::types::PermissionDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionOutcomeKind {
    Allowed,
    Denied,
    RequiresApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOutcome {
    pub kind: PermissionOutcomeKind,
    pub final_decision: PermissionDecision,
    pub source: MatchedPermissionSource,
    pub reason: String,
    pub request: PermissionRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matched_rule: Option<super::types::PermissionRule>,
}

impl PermissionOutcome {
    pub(crate) fn is_allowed(&self) -> bool {
        matches!(self.kind, PermissionOutcomeKind::Allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::{
        CommandDescriptor, CommandRisk, MatchedPermissionSource, PermissionDecision,
        PermissionMatchKind, PermissionRequest, PermissionResource, PermissionRule,
        PermissionRuleEffect, PermissionRuleSource, PermissionScope, PermissionTarget, RuleScope,
    };
    use std::path::PathBuf;

    #[test]
    fn permission_outcome_serializes_explanation_fields() {
        let outcome = PermissionOutcome {
            kind: PermissionOutcomeKind::RequiresApproval,
            final_decision: PermissionDecision::Ask,
            source: MatchedPermissionSource {
                kind: PermissionMatchKind::Rule,
                rule_source: Some(PermissionRuleSource::Workspace),
            },
            reason: "permission denied by workspace rule".into(),
            request: PermissionRequest {
                tool_name: "bash".into(),
                action: Some("run".into()),
                scope: PermissionScope::Execute,
                resource: PermissionResource::Command {
                    descriptor: CommandDescriptor {
                        command: "touch flag.txt".into(),
                        program: Some("touch".into()),
                        argv: vec!["flag.txt".into()],
                        risk: CommandRisk::Mutating,
                    },
                },
            },
            matched_rule: Some(PermissionRule {
                effect: PermissionRuleEffect::Ask,
                scope: RuleScope::Workspace,
                request_scope: Some(PermissionScope::Execute),
                target: PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: Some(PathBuf::from("/tmp/project/.quine/permissions.yaml")),
            }),
        };

        let value = serde_json::to_value(&outcome).unwrap();
        assert_eq!(value["source"]["kind"], "rule");
        assert_eq!(value["source"]["rule_source"], "workspace");
        assert_eq!(value["request"]["scope"], "execute");
        assert_eq!(value["request"]["resource"]["kind"], "command");
        assert_eq!(value["matched_rule"]["effect"], "ask");
        assert_eq!(
            value["matched_rule"]["source_path"],
            "/tmp/project/.quine/permissions.yaml"
        );
    }
}
