use super::context::PermissionContext;
use super::outcome::{PermissionOutcome, PermissionOutcomeKind};
use super::path::authorize_path;
use super::request::{
    MatchedPermissionSource, PermissionMatchKind, PermissionRequest, PermissionResource,
    PermissionScope, ToolLocalDecision,
};
use super::types::{
    PermissionDecision, PermissionMode, PermissionPromptBehavior, PermissionRule,
    PermissionRuleEffect, PermissionRuleSource, PermissionTarget,
};
use crate::permission::CommandRisk;

pub(crate) fn evaluate_permission(
    context: &PermissionContext,
    request: PermissionRequest,
    tool_local: Option<ToolLocalDecision>,
) -> PermissionOutcome {
    if let Some(local) = tool_local.filter(|decision| decision.decision == PermissionDecision::Deny)
    {
        return outcome_for(
            request,
            PermissionDecision::Deny,
            MatchedPermissionSource {
                kind: PermissionMatchKind::ToolLocal,
                rule_source: None,
            },
            local
                .reason
                .unwrap_or_else(|| "tool-local policy denied request".into()),
            context.prompt_behavior(),
            None,
        );
    }

    if let PermissionResource::Path { path } = &request.resource {
        if let Err(reason) = authorize_path(context, request.scope, path) {
            return outcome_for(
                request,
                PermissionDecision::Deny,
                MatchedPermissionSource {
                    kind: PermissionMatchKind::FilesystemBoundary,
                    rule_source: None,
                },
                reason,
                context.prompt_behavior(),
                None,
            );
        }
    }

    if let Some((source, rule, decision)) = first_matching_rule(context, &request) {
        let reason = match decision {
            PermissionDecision::Allow => format!("permission allowed by {source:?} rule"),
            PermissionDecision::Deny => format!("permission denied by {source:?} rule"),
            PermissionDecision::Ask => format!("permission requires approval by {source:?} rule"),
            PermissionDecision::Defer => "permission deferred unexpectedly".into(),
        };
        return outcome_for(
            request,
            decision,
            MatchedPermissionSource {
                kind: PermissionMatchKind::Rule,
                rule_source: Some(source),
            },
            reason,
            context.prompt_behavior(),
            Some(rule.clone()),
        );
    }

    let mode_decision = mode_default_decision(context.mode(), &request);
    let mode_reason = mode_default_reason(context.mode(), &request);
    outcome_for(
        request,
        mode_decision,
        MatchedPermissionSource {
            kind: PermissionMatchKind::ModeDefault,
            rule_source: None,
        },
        mode_reason,
        context.prompt_behavior(),
        None,
    )
}

fn outcome_for(
    request: PermissionRequest,
    decision: PermissionDecision,
    source: MatchedPermissionSource,
    reason: String,
    prompt_behavior: PermissionPromptBehavior,
    matched_rule: Option<PermissionRule>,
) -> PermissionOutcome {
    if decision == PermissionDecision::Ask && !prompt_behavior.is_interactive() {
        return PermissionOutcome {
            kind: PermissionOutcomeKind::Denied,
            final_decision: PermissionDecision::Deny,
            source: MatchedPermissionSource {
                kind: PermissionMatchKind::HeadlessFallback,
                rule_source: source.rule_source,
            },
            reason: format!(
                "{reason}; {} sessions cannot satisfy approval prompts",
                prompt_behavior.denial_label()
            ),
            request,
            matched_rule,
        };
    }

    let kind = match decision {
        PermissionDecision::Allow => PermissionOutcomeKind::Allowed,
        PermissionDecision::Deny => PermissionOutcomeKind::Denied,
        PermissionDecision::Ask => PermissionOutcomeKind::RequiresApproval,
        PermissionDecision::Defer => PermissionOutcomeKind::Denied,
    };

    PermissionOutcome {
        kind,
        final_decision: decision,
        source,
        reason,
        request,
        matched_rule,
    }
}

fn mode_default_decision(mode: PermissionMode, request: &PermissionRequest) -> PermissionDecision {
    if let PermissionResource::Command { descriptor } = &request.resource {
        if request.tool_name == "bash" && request.scope == PermissionScope::Execute {
            return match mode {
                PermissionMode::Bypass => PermissionDecision::Allow,
                PermissionMode::Plan => PermissionDecision::Deny,
                PermissionMode::Default | PermissionMode::AcceptEdits => match descriptor.risk {
                    CommandRisk::ReadOnly => PermissionDecision::Allow,
                    CommandRisk::Mutating
                    | CommandRisk::NestedShell
                    | CommandRisk::Interpreter
                    | CommandRisk::Unknown => PermissionDecision::Ask,
                },
            };
        }
    }

    match mode {
        PermissionMode::Bypass => PermissionDecision::Allow,
        PermissionMode::Plan => match request.scope {
            PermissionScope::Read => PermissionDecision::Allow,
            PermissionScope::Write
            | PermissionScope::Execute
            | PermissionScope::ProcessControl
            | PermissionScope::AgentControl => PermissionDecision::Deny,
        },
        PermissionMode::AcceptEdits => match request.scope {
            PermissionScope::Read | PermissionScope::Write => PermissionDecision::Allow,
            PermissionScope::Execute
            | PermissionScope::ProcessControl
            | PermissionScope::AgentControl => PermissionDecision::Ask,
        },
        PermissionMode::Default => match request.scope {
            PermissionScope::Read => PermissionDecision::Allow,
            PermissionScope::Write
            | PermissionScope::Execute
            | PermissionScope::ProcessControl
            | PermissionScope::AgentControl => PermissionDecision::Ask,
        },
    }
}

fn mode_default_reason(mode: PermissionMode, request: &PermissionRequest) -> String {
    if let PermissionResource::Command { descriptor } = &request.resource {
        if request.tool_name == "bash" && request.scope == PermissionScope::Execute {
            return format!(
                "permission resolved by {:?} mode default for {} bash command",
                mode,
                descriptor.risk.reason_label()
            );
        }
    }

    format!("permission resolved by {:?} mode default", mode)
}

fn first_matching_rule<'a>(
    context: &'a PermissionContext,
    request: &PermissionRequest,
) -> Option<(PermissionRuleSource, &'a PermissionRule, PermissionDecision)> {
    let sources = [
        PermissionRuleSource::Session,
        PermissionRuleSource::Workspace,
        PermissionRuleSource::User,
        PermissionRuleSource::BuiltIn,
    ];
    let effects = [
        PermissionRuleEffect::Deny,
        PermissionRuleEffect::Allow,
        PermissionRuleEffect::Ask,
    ];

    for source in sources {
        for effect in effects {
            if let Some(rule) = context
                .rules()
                .rules_for_source(source)
                .iter()
                .find(|rule| rule.effect == effect && rule_matches(rule, request))
            {
                let decision = match effect {
                    PermissionRuleEffect::Allow => PermissionDecision::Allow,
                    PermissionRuleEffect::Deny => PermissionDecision::Deny,
                    PermissionRuleEffect::Ask => PermissionDecision::Ask,
                };
                return Some((source, rule, decision));
            }
        }
    }

    None
}

fn rule_matches(rule: &PermissionRule, request: &PermissionRequest) -> bool {
    if rule
        .request_scope
        .is_some_and(|scope| scope != request.scope)
    {
        return false;
    }

    match &rule.target {
        PermissionTarget::Any => true,
        PermissionTarget::Tool { name } => name == &request.tool_name,
        PermissionTarget::Path { path } => match &request.resource {
            PermissionResource::Path {
                path: requested_path,
            } => requested_path.starts_with(path),
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::permission::command::{analyze_command, CommandRisk};
    use crate::permission::context::PermissionContext;
    use crate::permission::types::{
        PermissionPromptBehavior, PermissionRule, PermissionRuleEffect, PermissionRuleSource,
        PermissionScope as RuleScope, PermissionTarget,
    };
    use tempfile::TempDir;

    fn request(scope: PermissionScope) -> PermissionRequest {
        PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope,
            resource: PermissionResource::Command {
                descriptor: analyze_command("echo hi"),
            },
        }
    }

    #[test]
    fn tool_local_deny_wins_over_allow_rule() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.add_rule(
            PermissionRuleSource::Session,
            PermissionRule {
                effect: PermissionRuleEffect::Allow,
                scope: RuleScope::Session,
                request_scope: None,
                target: PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: None,
            },
        );

        let outcome = evaluate_permission(
            &context,
            request(PermissionScope::Execute),
            Some(ToolLocalDecision {
                decision: PermissionDecision::Deny,
                reason: Some("dangerous command pattern".into()),
            }),
        );

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(outcome.source.kind, PermissionMatchKind::ToolLocal);
        assert!(outcome.reason.contains("dangerous command pattern"));
    }

    #[test]
    fn explicit_deny_beats_explicit_allow() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let allow_rule = PermissionRule {
            effect: PermissionRuleEffect::Allow,
            scope: RuleScope::Session,
            request_scope: None,
            target: PermissionTarget::Tool {
                name: "bash".into(),
            },
            source_path: None,
        };
        let deny_rule = PermissionRule {
            effect: PermissionRuleEffect::Deny,
            scope: RuleScope::Session,
            request_scope: None,
            target: PermissionTarget::Tool {
                name: "bash".into(),
            },
            source_path: None,
        };
        context.add_rule(PermissionRuleSource::Workspace, allow_rule);
        context.add_rule(PermissionRuleSource::Workspace, deny_rule);

        let outcome = evaluate_permission(&context, request(PermissionScope::Execute), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(
            outcome.source.rule_source,
            Some(PermissionRuleSource::Workspace)
        );
    }

    #[test]
    fn explicit_allow_beats_mode_default() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.add_rule(
            PermissionRuleSource::Session,
            PermissionRule {
                effect: PermissionRuleEffect::Allow,
                scope: RuleScope::Session,
                request_scope: None,
                target: PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: None,
            },
        );

        let outcome = evaluate_permission(&context, request(PermissionScope::Execute), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Allowed);
        assert_eq!(outcome.final_decision, PermissionDecision::Allow);
    }

    #[test]
    fn explicit_ask_beats_mode_default() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.add_rule(
            PermissionRuleSource::User,
            PermissionRule {
                effect: PermissionRuleEffect::Ask,
                scope: RuleScope::Global,
                request_scope: Some(PermissionScope::Read),
                target: PermissionTarget::Tool {
                    name: "read_file".into(),
                },
                source_path: Some(PathBuf::from("/home/test/.quine/permissions.yaml")),
            },
        );

        let outcome = evaluate_permission(
            &context,
            PermissionRequest {
                tool_name: "read_file".into(),
                action: Some("open".into()),
                scope: PermissionScope::Read,
                resource: PermissionResource::Path {
                    path: PathBuf::from("/workspace/notes.txt"),
                },
            },
            None,
        );

        assert_eq!(outcome.kind, PermissionOutcomeKind::RequiresApproval);
        assert_eq!(outcome.final_decision, PermissionDecision::Ask);
        assert_eq!(outcome.source.rule_source, Some(PermissionRuleSource::User));
    }

    #[test]
    fn session_rule_overrides_persisted_workspace_rule_with_source_provenance() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.add_rule(
            PermissionRuleSource::Workspace,
            PermissionRule {
                effect: PermissionRuleEffect::Deny,
                scope: RuleScope::Workspace,
                request_scope: Some(PermissionScope::Execute),
                target: PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: Some(PathBuf::from("/workspace/.quine/permissions.yaml")),
            },
        );
        context.add_rule(
            PermissionRuleSource::Session,
            PermissionRule {
                effect: PermissionRuleEffect::Allow,
                scope: RuleScope::Session,
                request_scope: Some(PermissionScope::Execute),
                target: PermissionTarget::Tool {
                    name: "bash".into(),
                },
                source_path: None,
            },
        );

        let outcome = evaluate_permission(&context, request(PermissionScope::Execute), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Allowed);
        assert_eq!(
            outcome.source.rule_source,
            Some(PermissionRuleSource::Session)
        );
        assert_eq!(
            outcome
                .matched_rule
                .as_ref()
                .and_then(|rule| rule.request_scope),
            Some(PermissionScope::Execute)
        );
    }

    #[test]
    fn mode_default_allows_read_only_bash_commands() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let request = PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope: PermissionScope::Execute,
            resource: PermissionResource::Command {
                descriptor: analyze_command("pwd"),
            },
        };

        let outcome = evaluate_permission(&context, request, None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Allowed);
        assert_eq!(outcome.final_decision, PermissionDecision::Allow);
        assert!(outcome.reason.contains("read-oriented bash command"));
    }

    #[test]
    fn mode_default_requires_approval_for_mutating_bash_commands() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let request = PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope: PermissionScope::Execute,
            resource: PermissionResource::Command {
                descriptor: analyze_command("touch test.txt"),
            },
        };

        let outcome = evaluate_permission(&context, request, None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::RequiresApproval);
        assert_eq!(outcome.final_decision, PermissionDecision::Ask);
        assert!(outcome.reason.contains("mutating bash command"));
    }

    #[test]
    fn mode_default_requires_approval_for_nested_shell_bash_commands() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let request = PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope: PermissionScope::Execute,
            resource: PermissionResource::Command {
                descriptor: analyze_command("sh -c 'pwd'"),
            },
        };

        let outcome = evaluate_permission(&context, request, None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::RequiresApproval);
        assert_eq!(outcome.final_decision, PermissionDecision::Ask);
        assert!(outcome.reason.contains("nested-shell bash command"));
    }

    #[test]
    fn analyzed_command_metadata_changes_mode_default_outcome() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let read_only = PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope: PermissionScope::Execute,
            resource: PermissionResource::Command {
                descriptor: analyze_command("pwd"),
            },
        };
        let interpreter = PermissionRequest {
            tool_name: "bash".into(),
            action: Some("run".into()),
            scope: PermissionScope::Execute,
            resource: PermissionResource::Command {
                descriptor: analyze_command("python -c 'print(1)'"),
            },
        };

        let read_outcome = evaluate_permission(&context, read_only, None);
        let interpreter_outcome = evaluate_permission(&context, interpreter, None);

        assert_eq!(read_outcome.kind, PermissionOutcomeKind::Allowed);
        assert_eq!(
            interpreter_outcome.kind,
            PermissionOutcomeKind::RequiresApproval
        );
        let PermissionResource::Command {
            descriptor: read_descriptor,
        } = &read_outcome.request.resource
        else {
            panic!("expected command resource");
        };
        let PermissionResource::Command {
            descriptor: interpreter_descriptor,
        } = &interpreter_outcome.request.resource
        else {
            panic!("expected command resource");
        };
        assert_eq!(read_descriptor.risk, CommandRisk::ReadOnly);
        assert_eq!(interpreter_descriptor.risk, CommandRisk::Interpreter);
    }

    #[test]
    fn defer_falls_through_to_mode_default() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );

        let outcome = evaluate_permission(
            &context,
            request(PermissionScope::Execute),
            Some(ToolLocalDecision {
                decision: PermissionDecision::Defer,
                reason: Some("let engine decide".into()),
            }),
        );

        assert_eq!(outcome.kind, PermissionOutcomeKind::RequiresApproval);
        assert_eq!(outcome.source.kind, PermissionMatchKind::ModeDefault);
    }

    #[test]
    fn plan_mode_default_denies_non_read_requests() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.set_mode(PermissionMode::Plan);

        let outcome = evaluate_permission(&context, request(PermissionScope::Write), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(outcome.final_decision, PermissionDecision::Deny);
    }

    #[test]
    fn headless_ask_fails_safe() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Headless,
        );

        let outcome = evaluate_permission(&context, request(PermissionScope::Execute), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(outcome.final_decision, PermissionDecision::Deny);
        assert_eq!(outcome.source.kind, PermissionMatchKind::HeadlessFallback);
        assert!(outcome
            .reason
            .contains("headless sessions cannot satisfy approval prompts"));
    }

    #[test]
    fn background_ask_fails_safe() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Background,
        );

        let outcome = evaluate_permission(&context, request(PermissionScope::Execute), None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(outcome.final_decision, PermissionDecision::Deny);
        assert_eq!(outcome.source.kind, PermissionMatchKind::HeadlessFallback);
        assert!(outcome
            .reason
            .contains("background sessions cannot satisfy approval prompts"));
    }

    #[test]
    fn outcome_serializes_with_source_attribution() {
        let context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let outcome = evaluate_permission(&context, request(PermissionScope::Read), None);

        let json = serde_json::to_value(&outcome).expect("serialize outcome");
        assert_eq!(json["kind"], "allowed");
        assert_eq!(json["source"]["kind"], "mode_default");
        assert_eq!(json["request"]["tool_name"], "bash");
    }

    #[test]
    fn filesystem_boundary_denies_outside_root_before_mode_default() {
        let workspace = TempDir::new().unwrap();
        let outside = TempDir::new().unwrap();
        let context = PermissionContext::new(
            workspace.path().to_path_buf(),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let request = PermissionRequest {
            tool_name: "apply_patch".into(),
            action: None,
            scope: PermissionScope::Write,
            resource: PermissionResource::Path {
                path: outside.path().join("forbidden.txt"),
            },
        };

        let outcome = evaluate_permission(&context, request, None);

        assert_eq!(outcome.kind, PermissionOutcomeKind::Denied);
        assert_eq!(outcome.source.kind, PermissionMatchKind::FilesystemBoundary);
        assert!(outcome.reason.contains("outside approved roots"));
    }
}
