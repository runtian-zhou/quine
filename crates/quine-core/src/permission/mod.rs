pub(crate) mod approval;
pub(crate) mod command;
pub(crate) mod context;
pub(crate) mod engine;
pub(crate) mod mode;
pub(crate) mod outcome;
pub(crate) mod path;
pub(crate) mod request;
pub(crate) mod types;

pub use approval::PendingPermissionApproval;
pub(crate) use approval::{
    build_permission_approval_request, parse_permission_approval_response, PermissionApprovalChoice,
};
pub(crate) use command::analyze_command;
pub use command::{CommandDescriptor, CommandRisk};
pub(crate) use context::PermissionContext;
pub use context::PermissionRuntimeSnapshot;
pub(crate) use engine::evaluate_permission;
pub(crate) use mode::exit_plan_mode;
pub use outcome::{PermissionOutcome, PermissionOutcomeKind};
pub use request::{
    MatchedPermissionSource, PermissionMatchKind, PermissionRequest, PermissionResource,
    PermissionScope, ToolLocalDecision,
};
pub use types::{
    ApprovalRequestId, PermissionDecision, PermissionMode, PermissionPromptBehavior,
    PermissionRule, PermissionRuleEffect, PermissionRuleSet, PermissionRuleSource,
    PermissionScope as RuleScope, PermissionTarget,
};
