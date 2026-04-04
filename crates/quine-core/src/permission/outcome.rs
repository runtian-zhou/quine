use serde::{Deserialize, Serialize};

use super::request::{MatchedPermissionSource, PermissionRequest};
use super::types::PermissionDecision;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionOutcomeKind {
    Allowed,
    Denied,
    RequiresApproval,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PermissionOutcome {
    pub kind: PermissionOutcomeKind,
    pub final_decision: PermissionDecision,
    pub source: MatchedPermissionSource,
    pub reason: String,
    pub request: PermissionRequest,
}

impl PermissionOutcome {
    pub(crate) fn is_allowed(&self) -> bool {
        matches!(self.kind, PermissionOutcomeKind::Allowed)
    }
}
