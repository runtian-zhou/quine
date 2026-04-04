use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use quine_llm::{Message, MessageContent, Role};

use super::session::{next_unsummarized_message_index, SessionMemoryPaths, SessionMemoryState};
use super::template::SessionSummaryDocument;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionSummaryMetadata {
    pub(crate) last_summarized_message_index: usize,
    pub(crate) updated_at: DateTime<Utc>,
    pub(crate) template_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionSummaryUpdate {
    pub(crate) from_message_index: usize,
    pub(crate) to_message_index: usize,
    pub(crate) document: SessionSummaryDocument,
    pub(crate) metadata: SessionSummaryMetadata,
}

pub(crate) fn initialize_summary_if_missing(paths: &SessionMemoryPaths) -> Result<()> {
    if !paths.directory.exists() {
        std::fs::create_dir_all(&paths.directory).with_context(|| {
            format!(
                "failed to create session memory directory {:?}",
                paths.directory
            )
        })?;
    }

    if !paths.summary_path.exists() {
        std::fs::write(
            &paths.summary_path,
            SessionSummaryDocument::empty().render_markdown(),
        )
        .with_context(|| format!("failed to initialize summary file {:?}", paths.summary_path))?;
    }

    Ok(())
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn load_summary_metadata(path: &Path) -> Result<SessionSummaryMetadata> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read summary metadata {:?}", path))?;
    let metadata: SessionSummaryMetadata = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse summary metadata {:?}", path))?;
    Ok(metadata)
}

pub(crate) fn store_summary_metadata(path: &Path, metadata: &SessionSummaryMetadata) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create summary metadata parent {:?}", parent))?;
    }
    let content = serde_json::to_string_pretty(metadata)?;
    std::fs::write(path, content)
        .with_context(|| format!("failed to write summary metadata {:?}", path))?;
    Ok(())
}

pub(crate) fn should_refresh_summary(state: &SessionMemoryState, history: &[Message]) -> bool {
    if !state.enabled || state.refresh_in_flight || history.is_empty() {
        return false;
    }
    history.len().saturating_sub(1) > state.last_summarized_message_index.unwrap_or(usize::MAX)
        || state.last_summarized_message_index.is_none()
}

pub(crate) fn build_summary_update(
    state: &SessionMemoryState,
    history: &[Message],
) -> Option<SessionSummaryUpdate> {
    if history.is_empty() {
        return None;
    }
    let from = next_unsummarized_message_index(state.last_summarized_message_index);
    let to = history.len().checked_sub(1)?;
    if from > to {
        return None;
    }

    // Keep session memory cumulative across refreshes so compaction can
    // preserve continuity for the whole summarized prefix, not just the
    // most recent incremental slice.
    let document = summarize_messages(&history[..=to]);
    let metadata = SessionSummaryMetadata {
        last_summarized_message_index: to,
        updated_at: Utc::now(),
        template_version: state.template_version,
    };

    Some(SessionSummaryUpdate {
        from_message_index: from,
        to_message_index: to,
        document,
        metadata,
    })
}

pub(crate) fn refresh_summary_from_history(
    state: &SessionMemoryState,
    history: &[Message],
) -> Result<Option<SessionSummaryUpdate>> {
    initialize_summary_if_missing(&state.paths)?;

    let Some(update) = build_summary_update(state, history) else {
        return Ok(None);
    };

    std::fs::write(&state.paths.summary_path, update.document.render_markdown()).with_context(
        || {
            format!(
                "failed to write session summary markdown {:?}",
                state.paths.summary_path
            )
        },
    )?;
    store_summary_metadata(&state.paths.metadata_path, &update.metadata)?;
    Ok(Some(update))
}

fn summarize_messages(messages: &[Message]) -> SessionSummaryDocument {
    let mut current_state = Vec::new();
    let mut task_specification = Vec::new();
    let mut files_and_functions = BTreeSet::new();
    let mut workflow = Vec::new();
    let mut errors_and_corrections = Vec::new();
    let mut codebase_and_system_documentation = Vec::new();
    let mut learnings = Vec::new();
    let mut key_results = Vec::new();
    let mut worklog = Vec::new();

    for message in messages {
        match (&message.role, &message.content) {
            (Role::User, MessageContent::Text(text)) => {
                task_specification.push(trimmed_line(text));
                worklog.push(format!("User: {}", trimmed_line(text)));
            }
            (Role::Assistant, MessageContent::Text(text)) => {
                current_state.push(trimmed_line(text));
                workflow.push(trimmed_line(text));
                key_results.push(trimmed_line(text));
                worklog.push(format!("Assistant: {}", trimmed_line(text)));
                collect_paths(text, &mut files_and_functions);
            }
            (Role::Assistant, MessageContent::ToolUse { text, tool_calls }) => {
                if let Some(text) = text {
                    workflow.push(trimmed_line(text));
                    worklog.push(format!("Assistant planned tools: {}", trimmed_line(text)));
                    collect_paths(text, &mut files_and_functions);
                }
                for call in tool_calls {
                    workflow.push(format!("Tool requested: {}", call.tool_name));
                    worklog.push(format!("Tool requested: {}", call.tool_name));
                }
            }
            (
                Role::Tool,
                MessageContent::ToolResult {
                    output, is_error, ..
                },
            ) => {
                if *is_error {
                    errors_and_corrections.push(trimmed_line(output));
                } else {
                    learnings.push(trimmed_line(output));
                }
                worklog.push(format!("Tool result: {}", trimmed_line(output)));
                collect_paths(output, &mut files_and_functions);
            }
            (Role::System, MessageContent::Text(text)) => {
                codebase_and_system_documentation.push(trimmed_line(text));
            }
            _ => {}
        }
    }

    SessionSummaryDocument {
        current_state: non_empty_or_default(current_state, "No current state captured yet."),
        task_specification: non_empty_or_default(
            task_specification,
            "No task specification captured yet.",
        ),
        files_and_functions: non_empty_or_default(
            files_and_functions.into_iter().collect(),
            "No files or functions captured yet.",
        ),
        workflow: non_empty_or_default(workflow, "No workflow notes captured yet."),
        errors_and_corrections: non_empty_or_default(
            errors_and_corrections,
            "No errors or corrections captured yet.",
        ),
        codebase_and_system_documentation: non_empty_or_default(
            codebase_and_system_documentation,
            "No codebase notes captured yet.",
        ),
        learnings: non_empty_or_default(learnings, "No learnings captured yet."),
        key_results: non_empty_or_default(key_results, "No key results captured yet."),
        worklog: non_empty_or_default(worklog, "No worklog entries captured yet."),
    }
}

fn non_empty_or_default(values: Vec<String>, fallback: &str) -> Vec<String> {
    if values.is_empty() {
        vec![fallback.into()]
    } else {
        values
    }
}

fn trimmed_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text.trim())
        .chars()
        .take(200)
        .collect()
}

fn collect_paths(text: &str, results: &mut BTreeSet<String>) {
    for token in text.split_whitespace() {
        let trimmed = token.trim_matches(|c: char| "`'\",()[]{}".contains(c));
        if trimmed.contains('/') || trimmed.ends_with(".rs") || trimmed.ends_with(".md") {
            results.insert(trimmed.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_summary_metadata, refresh_summary_from_history, should_refresh_summary,
        store_summary_metadata, SessionSummaryMetadata,
    };
    use crate::memory::session::{
        session_memory_paths, SessionMemoryState, SESSION_MEMORY_TEMPLATE_VERSION,
    };
    use crate::memory::summary::build_summary_update;
    use crate::session::SessionId;
    use chrono::Utc;
    use quine_llm::Message;
    use tempfile::TempDir;

    #[test]
    fn metadata_round_trips_through_json_sidecar() {
        let temp = TempDir::new().unwrap();
        let metadata_path = temp.path().join("summary.meta.json");
        let metadata = SessionSummaryMetadata {
            last_summarized_message_index: 7,
            updated_at: Utc::now(),
            template_version: 1,
        };
        store_summary_metadata(&metadata_path, &metadata).unwrap();
        let loaded = load_summary_metadata(&metadata_path).unwrap();
        assert_eq!(loaded.last_summarized_message_index, 7);
        assert_eq!(loaded.template_version, 1);
    }

    #[test]
    fn malformed_metadata_is_rejected() {
        let temp = TempDir::new().unwrap();
        let metadata_path = temp.path().join("summary.meta.json");
        std::fs::write(&metadata_path, "{not json}").unwrap();
        assert!(load_summary_metadata(&metadata_path).is_err());
    }

    #[test]
    fn refresh_decision_respects_disabled_inflight_and_boundary_rules() {
        let state = SessionMemoryState {
            enabled: true,
            paths: session_memory_paths(std::env::temp_dir().as_path(), SessionId::new()),
            refresh_in_flight: false,
            last_summarized_message_index: Some(1),
            last_refresh_at: None,
            template_version: SESSION_MEMORY_TEMPLATE_VERSION,
            refresh_handle: Default::default(),
            persistent_enabled: true,
            last_persistent_extracted_message_index: None,
        };
        let history = vec![Message::user("a"), Message::assistant("b")];
        assert!(!should_refresh_summary(&state, &history));

        let mut disabled = state.clone();
        disabled.enabled = false;
        assert!(!should_refresh_summary(&disabled, &history));

        let mut inflight = state.clone();
        inflight.refresh_in_flight = true;
        assert!(!should_refresh_summary(&inflight, &history));

        let mut stale = state.clone();
        stale.last_summarized_message_index = Some(0);
        assert!(should_refresh_summary(&stale, &history));
    }

    #[test]
    fn build_update_and_refresh_write_summary_files() {
        let temp = TempDir::new().unwrap();
        let paths = session_memory_paths(temp.path(), SessionId::new());
        let state = SessionMemoryState {
            enabled: true,
            paths: paths.clone(),
            refresh_in_flight: false,
            last_summarized_message_index: None,
            last_refresh_at: None,
            template_version: SESSION_MEMORY_TEMPLATE_VERSION,
            refresh_handle: Default::default(),
            persistent_enabled: true,
            last_persistent_extracted_message_index: None,
        };
        let history = vec![
            Message::user("inspect crates/quine-core/src/engine.rs"),
            Message::assistant("Updated crates/quine-core/src/engine.rs"),
        ];
        let update = build_summary_update(&state, &history).unwrap();
        assert_eq!(update.from_message_index, 0);
        assert_eq!(update.to_message_index, 1);
        assert!(update
            .document
            .render_markdown()
            .contains("crates/quine-core/src/engine.rs"));

        let written = refresh_summary_from_history(&state, &history)
            .unwrap()
            .unwrap();
        assert_eq!(written.to_message_index, 1);
        assert!(paths.summary_path.exists());
        assert!(paths.metadata_path.exists());
    }

    #[test]
    fn refresh_keeps_earlier_continuity_across_multiple_updates() {
        let temp = TempDir::new().unwrap();
        let paths = session_memory_paths(temp.path(), SessionId::new());
        let mut state = SessionMemoryState {
            enabled: true,
            paths: paths.clone(),
            refresh_in_flight: false,
            last_summarized_message_index: None,
            last_refresh_at: None,
            template_version: SESSION_MEMORY_TEMPLATE_VERSION,
            refresh_handle: Default::default(),
            persistent_enabled: true,
            last_persistent_extracted_message_index: None,
        };

        let first_history = vec![
            Message::user("remember fact one"),
            Message::assistant("ACK ROUND 1"),
        ];
        let first = refresh_summary_from_history(&state, &first_history)
            .unwrap()
            .unwrap();
        state.last_summarized_message_index = Some(first.metadata.last_summarized_message_index);

        let second_history = vec![
            Message::user("remember fact one"),
            Message::assistant("ACK ROUND 1"),
            Message::user("remember fact two"),
            Message::assistant("ACK ROUND 2"),
        ];
        let second = refresh_summary_from_history(&state, &second_history)
            .unwrap()
            .unwrap();
        let markdown = second.document.render_markdown();

        assert!(markdown.contains("remember fact one"));
        assert!(markdown.contains("remember fact two"));
        assert!(markdown.contains("ACK ROUND 1"));
        assert!(markdown.contains("ACK ROUND 2"));
    }
}
