use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use quine_core::{
    authorize_memory_write, build_memory_permission_context, resolve_scoped_memory_paths,
    workspace_is_trusted, MemoryAuthorizationReason, MemoryDecisionReason, MemoryPermissionContext,
    MemoryStatus, PersistedPersistentMemoryState, PersistedSession,
    PersistentExtractionDiagnostics, PersistentMemoryScope, ScopedMemoryPaths,
    ScopedPersistentMemoryState,
};
use quine_llm::{Message, MessageContent, Role};
use serde::{Deserialize, Serialize};
use tokio::fs;

#[allow(dead_code)]
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentMemoryPaths {
    pub root: PathBuf,
    pub index_markdown_path: PathBuf,
    pub index_json_path: PathBuf,
    pub entries_dir: PathBuf,
    pub tombstones_dir: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersistentMemorySource {
    Explicit,
    Heuristic,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PersistentMemoryStatus {
    Active,
    Tombstoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentMemoryFrontmatter {
    pub entry_id: String,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub keywords: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub source: PersistentMemorySource,
    pub status: PersistentMemoryStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentMemoryRecord {
    pub slug: String,
    pub frontmatter: PersistentMemoryFrontmatter,
    pub body: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentMemoryIndexEntry {
    pub entry_id: String,
    pub title: String,
    pub summary: String,
    pub slug: String,
    pub path: String,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub keywords: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentMemoryIndex {
    pub project_key: String,
    pub entries: Vec<PersistentMemoryIndexEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PersistentMemoryTombstone {
    pub entry_id: String,
    pub reason: String,
    pub deleted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ExtractionDecision {
    Upsert(PersistentMemoryRecord),
    Tombstone { entry_id: String, reason: String },
    Ignore,
}

#[derive(Debug, Clone)]
pub struct MemoryStore {
    root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractionResult {
    pub state: Option<PersistedPersistentMemoryState>,
    pub diagnostics: PersistentExtractionDiagnostics,
    pub writable_scope: Option<PersistentMemoryScope>,
    pub write_status: MemoryStatus,
    pub write_reason: Option<MemoryAuthorizationReason>,
}

impl MemoryStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[allow(dead_code)]
    #[cfg(test)]
    #[allow(dead_code)]
    pub fn project_paths(&self, project_root: &std::path::Path) -> PersistentMemoryPaths {
        let key = project_key(project_root);
        let root = self.root.join("projects").join(&key);
        PersistentMemoryPaths {
            index_markdown_path: root.join("MEMORY.md"),
            index_json_path: root.join("index.json"),
            entries_dir: root.join("entries"),
            tombstones_dir: root.join("tombstones"),
            root,
        }
    }

    pub async fn extract_and_persist_for_session(
        &self,
        session: &PersistedSession,
    ) -> Result<ExtractionResult> {
        let Some(memory_state) = session.memory_state.as_ref() else {
            return Ok(ExtractionResult {
                state: None,
                diagnostics: PersistentExtractionDiagnostics {
                    attempted: false,
                    status: MemoryStatus::Skipped,
                    reason: Some(MemoryDecisionReason::NotAttempted),
                    last_extracted_message_index: None,
                    created: 0,
                    updated: 0,
                    tombstoned: 0,
                    ignored: 0,
                },
                writable_scope: None,
                write_status: MemoryStatus::Skipped,
                write_reason: Some(MemoryAuthorizationReason::ScopeUnavailable),
            });
        };
        let persistent =
            memory_state
                .persistent_memory
                .clone()
                .unwrap_or(PersistedPersistentMemoryState {
                    enabled: true,
                    last_extracted_message_index: None,
                    scope_state: None,
                });
        if !persistent.enabled {
            return Ok(ExtractionResult {
                state: Some(persistent.clone()),
                diagnostics: PersistentExtractionDiagnostics {
                    attempted: false,
                    status: MemoryStatus::Skipped,
                    reason: Some(MemoryDecisionReason::Disabled),
                    last_extracted_message_index: persistent.last_extracted_message_index,
                    created: 0,
                    updated: 0,
                    tombstoned: 0,
                    ignored: 0,
                },
                writable_scope: persistent
                    .scope_state
                    .and_then(|state| state.writable_scope),
                write_status: MemoryStatus::Skipped,
                write_reason: Some(MemoryAuthorizationReason::ScopeDisabled),
            });
        }

        let resolution = resolve_scoped_memory_paths(
            &self.root,
            &session.config.memory_policy,
            &session.config.working_directory,
            session.config.agent_key.as_deref(),
            session.config.team_key.as_deref(),
        );
        let writable_scope = resolution
            .writable_scope
            .as_ref()
            .map(|item| item.scope.clone());
        let write_context = build_memory_permission_context(
            workspace_is_trusted(&session.config.working_directory),
            false,
            session.config.agent_key.as_deref(),
            session.config.team_key.as_deref(),
        );
        let start = persistent
            .last_extracted_message_index
            .map_or(0, |index| index.saturating_add(1));
        if start >= session.history.len() {
            return Ok(ExtractionResult {
                state: Some(persistent.clone()),
                diagnostics: PersistentExtractionDiagnostics {
                    attempted: false,
                    status: MemoryStatus::Skipped,
                    reason: Some(MemoryDecisionReason::NoNewMessages),
                    last_extracted_message_index: persistent.last_extracted_message_index,
                    created: 0,
                    updated: 0,
                    tombstoned: 0,
                    ignored: 0,
                },
                writable_scope,
                write_status: MemoryStatus::Skipped,
                write_reason: None,
            });
        }

        let paths = resolution.writable_scope.as_ref().cloned();
        let mut live_records = match paths.as_ref() {
            Some(paths) => self.load_live_records(paths).await?,
            None => HashMap::new(),
        };
        let mut changed = false;
        let mut created = 0usize;
        let mut updated = 0usize;
        let mut tombstoned = 0usize;
        let mut ignored = 0usize;
        let mut write_reason = None;
        let mut write_status = MemoryStatus::NotRun;
        for message in &session.history[start..] {
            let explicit_user_memory_intent = is_explicit_user_memory_intent(message);
            let message_permission_context = MemoryPermissionContext {
                explicit_user_memory_intent,
                ..write_context.clone()
            };
            match self.decision_for_message(message, &live_records) {
                ExtractionDecision::Upsert(record) => {
                    let Some(paths) = paths.as_ref() else {
                        write_status = MemoryStatus::Skipped;
                        write_reason = Some(MemoryAuthorizationReason::ScopeUnavailable);
                        ignored += 1;
                        continue;
                    };
                    if let Err(reason) = authorize_memory_write(
                        &session.config.memory_policy.write_policy,
                        &paths.scope,
                        &message_permission_context,
                    ) {
                        write_status = MemoryStatus::Skipped;
                        write_reason = Some(reason);
                        ignored += 1;
                        continue;
                    }
                    if live_records.contains_key(&record.frontmatter.entry_id) {
                        updated += 1;
                    } else {
                        created += 1;
                    }
                    live_records.insert(record.frontmatter.entry_id.clone(), record);
                    changed = true;
                }
                ExtractionDecision::Tombstone { entry_id, reason } => {
                    let Some(paths) = paths.as_ref() else {
                        write_status = MemoryStatus::Skipped;
                        write_reason = Some(MemoryAuthorizationReason::ScopeUnavailable);
                        ignored += 1;
                        continue;
                    };
                    if let Err(deny_reason) = authorize_memory_write(
                        &session.config.memory_policy.write_policy,
                        &paths.scope,
                        &message_permission_context,
                    ) {
                        write_status = MemoryStatus::Skipped;
                        write_reason = Some(deny_reason);
                        ignored += 1;
                        continue;
                    }
                    if live_records.remove(&entry_id).is_some() {
                        self.write_tombstone(paths, &entry_id, &reason).await?;
                        changed = true;
                        tombstoned += 1;
                    } else {
                        ignored += 1;
                    }
                }
                ExtractionDecision::Ignore => ignored += 1,
            }
        }

        if changed {
            let paths = paths.as_ref().expect("paths must exist for changed writes");
            self.persist_records(paths, &live_records).await?;
            write_status = MemoryStatus::Succeeded;
        } else if write_reason.is_none() {
            write_status = MemoryStatus::Skipped;
        }

        let state = PersistedPersistentMemoryState {
            enabled: true,
            last_extracted_message_index: Some(session.history.len().saturating_sub(1)),
            scope_state: Some(ScopedPersistentMemoryState {
                readable_scopes: resolution
                    .readable_scopes
                    .iter()
                    .map(|item| item.scope.clone())
                    .collect(),
                writable_scope: writable_scope.clone(),
                lookup_order: resolution.lookup_order,
                conflict_resolution: resolution.conflict_resolution,
            }),
        };
        Ok(ExtractionResult {
            state: Some(state.clone()),
            diagnostics: PersistentExtractionDiagnostics {
                attempted: true,
                status: if changed {
                    MemoryStatus::Succeeded
                } else {
                    MemoryStatus::Skipped
                },
                reason: if changed {
                    None
                } else {
                    Some(MemoryDecisionReason::NoChanges)
                },
                last_extracted_message_index: state.last_extracted_message_index,
                created,
                updated,
                tombstoned,
                ignored,
            },
            writable_scope,
            write_status,
            write_reason,
        })
    }

    fn decision_for_message(
        &self,
        message: &Message,
        live_records: &HashMap<String, PersistentMemoryRecord>,
    ) -> ExtractionDecision {
        let MessageContent::Text(text) = &message.content else {
            return ExtractionDecision::Ignore;
        };
        let normalized = text.trim();
        if normalized.is_empty() {
            return ExtractionDecision::Ignore;
        }

        if message.role == Role::User {
            let lowercase = normalized.to_ascii_lowercase();
            if let Some(content) = extract_explicit_remember(normalized, &lowercase) {
                return ExtractionDecision::Upsert(build_record(
                    content,
                    PersistentMemorySource::Explicit,
                ));
            }
            if let Some(target) = extract_explicit_forget(normalized, &lowercase) {
                let target_normalized = normalize_fact(target);
                if let Some(record) = live_records
                    .values()
                    .find(|record| normalize_fact(&record.body) == target_normalized)
                {
                    return ExtractionDecision::Tombstone {
                        entry_id: record.frontmatter.entry_id.clone(),
                        reason: "explicit forget".into(),
                    };
                }
            }
            if let Some(content) = extract_heuristic_memory(normalized, &lowercase) {
                let record = build_record(content, PersistentMemorySource::Heuristic);
                if live_records.contains_key(&record.frontmatter.entry_id) {
                    return ExtractionDecision::Ignore;
                }
                return ExtractionDecision::Upsert(record);
            }
        }

        ExtractionDecision::Ignore
    }

    async fn load_live_records(
        &self,
        paths: &ScopedMemoryPaths,
    ) -> Result<HashMap<String, PersistentMemoryRecord>> {
        let mut records = HashMap::new();
        let mut dir = match fs::read_dir(&paths.entries_dir).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(records),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = dir.next_entry().await? {
            if !entry.file_type().await?.is_file() {
                continue;
            }
            let content = fs::read_to_string(entry.path()).await?;
            let record = parse_record(&content)?;
            if record.frontmatter.status == PersistentMemoryStatus::Active {
                records.insert(record.frontmatter.entry_id.clone(), record);
            }
        }
        Ok(records)
    }

    async fn persist_records(
        &self,
        paths: &ScopedMemoryPaths,
        records: &HashMap<String, PersistentMemoryRecord>,
    ) -> Result<()> {
        fs::create_dir_all(&paths.entries_dir).await?;
        fs::create_dir_all(&paths.tombstones_dir).await?;

        let mut expected = HashSet::new();
        let mut ordered: Vec<_> = records.values().cloned().collect();
        ordered.sort_by(|left, right| {
            left.frontmatter
                .title
                .cmp(&right.frontmatter.title)
                .then_with(|| left.frontmatter.entry_id.cmp(&right.frontmatter.entry_id))
        });

        for record in &ordered {
            let path = paths.entries_dir.join(format!("{}.md", record.slug));
            expected.insert(path.clone());
            fs::write(&path, render_record(record)).await?;
        }

        let mut dir = match fs::read_dir(&paths.entries_dir).await {
            Ok(dir) => dir,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        while let Some(entry) = dir.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_file() && !expected.contains(&path) {
                fs::remove_file(path).await?;
            }
        }

        let index = PersistentMemoryIndex {
            project_key: paths
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string(),
            entries: ordered
                .iter()
                .map(|record| PersistentMemoryIndexEntry {
                    entry_id: record.frontmatter.entry_id.clone(),
                    title: record.frontmatter.title.clone(),
                    summary: record.frontmatter.summary.clone(),
                    slug: record.slug.clone(),
                    path: format!("entries/{}.md", record.slug),
                    updated_at: record.frontmatter.updated_at,
                    keywords: record.frontmatter.keywords.clone(),
                })
                .collect(),
        };
        fs::write(&paths.index_json_path, serde_json::to_vec_pretty(&index)?).await?;
        fs::write(&paths.index_markdown_path, render_memory_index(&index)).await?;
        Ok(())
    }

    async fn write_tombstone(
        &self,
        paths: &ScopedMemoryPaths,
        entry_id: &str,
        reason: &str,
    ) -> Result<()> {
        fs::create_dir_all(&paths.tombstones_dir).await?;
        let tombstone = PersistentMemoryTombstone {
            entry_id: entry_id.to_string(),
            reason: reason.to_string(),
            deleted_at: Utc::now(),
        };
        let path = paths.tombstones_dir.join(format!("{entry_id}.json"));
        fs::write(path, serde_json::to_vec_pretty(&tombstone)?).await?;
        Ok(())
    }
}

#[allow(dead_code)]
#[cfg(test)]
pub fn resolve_project_root(working_directory: &std::path::Path) -> PathBuf {
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

#[cfg(test)]
pub fn project_key(project_root: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let normalized = project_root.to_string_lossy().replace('\\', "/");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

fn extract_explicit_remember<'a>(text: &'a str, lowercase: &str) -> Option<&'a str> {
    for marker in ["remember this:", "remember:", "please remember:"] {
        if let Some(index) = lowercase.find(marker) {
            let content = text[index + marker.len()..].trim();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    None
}

fn extract_explicit_forget<'a>(text: &'a str, lowercase: &str) -> Option<&'a str> {
    for marker in ["forget this:", "forget:", "please forget:"] {
        if let Some(index) = lowercase.find(marker) {
            let content = text[index + marker.len()..].trim();
            if !content.is_empty() {
                return Some(content);
            }
        }
    }
    None
}

fn is_explicit_user_memory_intent(message: &Message) -> bool {
    let MessageContent::Text(text) = &message.content else {
        return false;
    };
    let normalized = text.trim();
    let lowercase = normalized.to_ascii_lowercase();
    extract_explicit_remember(normalized, &lowercase).is_some()
        || extract_explicit_forget(normalized, &lowercase).is_some()
}

fn extract_heuristic_memory<'a>(text: &'a str, lowercase: &str) -> Option<&'a str> {
    if lowercase.contains("my preference is") {
        return text
            .split_once(':')
            .map(|(_, tail)| tail.trim())
            .filter(|tail| !tail.is_empty());
    }
    None
}

fn build_record(content: &str, source: PersistentMemorySource) -> PersistentMemoryRecord {
    let normalized = normalize_fact(content);
    let slug = slugify(content);
    let title = titleize(content);
    let keywords = extract_keywords(content);
    let now = Utc::now();
    PersistentMemoryRecord {
        slug,
        frontmatter: PersistentMemoryFrontmatter {
            entry_id: normalized.clone(),
            title,
            summary: content.trim().to_string(),
            keywords,
            created_at: now,
            updated_at: now,
            source,
            status: PersistentMemoryStatus::Active,
        },
        body: content.trim().to_string(),
    }
}

fn normalize_fact(content: &str) -> String {
    content
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                ' '
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
}

fn slugify(content: &str) -> String {
    let slug = normalize_fact(content);
    if slug.is_empty() {
        "memory".into()
    } else {
        slug
    }
}

fn titleize(content: &str) -> String {
    let trimmed = content.trim();
    if trimmed.len() <= 60 {
        trimmed.to_string()
    } else {
        format!("{}…", trimmed.chars().take(57).collect::<String>())
    }
}

fn extract_keywords(content: &str) -> Vec<String> {
    let mut seen = BTreeMap::new();
    for token in content
        .split(|character: char| !character.is_ascii_alphanumeric())
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| token.len() >= 4)
    {
        seen.entry(token.clone()).or_insert(token);
    }
    seen.into_values().take(5).collect()
}

pub fn render_record(record: &PersistentMemoryRecord) -> String {
    let frontmatter = serde_yaml::to_string(&record.frontmatter).expect("frontmatter serializes");
    format!("---\n{}---\n\n{}\n", frontmatter, record.body.trim())
}

pub fn parse_record(content: &str) -> Result<PersistentMemoryRecord> {
    let trimmed = content.trim_start();
    let Some(rest) = trimmed.strip_prefix("---\n") else {
        anyhow::bail!("missing frontmatter delimiter");
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        anyhow::bail!("missing closing frontmatter delimiter");
    };
    let frontmatter: PersistentMemoryFrontmatter = serde_yaml::from_str(frontmatter)
        .context("failed to parse persistent memory frontmatter")?;
    Ok(PersistentMemoryRecord {
        slug: slugify(&frontmatter.title),
        frontmatter,
        body: body.trim().to_string(),
    })
}

pub fn render_memory_index(index: &PersistentMemoryIndex) -> String {
    let mut rendered = String::from("# MEMORY\n\n");
    if index.entries.is_empty() {
        rendered.push_str("No durable memories recorded.\n");
        return rendered;
    }
    for entry in &index.entries {
        rendered.push_str(&format!(
            "- [{}]({}) — {}\n",
            entry.title, entry.path, entry.summary
        ));
    }
    rendered
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn project_key_is_stable() {
        let a = project_key(Path::new("/tmp/project"));
        let b = project_key(Path::new("/tmp/project"));
        let c = project_key(Path::new("/tmp/other-project"));
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn record_round_trips_through_frontmatter() {
        let record = build_record(
            "Use concise bullets in final responses",
            PersistentMemorySource::Explicit,
        );
        let rendered = render_record(&record);
        let parsed = parse_record(&rendered).unwrap();
        assert_eq!(parsed.frontmatter.entry_id, record.frontmatter.entry_id);
        assert_eq!(parsed.frontmatter.title, record.frontmatter.title);
        assert_eq!(parsed.body, record.body);
    }

    #[test]
    fn malformed_frontmatter_is_rejected() {
        assert!(parse_record("hello").is_err());
    }

    #[test]
    fn memory_index_is_deterministic() {
        let mut entries = vec![
            PersistentMemoryIndexEntry {
                entry_id: "b".into(),
                title: "B".into(),
                summary: "Second".into(),
                slug: "b".into(),
                path: "entries/b.md".into(),
                updated_at: Utc::now(),
                keywords: vec![],
            },
            PersistentMemoryIndexEntry {
                entry_id: "a".into(),
                title: "A".into(),
                summary: "First".into(),
                slug: "a".into(),
                path: "entries/a.md".into(),
                updated_at: Utc::now(),
                keywords: vec![],
            },
        ];
        entries.sort_by(|left, right| left.title.cmp(&right.title));
        let index = PersistentMemoryIndex {
            project_key: "project".into(),
            entries,
        };
        assert_eq!(render_memory_index(&index), render_memory_index(&index));
    }

    #[tokio::test]
    async fn one_memory_per_file_and_tombstones_live_separately() {
        let root =
            std::env::temp_dir().join(format!("quine-memory-store-{}", uuid::Uuid::new_v4()));
        let store = MemoryStore::new(root.clone());
        let paths = quine_core::resolve_scoped_memory_paths(
            &root,
            &quine_core::MemoryPolicyConfig::default(),
            std::path::Path::new("/tmp/project"),
            None,
            None,
        )
        .readable_scopes
        .into_iter()
        .next()
        .unwrap();
        let mut records = HashMap::new();
        let record = build_record(
            "Use anyhow for app-level errors",
            PersistentMemorySource::Explicit,
        );
        records.insert(record.frontmatter.entry_id.clone(), record.clone());
        store.persist_records(&paths, &records).await.unwrap();
        assert!(paths
            .entries_dir
            .join(format!("{}.md", record.slug))
            .exists());
        store
            .write_tombstone(&paths, &record.frontmatter.entry_id, "test forget")
            .await
            .unwrap();
        assert!(paths
            .tombstones_dir
            .join(format!("{}.json", record.frontmatter.entry_id))
            .exists());
    }
}
