use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use quine_llm::{Message, Role};
use serde::Deserialize;

use super::{
    compare_scope_priority, MemoryConflictResolution, MemoryDecisionReason,
    MemorySelectionEntryDiagnostics, MemorySkippedEntryDiagnostics, MemoryStatus,
    PersistentMemoryScope, ScopedMemoryPaths,
};
use crate::persistence::{PersistedPromptMemoryState, PromptMemoryMode};

const INDEX_ONLY_CHAR_BUDGET: usize = 2_000;
const TARGETED_MAX_ENTRIES: usize = 2;
const TARGETED_TOTAL_CHAR_BUDGET: usize = 1_500;
const TARGETED_ENTRY_CHAR_BUDGET: usize = 500;
const STALE_CAVEAT: &str = "Durable memory may be stale. Verify it against the repository and the user's current instructions before relying on it.";

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct PersistentMemoryIndex {
    pub(crate) entries: Vec<PersistentMemoryIndexEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct PersistentMemoryIndexEntry {
    pub(crate) entry_id: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    pub(crate) slug: String,
    pub(crate) path: String,
    pub(crate) updated_at: DateTime<Utc>,
    #[serde(default)]
    pub(crate) keywords: Vec<String>,
    #[serde(default)]
    pub(crate) pinned: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct PersistentMemoryFrontmatter {
    pub(crate) entry_id: String,
    pub(crate) title: String,
    pub(crate) summary: String,
    #[serde(default)]
    pub(crate) keywords: Vec<String>,
    pub(crate) updated_at: DateTime<Utc>,
    #[serde(default)]
    pub(crate) pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PersistentMemoryRecord {
    frontmatter: PersistentMemoryFrontmatter,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedPersistentMemoryIndexEntry {
    scope: PersistentMemoryScope,
    root: std::path::PathBuf,
    entry: PersistentMemoryIndexEntry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PromptMemoryRunDiagnostics {
    pub(crate) mode: PromptMemoryMode,
    pub(crate) injection_ran: bool,
    pub(crate) status: MemoryStatus,
    pub(crate) reason: Option<MemoryDecisionReason>,
    pub(crate) selected_entries: Vec<MemorySelectionEntryDiagnostics>,
    pub(crate) skipped_entries: Vec<MemorySkippedEntryDiagnostics>,
    pub(crate) truncated: bool,
    pub(crate) conflict_winner_scope: Option<PersistentMemoryScope>,
}

#[derive(Debug, Clone)]
pub(crate) struct PromptMemoryInjection {
    pub(crate) system_prompt_suffix: Option<String>,
    pub(crate) inserted_messages: Vec<Message>,
    pub(crate) summary: PersistedPromptMemoryState,
    pub(crate) latest_user_index: Option<usize>,
    pub(crate) diagnostics: PromptMemoryRunDiagnostics,
}

pub(crate) async fn build_prompt_memory_injection(
    readable_scopes: &[ScopedMemoryPaths],
    mode: PromptMemoryMode,
    history: &[Message],
    previously_selected_entry_ids: &[String],
    conflict_resolution: MemoryConflictResolution,
) -> Result<PromptMemoryInjection> {
    let latest_user_index = latest_user_message_index(history);
    let empty = |reason: Option<MemoryDecisionReason>| PromptMemoryInjection {
        system_prompt_suffix: None,
        inserted_messages: Vec::new(),
        summary: PersistedPromptMemoryState {
            mode,
            ..PersistedPromptMemoryState::default()
        },
        latest_user_index,
        diagnostics: PromptMemoryRunDiagnostics {
            mode,
            injection_ran: false,
            status: match reason {
                Some(MemoryDecisionReason::Disabled) => MemoryStatus::Skipped,
                Some(_) => MemoryStatus::Skipped,
                None => MemoryStatus::NotRun,
            },
            reason,
            selected_entries: Vec::new(),
            skipped_entries: Vec::new(),
            truncated: false,
            conflict_winner_scope: None,
        },
    };

    match mode {
        PromptMemoryMode::Disabled => Ok(empty(Some(MemoryDecisionReason::Disabled))),
        PromptMemoryMode::IndexOnly => {
            let Some(memory_md) = load_memory_md(readable_scopes).await? else {
                return Ok(empty(Some(MemoryDecisionReason::NoIndex)));
            };
            let (truncated, body) = truncate_chars(&memory_md, INDEX_ONLY_CHAR_BUDGET);
            Ok(PromptMemoryInjection {
                system_prompt_suffix: Some(format!(
                    "# Durable Memory Index\n\n{body}\n\n{STALE_CAVEAT}"
                )),
                inserted_messages: Vec::new(),
                summary: PersistedPromptMemoryState {
                    mode,
                    truncated,
                    ..PersistedPromptMemoryState::default()
                },
                latest_user_index,
                diagnostics: PromptMemoryRunDiagnostics {
                    mode,
                    injection_ran: true,
                    status: MemoryStatus::Succeeded,
                    reason: None,
                    selected_entries: Vec::new(),
                    skipped_entries: Vec::new(),
                    truncated,
                    conflict_winner_scope: None,
                },
            })
        }
        PromptMemoryMode::TargetedRecall => {
            let Some(query) = latest_user_text(history) else {
                return Ok(empty(Some(MemoryDecisionReason::NoQuery)));
            };
            let scoped_entries = load_memory_index_entries(readable_scopes).await?;
            if scoped_entries.is_empty() {
                return Ok(empty(Some(MemoryDecisionReason::NoIndex)));
            }
            let normalized_query = tokenize(&query);
            if normalized_query.is_empty() {
                return Ok(empty(Some(MemoryDecisionReason::NoQuery)));
            }

            let exclude: HashSet<&str> = previously_selected_entry_ids
                .iter()
                .map(String::as_str)
                .collect();
            let (resolved_entries, conflict_winner_scope) =
                resolve_conflicts(scoped_entries, conflict_resolution);
            let mut candidates: Vec<_> = resolved_entries
                .into_iter()
                .map(|entry| {
                    if exclude.contains(entry.entry.entry_id.as_str()) {
                        return (entry, 0usize, Some(MemoryDecisionReason::Duplicate));
                    }
                    let overlap = candidate_overlap_score(&normalized_query, &entry.entry);
                    if overlap > 0 {
                        (entry, overlap, None)
                    } else {
                        (
                            entry,
                            overlap,
                            Some(MemoryDecisionReason::NoMatchingEntries),
                        )
                    }
                })
                .collect();
            candidates.sort_by(|(left, left_overlap, _), (right, right_overlap, _)| {
                right_overlap
                    .cmp(left_overlap)
                    .then_with(|| right.entry.pinned.cmp(&left.entry.pinned))
                    .then_with(|| right.entry.updated_at.cmp(&left.entry.updated_at))
                    .then_with(|| left.entry.entry_id.cmp(&right.entry.entry_id))
            });

            let mut total_chars = 0usize;
            let mut inserted_messages = Vec::new();
            let mut selected_entry_ids = Vec::new();
            let mut selected_titles = Vec::new();
            let mut skipped_reasons = Vec::new();
            let mut selected_entries = Vec::new();
            let mut skipped_entries = Vec::new();
            let mut truncated = false;

            for (entry, overlap, preset_reason) in candidates {
                if let Some(reason) = preset_reason {
                    skipped_reasons.push(format!(
                        "{}:{}",
                        entry.entry.entry_id,
                        decision_reason_code(reason)
                    ));
                    skipped_entries.push(MemorySkippedEntryDiagnostics {
                        entry_id: entry.entry.entry_id,
                        reason,
                    });
                    continue;
                }
                if overlap == 0 {
                    continue;
                }
                if selected_entry_ids.len() >= TARGETED_MAX_ENTRIES {
                    skipped_reasons.push(format!("{}:budget", entry.entry.entry_id));
                    skipped_entries.push(MemorySkippedEntryDiagnostics {
                        entry_id: entry.entry.entry_id,
                        reason: MemoryDecisionReason::Budget,
                    });
                    continue;
                }
                let record = load_record(&entry.root, &entry.entry.path).await?;
                let (entry_truncated, body) =
                    truncate_chars(&record.body, TARGETED_ENTRY_CHAR_BUDGET);
                let reminder = format!(
                    "Relevant durable memory `{}`:\n{}\n\n{}",
                    record.frontmatter.entry_id, body, STALE_CAVEAT
                );
                let reminder_chars = reminder.chars().count();
                if total_chars + reminder_chars > TARGETED_TOTAL_CHAR_BUDGET {
                    skipped_reasons.push(format!("{}:budget", entry.entry.entry_id));
                    skipped_entries.push(MemorySkippedEntryDiagnostics {
                        entry_id: entry.entry.entry_id,
                        reason: MemoryDecisionReason::Budget,
                    });
                    continue;
                }
                total_chars += reminder_chars;
                truncated |= entry_truncated;
                selected_entry_ids.push(record.frontmatter.entry_id.clone());
                selected_titles.push(record.frontmatter.title.clone());
                selected_entries.push(MemorySelectionEntryDiagnostics {
                    entry_id: record.frontmatter.entry_id.clone(),
                    title: record.frontmatter.title.clone(),
                    path: entry.entry.path.clone(),
                });
                inserted_messages.push(Message {
                    role: Role::System,
                    content: quine_llm::MessageContent::Text(reminder),
                });
            }

            let status = if selected_entry_ids.is_empty() {
                MemoryStatus::Skipped
            } else {
                MemoryStatus::Succeeded
            };
            let reason = if selected_entry_ids.is_empty() {
                Some(MemoryDecisionReason::NoMatchingEntries)
            } else {
                None
            };

            Ok(PromptMemoryInjection {
                system_prompt_suffix: None,
                inserted_messages,
                summary: PersistedPromptMemoryState {
                    mode,
                    selected_entry_ids,
                    selected_titles,
                    skipped_reasons,
                    truncated,
                },
                latest_user_index,
                diagnostics: PromptMemoryRunDiagnostics {
                    mode,
                    injection_ran: true,
                    status,
                    reason,
                    selected_entries,
                    skipped_entries,
                    truncated,
                    conflict_winner_scope,
                },
            })
        }
    }
}

fn latest_user_message_index(history: &[Message]) -> Option<usize> {
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(index, message)| (message.role == Role::User).then_some(index))
}

fn latest_user_text(history: &[Message]) -> Option<String> {
    latest_user_message_index(history).and_then(|index| match &history[index].content {
        quine_llm::MessageContent::Text(text) => Some(text.clone()),
        _ => None,
    })
}

fn candidate_overlap_score(query_tokens: &[String], entry: &PersistentMemoryIndexEntry) -> usize {
    let mut haystack = vec![
        entry.title.clone(),
        entry.summary.clone(),
        entry.slug.clone(),
    ];
    haystack.extend(entry.keywords.clone());
    let haystack_tokens: HashSet<String> = haystack
        .into_iter()
        .flat_map(|text| tokenize(&text))
        .collect();
    query_tokens
        .iter()
        .filter(|token| haystack_tokens.contains(*token))
        .count()
}

fn tokenize(text: &str) -> Vec<String> {
    text.split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| token.len() >= 3)
        .map(|token| token.to_ascii_lowercase())
        .collect()
}

fn truncate_chars(text: &str, max_chars: usize) -> (bool, String) {
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return (false, text.to_string());
    }
    let clipped = text
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    (true, format!("{clipped}…"))
}

async fn load_memory_md(readable_scopes: &[ScopedMemoryPaths]) -> Result<Option<String>> {
    let mut sections = Vec::new();
    for scope in readable_scopes {
        match tokio::fs::read_to_string(&scope.index_markdown_path).await {
            Ok(content) => sections.push(format!(
                "## {} Scope\n\n{}",
                scope.scope.label(),
                content.trim()
            )),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    if sections.is_empty() {
        Ok(None)
    } else {
        Ok(Some(sections.join("\n\n")))
    }
}

async fn load_memory_index_entries(
    readable_scopes: &[ScopedMemoryPaths],
) -> Result<Vec<ScopedPersistentMemoryIndexEntry>> {
    let mut entries = Vec::new();
    for scope in readable_scopes {
        let raw = match tokio::fs::read_to_string(&scope.index_json_path).await {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        let index: PersistentMemoryIndex = serde_json::from_str(&raw)?;
        entries.extend(
            index
                .entries
                .into_iter()
                .map(|entry| ScopedPersistentMemoryIndexEntry {
                    scope: scope.scope.clone(),
                    root: scope.root.clone(),
                    entry,
                }),
        );
    }
    Ok(entries)
}

async fn load_record(base_dir: &Path, relative_path: &str) -> Result<PersistentMemoryRecord> {
    let path = base_dir.join(relative_path);
    let content = tokio::fs::read_to_string(&path).await?;
    parse_record(&content)
        .with_context(|| format!("failed to parse persistent memory record {:?}", path))
}

fn parse_record(content: &str) -> Result<PersistentMemoryRecord> {
    let normalized = content.replace("\r\n", "\n");
    let trimmed = normalized.trim_start_matches('\u{feff}').trim_start();
    let Some(rest) = trimmed.strip_prefix("---\n") else {
        anyhow::bail!("missing frontmatter delimiter");
    };
    let Some((frontmatter, body)) = rest.split_once("\n---\n") else {
        anyhow::bail!("missing closing frontmatter delimiter");
    };
    let frontmatter: PersistentMemoryFrontmatter = serde_yaml::from_str(frontmatter)?;
    Ok(PersistentMemoryRecord {
        frontmatter,
        body: body.trim().to_string(),
    })
}

fn decision_reason_code(reason: MemoryDecisionReason) -> &'static str {
    match reason {
        MemoryDecisionReason::Disabled => "disabled",
        MemoryDecisionReason::NoActivityYet => "no_activity_yet",
        MemoryDecisionReason::NoNewMessages => "no_new_messages",
        MemoryDecisionReason::NoChanges => "no_changes",
        MemoryDecisionReason::NoQuery => "no_query",
        MemoryDecisionReason::NoIndex => "no_index",
        MemoryDecisionReason::NoMatchingEntries => "no_matching_entries",
        MemoryDecisionReason::Duplicate => "duplicate",
        MemoryDecisionReason::Budget => "budget",
        MemoryDecisionReason::RefreshNotNeeded => "refresh_not_needed",
        MemoryDecisionReason::MissingSummary => "missing_summary",
        MemoryDecisionReason::InvalidBoundary => "invalid_boundary",
        MemoryDecisionReason::Fallback => "fallback",
        MemoryDecisionReason::NotAttempted => "not_attempted",
    }
}

pub(crate) fn project_root_for_prompt_memory(working_directory: &Path) -> PathBuf {
    super::resolve_project_root(working_directory)
}

fn resolve_conflicts(
    entries: Vec<ScopedPersistentMemoryIndexEntry>,
    strategy: MemoryConflictResolution,
) -> (
    Vec<ScopedPersistentMemoryIndexEntry>,
    Option<PersistentMemoryScope>,
) {
    let mut grouped: std::collections::BTreeMap<String, Vec<ScopedPersistentMemoryIndexEntry>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        grouped
            .entry(entry.entry.entry_id.clone())
            .or_default()
            .push(entry);
    }

    let mut resolved = Vec::new();
    let mut conflict_winner_scope = None;
    for mut group in grouped.into_values() {
        if group.len() == 1 {
            resolved.push(group.remove(0));
            continue;
        }
        group.sort_by(|left, right| {
            if matches!(
                strategy,
                MemoryConflictResolution::PreferMostRecentlyUpdated
                    | MemoryConflictResolution::ErrorOnConflictingWrites
            ) {
                right
                    .entry
                    .updated_at
                    .cmp(&left.entry.updated_at)
                    .then_with(|| compare_scope_priority(&left.scope, &right.scope, strategy))
            } else {
                compare_scope_priority(&left.scope, &right.scope, strategy)
            }
        });
        if matches!(strategy, MemoryConflictResolution::ErrorOnConflictingWrites) {
            continue;
        }
        if conflict_winner_scope.is_none() {
            conflict_winner_scope = Some(group[0].scope.clone());
        }
        resolved.push(group.remove(0));
    }
    (resolved, conflict_winner_scope)
}

pub(crate) fn splice_prompt_memory_messages(
    history: &[Message],
    injection: &PromptMemoryInjection,
) -> Vec<Message> {
    if injection.inserted_messages.is_empty() {
        return history.to_vec();
    }
    let Some(user_index) = injection.latest_user_index else {
        return history.to_vec();
    };
    let mut messages = Vec::with_capacity(history.len() + injection.inserted_messages.len());
    messages.extend_from_slice(&history[..user_index]);
    messages.extend(injection.inserted_messages.iter().cloned());
    messages.extend_from_slice(&history[user_index..]);
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_memory_targeted_recall_ranks_by_overlap_recency_and_pin() {
        let newer = Utc::now();
        let older = newer - chrono::TimeDelta::days(1);
        let query = tokenize("What terminal command should I use to run the Rust test suite?");
        let a = PersistentMemoryIndexEntry {
            entry_id: "rust-test-command".into(),
            title: "Run Rust tests".into(),
            summary: "Use cargo test to run the Rust test suite".into(),
            slug: "rust-test-command".into(),
            path: "entries/rust-test-command.md".into(),
            updated_at: older,
            keywords: vec!["cargo".into(), "test".into(), "rust".into()],
            pinned: false,
        };
        let b = PersistentMemoryIndexEntry {
            entry_id: "recent-but-weaker".into(),
            title: "Build workspace".into(),
            summary: "Use cargo build".into(),
            slug: "recent-but-weaker".into(),
            path: "entries/recent-but-weaker.md".into(),
            updated_at: newer,
            keywords: vec!["build".into()],
            pinned: false,
        };
        let c = PersistentMemoryIndexEntry {
            entry_id: "pinned-test".into(),
            title: "Rust test command".into(),
            summary: "cargo test is the command".into(),
            slug: "pinned-test".into(),
            path: "entries/pinned-test.md".into(),
            updated_at: newer,
            keywords: vec!["test".into(), "rust".into()],
            pinned: true,
        };

        let mut candidates = [
            (a.clone(), candidate_overlap_score(&query, &a)),
            (b.clone(), candidate_overlap_score(&query, &b)),
            (c.clone(), candidate_overlap_score(&query, &c)),
        ];
        candidates.sort_by(|(left, left_overlap), (right, right_overlap)| {
            right_overlap
                .cmp(left_overlap)
                .then_with(|| right.pinned.cmp(&left.pinned))
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| left.entry_id.cmp(&right.entry_id))
        });

        assert_eq!(candidates[0].0.entry_id, "rust-test-command");
        assert_eq!(candidates[1].0.entry_id, "pinned-test");
        assert_eq!(candidates[2].0.entry_id, "recent-but-weaker");
    }

    #[test]
    fn prompt_memory_targeted_recall_excludes_memory_md_and_unmatched_entries() {
        let query = tokenize("run the rust test suite");
        let unrelated = PersistentMemoryIndexEntry {
            entry_id: "editor-preference".into(),
            title: "Editor preference".into(),
            summary: "Use vim".into(),
            slug: "editor-preference".into(),
            path: "entries/editor-preference.md".into(),
            updated_at: Utc::now(),
            keywords: vec!["editor".into()],
            pinned: false,
        };
        assert_eq!(candidate_overlap_score(&query, &unrelated), 0);
    }

    #[test]
    fn prompt_memory_index_only_truncates_memory_md_by_budget() {
        let long = "x".repeat(INDEX_ONLY_CHAR_BUDGET + 10);
        let (truncated, clipped) = truncate_chars(&long, INDEX_ONLY_CHAR_BUDGET);
        assert!(truncated);
        assert!(clipped.ends_with('…'));
        assert!(clipped.chars().count() <= INDEX_ONLY_CHAR_BUDGET);
    }
}
