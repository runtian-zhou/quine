use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    Default,
    AcceptEdits,
    Plan,
    Bypass,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
    Defer,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleEffect {
    Allow,
    Deny,
    Ask,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleSource {
    BuiltIn,
    Session,
    User,
    Workspace,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionScope {
    Session,
    Workspace,
    Global,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PermissionTarget {
    Any,
    Tool { name: String },
    Path { path: PathBuf },
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRule {
    pub effect: PermissionRuleEffect,
    pub scope: PermissionScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_scope: Option<crate::permission::request::PermissionScope>,
    pub target: PermissionTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<PathBuf>,
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRuleSet {
    pub built_in: Vec<PermissionRule>,
    pub session: Vec<PermissionRule>,
    pub user: Vec<PermissionRule>,
    pub workspace: Vec<PermissionRule>,
}

impl PermissionRuleSet {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rules_for_source_mut(
        &mut self,
        source: PermissionRuleSource,
    ) -> &mut Vec<PermissionRule> {
        match source {
            PermissionRuleSource::BuiltIn => &mut self.built_in,
            PermissionRuleSource::Session => &mut self.session,
            PermissionRuleSource::User => &mut self.user,
            PermissionRuleSource::Workspace => &mut self.workspace,
        }
    }

    #[allow(dead_code)]
    pub(crate) fn rules_for_source(&self, source: PermissionRuleSource) -> &[PermissionRule] {
        match source {
            PermissionRuleSource::BuiltIn => &self.built_in,
            PermissionRuleSource::Session => &self.session,
            PermissionRuleSource::User => &self.user,
            PermissionRuleSource::Workspace => &self.workspace,
        }
    }
}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPromptBehavior {
    #[default]
    Interactive,
    Headless,
    Background,
}

impl PermissionPromptBehavior {
    pub fn is_interactive(self) -> bool {
        matches!(self, Self::Interactive)
    }

    pub fn denial_label(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Headless => "headless",
            Self::Background => "background",
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApprovalRequestId(pub Uuid);
