use std::path::PathBuf;

use super::types::{
    PermissionMode, PermissionPromptBehavior, PermissionRule, PermissionRuleSet,
    PermissionRuleSource,
};

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PermissionContext {
    mode: PermissionMode,
    pre_plan_mode: Option<PermissionMode>,
    rules: PermissionRuleSet,
    workspace_root: PathBuf,
    additional_allowed_roots: Vec<PathBuf>,
    prompt_behavior: PermissionPromptBehavior,
}

impl PermissionContext {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(
        workspace_root: PathBuf,
        plan_mode: bool,
        prompt_behavior: PermissionPromptBehavior,
    ) -> Self {
        Self {
            mode: if plan_mode {
                PermissionMode::Plan
            } else {
                PermissionMode::Default
            },
            pre_plan_mode: None,
            rules: PermissionRuleSet::default(),
            workspace_root,
            additional_allowed_roots: Vec::new(),
            prompt_behavior,
        }
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn mode(&self) -> PermissionMode {
        self.mode
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_mode(&mut self, mode: PermissionMode) {
        self.mode = mode;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn pre_plan_mode(&self) -> Option<PermissionMode> {
        self.pre_plan_mode
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn set_pre_plan_mode(&mut self, mode: Option<PermissionMode>) {
        self.pre_plan_mode = mode;
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn rules(&self) -> &PermissionRuleSet {
        &self.rules
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn workspace_root(&self) -> &PathBuf {
        &self.workspace_root
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn additional_allowed_roots(&self) -> &[PathBuf] {
        &self.additional_allowed_roots
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn prompt_behavior(&self) -> PermissionPromptBehavior {
        self.prompt_behavior
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn add_rule(&mut self, source: PermissionRuleSource, rule: PermissionRule) {
        self.rules.rules_for_source_mut(source).push(rule);
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn add_allowed_root(&mut self, path: PathBuf) {
        self.additional_allowed_roots.push(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permission::types::{
        PermissionRuleEffect, PermissionRuleSource, PermissionScope, PermissionTarget,
    };

    #[test]
    fn default_permission_context_is_conservative() {
        let workspace_root = PathBuf::from("/workspace");
        let context = PermissionContext::new(
            workspace_root.clone(),
            false,
            PermissionPromptBehavior::Interactive,
        );

        assert_eq!(context.mode(), PermissionMode::Default);
        assert_eq!(context.pre_plan_mode(), None);
        assert_eq!(context.workspace_root(), &workspace_root);
        assert!(context.additional_allowed_roots().is_empty());
        assert_eq!(
            context.prompt_behavior(),
            PermissionPromptBehavior::Interactive
        );
        assert!(context.rules().built_in.is_empty());
        assert!(context.rules().session.is_empty());
        assert!(context.rules().user.is_empty());
        assert!(context.rules().workspace.is_empty());
    }

    #[test]
    fn permission_rules_remain_partitioned_by_source() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let built_in_rule = PermissionRule {
            effect: PermissionRuleEffect::Allow,
            scope: PermissionScope::Workspace,
            target: PermissionTarget::Tool {
                name: "read_file".into(),
            },
        };
        let user_rule = PermissionRule {
            effect: PermissionRuleEffect::Ask,
            scope: PermissionScope::Session,
            target: PermissionTarget::Any,
        };

        context.add_rule(PermissionRuleSource::BuiltIn, built_in_rule.clone());
        context.add_rule(PermissionRuleSource::User, user_rule.clone());

        assert_eq!(context.rules().built_in, vec![built_in_rule]);
        assert!(context.rules().session.is_empty());
        assert_eq!(context.rules().user, vec![user_rule]);
        assert!(context.rules().workspace.is_empty());
    }

    #[test]
    fn additional_allowed_roots_append_without_side_effects() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        let extra_a = PathBuf::from("/tmp/a");
        let extra_b = PathBuf::from("/tmp/b");

        context.add_allowed_root(extra_a.clone());
        context.add_allowed_root(extra_b.clone());

        assert_eq!(context.mode(), PermissionMode::Default);
        assert_eq!(context.pre_plan_mode(), None);
        assert_eq!(
            context.prompt_behavior(),
            PermissionPromptBehavior::Interactive
        );
        assert_eq!(context.additional_allowed_roots(), &[extra_a, extra_b]);
        assert!(context.rules().built_in.is_empty());
        assert!(context.rules().session.is_empty());
        assert!(context.rules().user.is_empty());
        assert!(context.rules().workspace.is_empty());
    }
}
