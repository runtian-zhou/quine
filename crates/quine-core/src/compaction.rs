use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chrono::Utc;
use quine_llm::{Message, MessageContent, Role};
use serde::Serialize;
use tokio::fs;
use tokio::time::Duration;

use crate::memory::{
    load_compaction_snapshot, SessionMemoryCompactionSnapshot, SessionMemoryState,
};

pub const AUTO_COMPACT_THRESHOLD_NUMERATOR: u64 = 3;
pub const AUTO_COMPACT_THRESHOLD_DENOMINATOR: u64 = 5;
pub const MAX_TOOL_RESULT_CHARS_IN_HISTORY: usize = 256_000;
const TOOL_RESULT_PREVIEW_HEAD_CHARS: usize = 8_000;
const TOOL_RESULT_PREVIEW_TAIL_CHARS: usize = 2_000;
const INITIAL_TOOL_RESULT_PREVIEW_LINES: usize = 12;
const SESSION_MEMORY_REFRESH_WAIT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionTrigger {
    Auto,
    Manual,
}

impl CompactionTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CompactionSource {
    SessionMemory,
    LegacySummarizer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CompactionPlan {
    pub(crate) source: CompactionSource,
    pub(crate) summary: String,
    pub(crate) tail_start: usize,
}

#[derive(Debug, Clone)]
pub struct ArchivedTranscript {
    pub generation: u64,
    pub path: PathBuf,
}

#[derive(Debug, Serialize)]
struct TranscriptArchive {
    session_id: String,
    generation: u64,
    trigger: String,
    archived_at: String,
    history: Vec<Message>,
}

pub fn auto_compact_threshold(max_context_window: Option<u64>) -> Option<u64> {
    max_context_window.map(|window| {
        window.saturating_mul(AUTO_COMPACT_THRESHOLD_NUMERATOR) / AUTO_COMPACT_THRESHOLD_DENOMINATOR
    })
}

pub fn should_auto_compact(
    max_context_window: Option<u64>,
    last_input_tokens: Option<u64>,
) -> bool {
    match (
        auto_compact_threshold(max_context_window),
        last_input_tokens,
    ) {
        (Some(threshold), Some(tokens)) => tokens >= threshold,
        _ => false,
    }
}

pub fn build_micro_compacted_history(history: &[Message]) -> Vec<Message> {
    let preserve_from = live_tail_start(history).unwrap_or(history.len());
    let tool_names = tool_name_map(history);

    history
        .iter()
        .enumerate()
        .map(|(index, message)| match &message.content {
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } if index < preserve_from => Message {
                role: message.role.clone(),
                content: MessageContent::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    output: render_tool_placeholder(
                        tool_names.get(tool_use_id).map(String::as_str),
                        tool_use_id,
                        *is_error,
                        output.len(),
                    ),
                    is_error: *is_error,
                },
            },
            _ => message.clone(),
        })
        .collect()
}

pub fn split_history_for_compaction(history: &[Message]) -> (Vec<Message>, Vec<Message>) {
    let tail_start = live_tail_start(history).unwrap_or(history.len());
    (
        history[..tail_start].to_vec(),
        history[tail_start..].to_vec(),
    )
}

pub fn summarizer_messages(
    archive_ref: &str,
    trigger: CompactionTrigger,
    history: &[Message],
) -> Vec<Message> {
    let transcript = render_transcript(history);
    vec![
        Message::system(
            "You summarize archived coding-agent conversations for future continuation. \
             Return plain text with these sections exactly: Current Goal, Constraints, Tool Findings, \
             Open Threads, Latest State. Be concrete and concise. Do not invent facts.",
        ),
        Message::user(format!(
            "Archive reference: {archive_ref}\n\
             Trigger: {}\n\
             Summarize the transcript below so another LLM call can continue the session without \
             needing the full transcript.\n\n{}",
            trigger.as_str(),
            transcript
        )),
    ]
}

pub fn compacted_history(
    history: &[Message],
    summary: &str,
    archive_ref: &str,
    tail: &[Message],
) -> Vec<Message> {
    let mut compacted = Vec::new();
    if let Some(system) = history
        .first()
        .filter(|message| message.role == Role::System)
    {
        compacted.push(system.clone());
    }
    compacted.push(Message::assistant(format!(
        "Context compacted from archive `{archive_ref}`.\n\n{summary}"
    )));
    compacted.extend(tail.iter().cloned());
    compacted
}

pub(crate) async fn session_memory_compaction_plan(
    state: &SessionMemoryState,
    history: &[Message],
) -> Option<CompactionPlan> {
    let snapshot = load_compaction_snapshot(state, SESSION_MEMORY_REFRESH_WAIT)
        .await
        .ok()
        .flatten()?;
    compaction_plan_from_snapshot(state, history, snapshot)
}

pub(crate) fn legacy_compaction_plan(history: &[Message], summary: String) -> CompactionPlan {
    CompactionPlan {
        source: CompactionSource::LegacySummarizer,
        summary,
        tail_start: live_tail_start(history).unwrap_or(history.len()),
    }
}

pub(crate) fn apply_compaction_plan(
    history: &[Message],
    archive_ref: &str,
    plan: &CompactionPlan,
) -> Vec<Message> {
    compacted_history(
        history,
        &plan.summary,
        archive_ref,
        &history[plan.tail_start..],
    )
}

pub async fn archive_history(
    archive_root: &Path,
    session_id: &str,
    generation: u64,
    trigger: CompactionTrigger,
    history: &[Message],
) -> anyhow::Result<ArchivedTranscript> {
    let session_dir = archive_root.join("compactions").join(session_id);
    fs::create_dir_all(&session_dir).await?;
    let path = session_dir.join(format!("{generation:04}.json"));
    let archive = TranscriptArchive {
        session_id: session_id.to_string(),
        generation,
        trigger: trigger.as_str().to_string(),
        archived_at: Utc::now().to_rfc3339(),
        history: history.to_vec(),
    };
    let payload = serde_json::to_vec_pretty(&archive)?;
    fs::write(&path, payload).await?;
    Ok(ArchivedTranscript { generation, path })
}

pub async fn archive_tool_result(
    archive_root: &Path,
    session_id: &str,
    tool_use_id: &str,
    output: &str,
) -> std::io::Result<PathBuf> {
    let session_dir = archive_root.join("tool-results").join(session_id);
    fs::create_dir_all(&session_dir).await?;
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let safe_tool_use_id: String = tool_use_id
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => ch,
            _ => '_',
        })
        .collect();
    let path = session_dir.join(format!("{timestamp}-{safe_tool_use_id}.txt"));
    fs::write(&path, output).await?;
    Ok(path)
}

pub async fn archive_old_tool_results(
    archive_root: &Path,
    session_id: &str,
    history: &[Message],
) -> std::io::Result<Vec<Message>> {
    let preserve_from = live_tail_start(history).unwrap_or(history.len());
    let tool_names = tool_name_map(history);
    let mut remapped = Vec::with_capacity(history.len());

    for (index, message) in history.iter().enumerate() {
        let rewritten = match &message.content {
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } if index < preserve_from && !output.starts_with("[tool result archived:") => {
                let archived =
                    archive_tool_result(archive_root, session_id, tool_use_id, output).await?;
                let archive_ref = archived.display().to_string();
                let tool_name = tool_names
                    .get(tool_use_id)
                    .map(String::as_str)
                    .unwrap_or("unknown");
                Message {
                    role: message.role.clone(),
                    content: MessageContent::ToolResult {
                        tool_use_id: tool_use_id.clone(),
                        output: render_archived_tool_result(
                            tool_name,
                            tool_use_id,
                            *is_error,
                            output,
                            &archive_ref,
                        ),
                        is_error: *is_error,
                    },
                }
            }
            _ => message.clone(),
        };
        remapped.push(rewritten);
    }

    Ok(remapped)
}

pub fn render_archived_tool_result(
    tool_name: &str,
    tool_use_id: &str,
    is_error: bool,
    output: &str,
    archive_ref: &str,
) -> String {
    let status = if is_error { "error" } else { "ok" };
    let mut preview = output
        .chars()
        .take(TOOL_RESULT_PREVIEW_HEAD_CHARS)
        .collect::<String>();
    let total_chars = output.chars().count();
    if total_chars > TOOL_RESULT_PREVIEW_HEAD_CHARS {
        let tail = output
            .chars()
            .skip(total_chars.saturating_sub(TOOL_RESULT_PREVIEW_TAIL_CHARS))
            .collect::<String>();
        preview.push_str("\n\n[... elided ...]\n\n");
        preview.push_str(&tail);
    }
    format!(
        "[tool result archived: {tool_name}, {status}, {total_chars} chars, id={tool_use_id}, archive={archive_ref}]\n{preview}"
    )
}

pub fn render_initial_archived_tool_result(
    tool_name: &str,
    tool_use_id: &str,
    is_error: bool,
    output: &str,
    archive_ref: &str,
) -> String {
    let status = if is_error { "error" } else { "ok" };
    let total_chars = output.chars().count();
    let total_lines = output.lines().count();
    let preview = output
        .lines()
        .take(INITIAL_TOOL_RESULT_PREVIEW_LINES)
        .collect::<Vec<_>>()
        .join("\n");

    let omitted_notice = if total_lines > INITIAL_TOOL_RESULT_PREVIEW_LINES {
        format!(
            "\n\n[... omitted {} more line(s); full tool result archived at {archive_ref} ...]",
            total_lines - INITIAL_TOOL_RESULT_PREVIEW_LINES
        )
    } else if total_chars > preview.chars().count() {
        format!(
            "\n\n[... remaining content omitted; full tool result archived at {archive_ref} ...]"
        )
    } else {
        String::new()
    };

    format!(
        "[tool result archived: {tool_name}, {status}, {total_chars} chars, id={tool_use_id}, archive={archive_ref}]\n{preview}{omitted_notice}"
    )
}

fn live_tail_start(history: &[Message]) -> Option<usize> {
    let last = history.last()?;
    match &last.content {
        MessageContent::ToolResult { .. } => {
            let mut start = history.len() - 1;
            while start > 0
                && matches!(
                    history[start - 1].content,
                    MessageContent::ToolResult { .. }
                )
            {
                start -= 1;
            }
            if start > 0 && matches!(history[start - 1].content, MessageContent::ToolUse { .. }) {
                start -= 1;
            }
            Some(start)
        }
        MessageContent::ToolUse { .. } => Some(history.len() - 1),
        MessageContent::Text(_) if last.role == Role::User => Some(history.len() - 1),
        _ => None,
    }
}

fn compaction_plan_from_snapshot(
    state: &SessionMemoryState,
    history: &[Message],
    snapshot: SessionMemoryCompactionSnapshot,
) -> Option<CompactionPlan> {
    let live_tail_start = live_tail_start(history).unwrap_or(history.len());
    let tail_start = snapshot
        .metadata
        .last_summarized_message_index
        .checked_add(1)
        .unwrap_or(history.len());

    if tail_start > history.len() || tail_start > live_tail_start {
        return None;
    }

    if state
        .last_summarized_message_index
        .is_some_and(|index| index != snapshot.metadata.last_summarized_message_index)
    {
        return None;
    }

    Some(CompactionPlan {
        source: CompactionSource::SessionMemory,
        summary: snapshot.summary_markdown.trim().to_string(),
        tail_start,
    })
}

fn tool_name_map(history: &[Message]) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for message in history {
        if let MessageContent::ToolUse { tool_calls, .. } = &message.content {
            for call in tool_calls {
                names.insert(call.tool_use_id.clone(), call.tool_name.clone());
            }
        }
    }
    names
}

fn render_tool_placeholder(
    tool_name: Option<&str>,
    tool_use_id: &str,
    is_error: bool,
    output_len: usize,
) -> String {
    let status = if is_error { "error" } else { "ok" };
    match tool_name {
        Some(name) => {
            format!("[tool result elided: {name}, {status}, {output_len} chars, id={tool_use_id}]")
        }
        None => format!("[tool result elided: {status}, {output_len} chars, id={tool_use_id}]"),
    }
}

fn render_transcript(history: &[Message]) -> String {
    let mut lines = Vec::new();
    for message in history {
        if message.role == Role::System {
            continue;
        }

        match &message.content {
            MessageContent::Text(text) => {
                lines.push(format!("{}:\n{}", role_label(message.role.clone()), text));
            }
            MessageContent::ToolUse { text, tool_calls } => {
                let mut block = String::new();
                block.push_str("assistant tool_use:");
                if let Some(text) = text {
                    block.push('\n');
                    block.push_str(text);
                }
                for call in tool_calls {
                    block.push_str(&format!(
                        "\n- {} id={} args={}",
                        call.tool_name, call.tool_use_id, call.arguments
                    ));
                }
                lines.push(block);
            }
            MessageContent::ToolResult {
                tool_use_id,
                output,
                is_error,
            } => {
                lines.push(format!(
                    "tool result id={} error={}:\n{}",
                    tool_use_id, is_error, output
                ));
            }
        }
    }

    lines.join("\n\n")
}

fn role_label(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_llm::ToolUseRequest;
    use uuid::Uuid;

    #[test]
    fn micro_compact_replaces_old_tool_results() {
        let history = vec![
            Message::user("first"),
            Message::assistant_tool_use(
                None,
                vec![ToolUseRequest {
                    tool_use_id: "id-1".into(),
                    tool_name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "pwd"}),
                }],
            ),
            Message::tool_result("id-1", "very long output", false),
            Message::user("second"),
        ];

        let compacted = build_micro_compacted_history(&history);
        match &compacted[2].content {
            MessageContent::ToolResult { output, .. } => {
                assert!(output.contains("bash"));
                assert!(output.contains("elided"));
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn micro_compact_preserves_live_tool_tail() {
        let history = vec![
            Message::user("first"),
            Message::assistant_tool_use(
                None,
                vec![ToolUseRequest {
                    tool_use_id: "id-1".into(),
                    tool_name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "pwd"}),
                }],
            ),
            Message::tool_result("id-1", "tail output", false),
        ];

        let compacted = build_micro_compacted_history(&history);
        match &compacted[2].content {
            MessageContent::ToolResult { output, .. } => {
                assert_eq!(output, "tail output");
            }
            _ => panic!("expected tool result"),
        }
    }

    #[test]
    fn split_history_keeps_latest_user_message_live() {
        let history = vec![
            Message::system("system"),
            Message::user("old"),
            Message::assistant("answer"),
            Message::user("latest"),
        ];

        let (prefix, tail) = split_history_for_compaction(&history);
        assert_eq!(prefix.len(), 3);
        assert_eq!(tail.len(), 1);
    }

    #[tokio::test]
    async fn archive_old_tool_results_rewrites_only_non_live_results() {
        let history = vec![
            Message::assistant_tool_use(
                None,
                vec![ToolUseRequest {
                    tool_use_id: "id-1".into(),
                    tool_name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "pwd"}),
                }],
            ),
            Message::tool_result("id-1", "old output", false),
            Message::assistant_tool_use(
                None,
                vec![ToolUseRequest {
                    tool_use_id: "id-2".into(),
                    tool_name: "bash".into(),
                    arguments: serde_json::json!({"cmd": "ls"}),
                }],
            ),
            Message::tool_result("id-2", "live output", false),
        ];
        let archive_root =
            std::env::temp_dir().join(format!("quine-core-compaction-{}", Uuid::new_v4()));

        let rewritten = archive_old_tool_results(&archive_root, "session-1", &history)
            .await
            .unwrap();

        match &rewritten[1].content {
            MessageContent::ToolResult { output, .. } => {
                assert!(output.starts_with("[tool result archived: bash, ok"));
            }
            _ => panic!("expected archived old tool result"),
        }
        match &rewritten[3].content {
            MessageContent::ToolResult { output, .. } => {
                assert_eq!(output, "live output");
            }
            _ => panic!("expected live tool result"),
        }

        let _ = std::fs::remove_dir_all(archive_root);
    }
}
