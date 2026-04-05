use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::command::CommandDescriptor;
use super::types::{PermissionDecision, PermissionRuleSource};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Read,
    Write,
    Execute,
    ProcessControl,
    AgentControl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionResource {
    None,
    Path { path: PathBuf },
    Command { descriptor: CommandDescriptor },
    Process { target: String },
    Agent { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub tool_name: String,
    pub action: Option<String>,
    pub scope: PermissionScope,
    pub resource: PermissionResource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMatchKind {
    ToolLocal,
    FilesystemBoundary,
    Rule,
    ModeDefault,
    HeadlessFallback,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchedPermissionSource {
    pub kind: PermissionMatchKind,
    pub rule_source: Option<PermissionRuleSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolLocalDecision {
    pub decision: PermissionDecision,
    pub reason: Option<String>,
}
