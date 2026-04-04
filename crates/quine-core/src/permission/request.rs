use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::types::{PermissionDecision, PermissionRuleSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionScope {
    Read,
    Write,
    Execute,
    ProcessControl,
    AgentControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PermissionResource {
    None,
    Path { path: PathBuf },
    Command { command: String },
    Process { target: String },
    Agent { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PermissionRequest {
    pub tool_name: String,
    pub action: Option<String>,
    pub scope: PermissionScope,
    pub resource: PermissionResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PermissionMatchKind {
    ToolLocal,
    Rule,
    ModeDefault,
    HeadlessFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MatchedPermissionSource {
    pub kind: PermissionMatchKind,
    pub rule_source: Option<PermissionRuleSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ToolLocalDecision {
    pub decision: PermissionDecision,
    pub reason: Option<String>,
}
