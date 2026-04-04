use super::{context::PermissionContext, types::PermissionMode};

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ModeTransitionResult {
    Unchanged,
    EnteredPlan,
    ExitedPlan,
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn enter_plan_mode(context: &mut PermissionContext) -> ModeTransitionResult {
    if context.mode() == PermissionMode::Plan {
        return ModeTransitionResult::Unchanged;
    }

    context.set_pre_plan_mode(Some(context.mode()));
    context.set_mode(PermissionMode::Plan);
    ModeTransitionResult::EnteredPlan
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn exit_plan_mode(context: &mut PermissionContext) -> ModeTransitionResult {
    if context.mode() != PermissionMode::Plan {
        return ModeTransitionResult::Unchanged;
    }

    let restored = context.pre_plan_mode().unwrap_or(PermissionMode::Default);
    context.set_mode(restored);
    context.set_pre_plan_mode(None);
    ModeTransitionResult::ExitedPlan
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::permission::types::PermissionPromptBehavior;

    #[test]
    fn plan_mode_transitions_preserve_prior_mode() {
        let mut context = PermissionContext::new(
            PathBuf::from("/workspace"),
            false,
            PermissionPromptBehavior::Interactive,
        );
        context.set_mode(PermissionMode::AcceptEdits);

        assert_eq!(
            enter_plan_mode(&mut context),
            ModeTransitionResult::EnteredPlan
        );
        assert_eq!(context.mode(), PermissionMode::Plan);
        assert_eq!(context.pre_plan_mode(), Some(PermissionMode::AcceptEdits));

        assert_eq!(
            enter_plan_mode(&mut context),
            ModeTransitionResult::Unchanged
        );
        assert_eq!(context.pre_plan_mode(), Some(PermissionMode::AcceptEdits));

        assert_eq!(
            exit_plan_mode(&mut context),
            ModeTransitionResult::ExitedPlan
        );
        assert_eq!(context.mode(), PermissionMode::AcceptEdits);
        assert_eq!(context.pre_plan_mode(), None);

        assert_eq!(
            exit_plan_mode(&mut context),
            ModeTransitionResult::Unchanged
        );
        assert_eq!(context.mode(), PermissionMode::AcceptEdits);
    }
}
