use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PersistentMemoryScope {
    Project {
        project_key: String,
    },
    Agent {
        project_key: String,
        agent_key: String,
    },
    Team {
        team_key: String,
    },
}

impl PersistentMemoryScope {
    pub fn project(project_key: impl Into<String>) -> Self {
        Self::Project {
            project_key: project_key.into(),
        }
    }

    pub fn agent(project_key: impl Into<String>, agent_key: impl Into<String>) -> Self {
        Self::Agent {
            project_key: project_key.into(),
            agent_key: agent_key.into(),
        }
    }

    pub fn team(team_key: impl Into<String>) -> Self {
        Self::Team {
            team_key: team_key.into(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Project { .. } => "project",
            Self::Agent { .. } => "agent",
            Self::Team { .. } => "team",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryFeatureFlags {
    pub session_memory_enabled: bool,
    pub persistent_memory_enabled: bool,
    pub relevant_memory_enabled: bool,
    pub team_memory_enabled: bool,
    pub agent_memory_enabled: bool,
    pub advanced_scopes_enabled: bool,
}

impl Default for MemoryFeatureFlags {
    fn default() -> Self {
        Self {
            session_memory_enabled: true,
            persistent_memory_enabled: true,
            relevant_memory_enabled: true,
            team_memory_enabled: false,
            agent_memory_enabled: false,
            advanced_scopes_enabled: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryReadPolicy {
    pub allow_project_scope: bool,
    pub allow_agent_scope: bool,
    pub allow_team_scope: bool,
    pub allow_cross_scope_recall: bool,
}

impl Default for MemoryReadPolicy {
    fn default() -> Self {
        Self {
            allow_project_scope: true,
            allow_agent_scope: true,
            allow_team_scope: true,
            allow_cross_scope_recall: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryWritePolicy {
    pub allow_project_writes: bool,
    pub allow_agent_writes: bool,
    pub allow_team_writes: bool,
    pub require_trusted_workspace_for_writes: bool,
    pub require_explicit_user_intent_for_agent_writes: bool,
    pub require_explicit_user_intent_for_team_writes: bool,
}

impl Default for MemoryWritePolicy {
    fn default() -> Self {
        Self {
            allow_project_writes: true,
            allow_agent_writes: true,
            allow_team_writes: true,
            require_trusted_workspace_for_writes: false,
            require_explicit_user_intent_for_agent_writes: false,
            require_explicit_user_intent_for_team_writes: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScopedMemoryLookupOrder {
    #[default]
    ProjectOnly,
    ProjectThenAgent,
    ProjectThenTeam,
    ProjectThenAgentThenTeam,
    ProjectThenTeamThenAgent,
    AgentThenProject,
    TeamThenProject,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum MemoryConflictResolution {
    #[default]
    PreferNarrowerScope,
    PreferBroaderScope,
    PreferMostRecentlyUpdated,
    ErrorOnConflictingWrites,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryScopePolicy {
    pub read_policy: MemoryReadPolicy,
    pub write_policy: MemoryWritePolicy,
    pub default_write_scope: PersistentMemoryScope,
    pub lookup_order: ScopedMemoryLookupOrder,
    pub conflict_resolution: MemoryConflictResolution,
}

impl Default for MemoryScopePolicy {
    fn default() -> Self {
        Self {
            read_policy: MemoryReadPolicy::default(),
            write_policy: MemoryWritePolicy::default(),
            default_write_scope: PersistentMemoryScope::project("default"),
            lookup_order: ScopedMemoryLookupOrder::ProjectOnly,
            conflict_resolution: MemoryConflictResolution::PreferNarrowerScope,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MemoryPolicyConfig {
    #[serde(default)]
    pub flags: MemoryFeatureFlags,
    #[serde(default)]
    pub root_override: Option<PathBuf>,
    #[serde(default)]
    pub team_root_override: Option<PathBuf>,
    #[serde(default)]
    pub agent_root_override: Option<PathBuf>,
    #[serde(default)]
    pub read_policy: MemoryReadPolicy,
    #[serde(default)]
    pub write_policy: MemoryWritePolicy,
    #[serde(default)]
    pub default_write_scope: Option<ScopeSelector>,
    #[serde(default)]
    pub lookup_order: ScopedMemoryLookupOrder,
    #[serde(default)]
    pub conflict_resolution: MemoryConflictResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScopeSelector {
    #[default]
    Project,
    Agent,
    Team,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopedMemoryPaths {
    pub scope: PersistentMemoryScope,
    pub root: PathBuf,
    pub index_markdown_path: PathBuf,
    pub index_json_path: PathBuf,
    pub entries_dir: PathBuf,
    pub tombstones_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopedMemoryResolution {
    pub readable_scopes: Vec<ScopedMemoryPaths>,
    pub writable_scope: Option<ScopedMemoryPaths>,
    pub lookup_order: ScopedMemoryLookupOrder,
    pub conflict_resolution: MemoryConflictResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ScopedPersistentMemoryState {
    pub readable_scopes: Vec<PersistentMemoryScope>,
    pub writable_scope: Option<PersistentMemoryScope>,
    pub lookup_order: ScopedMemoryLookupOrder,
    pub conflict_resolution: MemoryConflictResolution,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct MemoryPermissionContext {
    pub workspace_is_trusted: bool,
    pub explicit_user_memory_intent: bool,
    pub active_agent_key: Option<String>,
    pub active_team_key: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MemoryAuthorizationReason {
    ScopeDisabled,
    ScopeUnavailable,
    TrustedWorkspaceRequired,
    ExplicitIntentRequired,
}

pub fn resolve_project_root(working_directory: &Path) -> PathBuf {
    let mut current = working_directory.to_path_buf();
    loop {
        if current.join(".git").exists()
            || current.join("CLAUDE.md").exists()
            || current.join("Cargo.toml").exists()
        {
            return current;
        }
        if !current.pop() {
            return working_directory.to_path_buf();
        }
    }
}

pub fn project_key(project_root: &Path) -> String {
    let normalized = project_root.to_string_lossy().replace('\\', "/");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

fn scope_sort_key(scope: &PersistentMemoryScope) -> u8 {
    match scope {
        PersistentMemoryScope::Project { .. } => 0,
        PersistentMemoryScope::Team { .. } => 1,
        PersistentMemoryScope::Agent { .. } => 2,
    }
}

fn scope_rank(scope: &PersistentMemoryScope) -> u8 {
    match scope {
        PersistentMemoryScope::Project { .. } => 0,
        PersistentMemoryScope::Team { .. } => 1,
        PersistentMemoryScope::Agent { .. } => 2,
    }
}

pub fn build_memory_permission_context(
    workspace_is_trusted: bool,
    explicit_user_memory_intent: bool,
    agent_key: Option<&str>,
    team_key: Option<&str>,
) -> MemoryPermissionContext {
    MemoryPermissionContext {
        workspace_is_trusted,
        explicit_user_memory_intent,
        active_agent_key: agent_key.map(ToOwned::to_owned),
        active_team_key: team_key.map(ToOwned::to_owned),
    }
}

pub fn workspace_is_trusted(working_directory: &Path) -> bool {
    let project_root = resolve_project_root(working_directory);
    project_root.join(".git").exists()
        || project_root.join("CLAUDE.md").exists()
        || project_root.join("Cargo.toml").exists()
}

pub fn authorize_memory_read(policy: &MemoryReadPolicy, scope: &PersistentMemoryScope) -> bool {
    match scope {
        PersistentMemoryScope::Project { .. } => policy.allow_project_scope,
        PersistentMemoryScope::Agent { .. } => policy.allow_agent_scope,
        PersistentMemoryScope::Team { .. } => policy.allow_team_scope,
    }
}

pub fn authorize_memory_write(
    policy: &MemoryWritePolicy,
    scope: &PersistentMemoryScope,
    context: &MemoryPermissionContext,
) -> Result<(), MemoryAuthorizationReason> {
    match scope {
        PersistentMemoryScope::Project { .. } => {
            if !policy.allow_project_writes {
                return Err(MemoryAuthorizationReason::ScopeDisabled);
            }
        }
        PersistentMemoryScope::Agent { .. } => {
            if !policy.allow_agent_writes {
                return Err(MemoryAuthorizationReason::ScopeDisabled);
            }
            if context.active_agent_key.is_none() {
                return Err(MemoryAuthorizationReason::ScopeUnavailable);
            }
            if policy.require_explicit_user_intent_for_agent_writes
                && !context.explicit_user_memory_intent
            {
                return Err(MemoryAuthorizationReason::ExplicitIntentRequired);
            }
        }
        PersistentMemoryScope::Team { .. } => {
            if !policy.allow_team_writes {
                return Err(MemoryAuthorizationReason::ScopeDisabled);
            }
            if context.active_team_key.is_none() {
                return Err(MemoryAuthorizationReason::ScopeUnavailable);
            }
            if policy.require_explicit_user_intent_for_team_writes
                && !context.explicit_user_memory_intent
            {
                return Err(MemoryAuthorizationReason::ExplicitIntentRequired);
            }
        }
    }

    if policy.require_trusted_workspace_for_writes && !context.workspace_is_trusted {
        return Err(MemoryAuthorizationReason::TrustedWorkspaceRequired);
    }

    Ok(())
}

pub fn resolve_scoped_memory_paths(
    memory_root: &Path,
    config: &MemoryPolicyConfig,
    working_directory: &Path,
    agent_key: Option<&str>,
    team_key: Option<&str>,
) -> ScopedMemoryResolution {
    let project_root = resolve_project_root(working_directory);
    let project_key = project_key(&project_root);
    let project_scope = PersistentMemoryScope::project(project_key.clone());

    let project_base = config
        .root_override
        .clone()
        .unwrap_or_else(|| memory_root.to_path_buf());
    let team_base = config
        .team_root_override
        .clone()
        .unwrap_or_else(|| project_base.clone());
    let agent_base = config
        .agent_root_override
        .clone()
        .unwrap_or_else(|| project_base.clone());

    let project_paths = scoped_paths(project_base, project_scope.clone());
    let agent_paths = if config.flags.advanced_scopes_enabled
        && config.flags.agent_memory_enabled
        && agent_key.is_some()
    {
        Some(scoped_paths(
            agent_base,
            PersistentMemoryScope::agent(project_key.clone(), agent_key.unwrap_or_default()),
        ))
    } else {
        None
    };
    let team_paths = if config.flags.advanced_scopes_enabled
        && config.flags.team_memory_enabled
        && team_key.is_some()
    {
        Some(scoped_paths(
            team_base,
            PersistentMemoryScope::team(team_key.unwrap_or_default()),
        ))
    } else {
        None
    };

    let readable_scopes = ordered_readable_scopes(
        &project_paths,
        agent_paths.as_ref(),
        team_paths.as_ref(),
        config,
    );
    let writable_scope = resolve_writable_scope(
        &project_paths,
        agent_paths.as_ref(),
        team_paths.as_ref(),
        config,
    );

    ScopedMemoryResolution {
        readable_scopes,
        writable_scope,
        lookup_order: config.lookup_order,
        conflict_resolution: config.conflict_resolution,
    }
}

fn ordered_readable_scopes(
    project_paths: &ScopedMemoryPaths,
    agent_paths: Option<&ScopedMemoryPaths>,
    team_paths: Option<&ScopedMemoryPaths>,
    config: &MemoryPolicyConfig,
) -> Vec<ScopedMemoryPaths> {
    let mut scopes = vec![project_paths.clone()];
    match (agent_paths, team_paths) {
        (Some(agent), Some(team)) => {
            if config.read_policy.allow_cross_scope_recall && config.flags.relevant_memory_enabled {
                let order = match config.lookup_order {
                    ScopedMemoryLookupOrder::ProjectThenAgentThenTeam => {
                        vec![project_paths.clone(), agent.clone(), team.clone()]
                    }
                    ScopedMemoryLookupOrder::ProjectThenTeamThenAgent => {
                        vec![project_paths.clone(), team.clone(), agent.clone()]
                    }
                    ScopedMemoryLookupOrder::AgentThenProject => {
                        vec![agent.clone(), project_paths.clone(), team.clone()]
                    }
                    ScopedMemoryLookupOrder::TeamThenProject => {
                        vec![team.clone(), project_paths.clone(), agent.clone()]
                    }
                    ScopedMemoryLookupOrder::ProjectThenTeam => {
                        vec![project_paths.clone(), team.clone()]
                    }
                    ScopedMemoryLookupOrder::ProjectThenAgent => {
                        vec![project_paths.clone(), agent.clone()]
                    }
                    ScopedMemoryLookupOrder::ProjectOnly => vec![project_paths.clone()],
                };
                scopes = order;
            } else {
                scopes = vec![project_paths.clone()];
            }
        }
        (Some(agent), None) => {
            scopes = match config.lookup_order {
                ScopedMemoryLookupOrder::AgentThenProject => {
                    vec![agent.clone(), project_paths.clone()]
                }
                _ => vec![project_paths.clone(), agent.clone()],
            };
        }
        (None, Some(team)) => {
            scopes = match config.lookup_order {
                ScopedMemoryLookupOrder::TeamThenProject => {
                    vec![team.clone(), project_paths.clone()]
                }
                _ => vec![project_paths.clone(), team.clone()],
            };
        }
        (None, None) => {}
    }

    scopes
        .into_iter()
        .filter(|item| authorize_memory_read(&config.read_policy, &item.scope))
        .collect()
}

fn resolve_writable_scope(
    project_paths: &ScopedMemoryPaths,
    agent_paths: Option<&ScopedMemoryPaths>,
    team_paths: Option<&ScopedMemoryPaths>,
    config: &MemoryPolicyConfig,
) -> Option<ScopedMemoryPaths> {
    let selector = config.default_write_scope.clone().unwrap_or_default();
    match selector {
        ScopeSelector::Project => Some(project_paths.clone()),
        ScopeSelector::Agent => agent_paths.cloned().or_else(|| Some(project_paths.clone())),
        ScopeSelector::Team => team_paths.cloned().or_else(|| Some(project_paths.clone())),
    }
}

fn scoped_paths(base_root: PathBuf, scope: PersistentMemoryScope) -> ScopedMemoryPaths {
    let root = match &scope {
        PersistentMemoryScope::Project { project_key } => {
            base_root.join("projects").join(project_key)
        }
        PersistentMemoryScope::Agent {
            project_key,
            agent_key,
        } => base_root.join("agents").join(project_key).join(agent_key),
        PersistentMemoryScope::Team { team_key } => base_root.join("teams").join(team_key),
    };
    ScopedMemoryPaths {
        scope,
        index_markdown_path: root.join("MEMORY.md"),
        index_json_path: root.join("index.json"),
        entries_dir: root.join("entries"),
        tombstones_dir: root.join("tombstones"),
        root,
    }
}

pub fn snapshot_scoped_persistent_memory_state(
    resolution: &ScopedMemoryResolution,
) -> ScopedPersistentMemoryState {
    ScopedPersistentMemoryState {
        readable_scopes: resolution
            .readable_scopes
            .iter()
            .map(|item| item.scope.clone())
            .collect(),
        writable_scope: resolution
            .writable_scope
            .as_ref()
            .map(|item| item.scope.clone()),
        lookup_order: resolution.lookup_order,
        conflict_resolution: resolution.conflict_resolution,
    }
}

pub fn compare_scope_priority(
    left: &PersistentMemoryScope,
    right: &PersistentMemoryScope,
    strategy: MemoryConflictResolution,
) -> std::cmp::Ordering {
    match strategy {
        MemoryConflictResolution::PreferNarrowerScope => scope_rank(right)
            .cmp(&scope_rank(left))
            .then_with(|| scope_sort_key(left).cmp(&scope_sort_key(right))),
        MemoryConflictResolution::PreferBroaderScope => scope_rank(left)
            .cmp(&scope_rank(right))
            .then_with(|| scope_sort_key(left).cmp(&scope_sort_key(right))),
        MemoryConflictResolution::PreferMostRecentlyUpdated
        | MemoryConflictResolution::ErrorOnConflictingWrites => {
            scope_sort_key(left).cmp(&scope_sort_key(right))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_project_only_by_default() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let config = MemoryPolicyConfig::default();
        let resolution = resolve_scoped_memory_paths(temp.path(), &config, temp.path(), None, None);
        assert_eq!(resolution.readable_scopes.len(), 1);
        assert!(matches!(
            resolution.readable_scopes[0].scope,
            PersistentMemoryScope::Project { .. }
        ));
    }

    #[test]
    fn resolves_agent_and_team_when_enabled_with_cross_scope() {
        let temp = tempfile::TempDir::new().unwrap();
        std::fs::write(temp.path().join("Cargo.toml"), "[package]\nname = \"x\"\n").unwrap();
        let config = MemoryPolicyConfig {
            flags: MemoryFeatureFlags {
                advanced_scopes_enabled: true,
                agent_memory_enabled: true,
                team_memory_enabled: true,
                ..MemoryFeatureFlags::default()
            },
            read_policy: MemoryReadPolicy {
                allow_cross_scope_recall: true,
                ..MemoryReadPolicy::default()
            },
            lookup_order: ScopedMemoryLookupOrder::ProjectThenAgentThenTeam,
            ..MemoryPolicyConfig::default()
        };
        let resolution = resolve_scoped_memory_paths(
            temp.path(),
            &config,
            temp.path(),
            Some("planner"),
            Some("infra"),
        );
        assert_eq!(resolution.readable_scopes.len(), 3);
        assert_eq!(resolution.readable_scopes[0].scope.label(), "project");
        assert_eq!(resolution.readable_scopes[1].scope.label(), "agent");
        assert_eq!(resolution.readable_scopes[2].scope.label(), "team");
    }

    #[test]
    fn agent_write_can_require_explicit_intent_and_trust() {
        let scope = PersistentMemoryScope::agent("project", "planner");
        let policy = MemoryWritePolicy {
            require_trusted_workspace_for_writes: true,
            require_explicit_user_intent_for_agent_writes: true,
            ..MemoryWritePolicy::default()
        };
        let denied = authorize_memory_write(
            &policy,
            &scope,
            &build_memory_permission_context(false, false, Some("planner"), None),
        );
        assert_eq!(
            denied,
            Err(MemoryAuthorizationReason::ExplicitIntentRequired)
        );
        let denied_trust = authorize_memory_write(
            &policy,
            &scope,
            &build_memory_permission_context(false, true, Some("planner"), None),
        );
        assert_eq!(
            denied_trust,
            Err(MemoryAuthorizationReason::TrustedWorkspaceRequired)
        );
    }
}
