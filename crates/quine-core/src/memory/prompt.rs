use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use quine_llm::{Message, Role};
use serde::Deserialize;
use sha2::{Digest, Sha256};

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

#[derive(Debug, Clone)]
pub(crate) struct PromptMemoryInjection {
    pub(crate) system_prompt_suffix: Option<String>,
    pub(crate) inserted_messages: Vec<Message>,
    pub(crate) summary: PersistedPromptMemoryState,
    pub(crate) latest_user_index: Option<usize>,
}

pub(crate) async fn build_prompt_memory_injection(
    archive_root: &Path,
    working_directory: &Path,
    mode: PromptMemoryMode,
    history: &[Message],
    previously_selected_entry_ids: &[String],
) -> Result<PromptMemoryInjection> {
    let latest_user_index = latest_user_message_index(history);
    let empty = PromptMemoryInjection {
        system_prompt_suffix: None,
        inserted_messages: Vec::new(),
        summary: PersistedPromptMemoryState {
            mode,
            ..PersistedPromptMemoryState::default()
        },
        latest_user_index,
    };

    match mode {
        PromptMemoryMode::Disabled => Ok(empty),
        PromptMemoryMode::IndexOnly => {
            let Some(memory_md) = load_memory_md(archive_root, working_directory).await? else {
                return Ok(empty);
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
            })
        }
        PromptMemoryMode::TargetedRecall => {
            let Some(query) = latest_user_text(history) else {
                return Ok(empty);
            };
            let Some(index) = load_memory_index(archive_root, working_directory).await? else {
                return Ok(empty);
            };
            let normalized_query = tokenize(&query);
            if normalized_query.is_empty() {
                return Ok(empty);
            }

            let exclude: HashSet<&str> = previously_selected_entry_ids
                .iter()
                .map(String::as_str)
                .collect();
            let mut candidates: Vec<_> = index
                .entries
                .into_iter()
                .filter(|entry| !exclude.contains(entry.entry_id.as_str()))
                .filter_map(|entry| {
                    let overlap = candidate_overlap_score(&normalized_query, &entry);
                    (overlap > 0).then_some((entry, overlap))
                })
                .collect();
            candidates.sort_by(|(left, left_overlap), (right, right_overlap)| {
                right_overlap
                    .cmp(left_overlap)
                    .then_with(|| right.pinned.cmp(&left.pinned))
                    .then_with(|| right.updated_at.cmp(&left.updated_at))
                    .then_with(|| left.entry_id.cmp(&right.entry_id))
            });

            let base_dir = memory_project_root(archive_root, working_directory);
            let mut total_chars = 0usize;
            let mut inserted_messages = Vec::new();
            let mut selected_entry_ids = Vec::new();
            let mut selected_titles = Vec::new();
            let mut skipped_reasons = Vec::new();
            let mut truncated = false;

            for (entry, _) in candidates {
                if selected_entry_ids.len() >= TARGETED_MAX_ENTRIES {
                    skipped_reasons.push(format!("{}:budget", entry.entry_id));
                    continue;
                }
                let record = load_record(&base_dir, &entry.path).await?;
                let (entry_truncated, body) =
                    truncate_chars(&record.body, TARGETED_ENTRY_CHAR_BUDGET);
                let reminder = format!(
                    "Relevant durable memory `{}`:\n{}\n\n{}",
                    record.frontmatter.entry_id, body, STALE_CAVEAT
                );
                let reminder_chars = reminder.chars().count();
                if total_chars + reminder_chars > TARGETED_TOTAL_CHAR_BUDGET {
                    skipped_reasons.push(format!("{}:budget", entry.entry_id));
                    continue;
                }
                total_chars += reminder_chars;
                truncated |= entry_truncated;
                selected_entry_ids.push(record.frontmatter.entry_id.clone());
                selected_titles.push(record.frontmatter.title.clone());
                inserted_messages.push(Message {
                    role: Role::System,
                    content: quine_llm::MessageContent::Text(reminder),
                });
            }

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

async fn load_memory_md(archive_root: &Path, working_directory: &Path) -> Result<Option<String>> {
    let path = memory_project_root(archive_root, working_directory).join("MEMORY.md");
    match tokio::fs::read_to_string(path).await {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn load_memory_index(
    archive_root: &Path,
    working_directory: &Path,
) -> Result<Option<PersistentMemoryIndex>> {
    let path = memory_project_root(archive_root, working_directory).join("index.json");
    let raw = match tokio::fs::read_to_string(path).await {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    Ok(Some(serde_json::from_str(&raw)?))
}

async fn load_record(base_dir: &Path, relative_path: &str) -> Result<PersistentMemoryRecord> {
    let path = base_dir.join(relative_path);
    let content = tokio::fs::read_to_string(&path).await?;
    parse_record(&content)
        .with_context(|| format!("failed to parse persistent memory record {:?}", path))
}

fn parse_record(content: &str) -> Result<PersistentMemoryRecord> {
    let trimmed = content.trim_start();
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

fn memory_project_root(archive_root: &Path, working_directory: &Path) -> PathBuf {
    archive_root
        .join("memory")
        .join("projects")
        .join(project_key(&resolve_project_root(working_directory)))
}

fn resolve_project_root(working_directory: &Path) -> PathBuf {
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

fn project_key(project_root: &Path) -> String {
    let normalized = project_root.to_string_lossy().replace('\\', "/");
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
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
