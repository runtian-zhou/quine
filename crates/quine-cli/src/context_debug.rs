use std::path::PathBuf;

use chrono::{DateTime, Utc};
use quine_llm::ToolDefinition;
use serde::{Deserialize, Serialize};

use crate::client::IpcClient;
use crate::render::Renderer;
use quine_harness::protocol::methods;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanSnapshot {
    pub plan_id: String,
    pub title: String,
    pub actions: Vec<PlanActionSnapshot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PlanActionSnapshot {
    pub action_id: String,
    pub title: String,
    pub description: String,
    pub depends_on: Vec<String>,
    pub status: PlanActionStatusSnapshot,
    pub result: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum PlanActionStatusSnapshot {
    Pending,
    InProgress,
    Completed,
    Failed { error: String },
    Skipped { reason: String },
}

impl PlanActionStatusSnapshot {
    pub fn label(&self) -> &str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Completed => "completed",
            Self::Failed { .. } => "failed",
            Self::Skipped { .. } => "skipped",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SkillSnapshot {
    pub name: String,
    pub description: String,
    pub version: String,
    pub system_prompt: Option<String>,
    pub source_path: PathBuf,
    pub tool_names: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SessionContextSnapshot {
    pub session_id: String,
    pub created_at: DateTime<Utc>,
    pub state: String,
    pub system_prompt: Option<String>,
    pub skills: Vec<String>,
    pub working_directory: PathBuf,
    pub plan_mode: bool,
    pub auto_approve_permissions: bool,
    pub available_tools: Vec<ToolDefinition>,
    pub loaded_skills: Vec<SkillSnapshot>,
    pub plans: Vec<PlanSnapshot>,
    pub history: Vec<HistoryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum HistoryEntry {
    Text {
        role: String,
        text: String,
    },
    ToolUse {
        role: String,
        text: Option<String>,
        tool_calls: Vec<ToolCallEntry>,
    },
    ToolResult {
        role: String,
        tool_use_id: String,
        output: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ToolCallEntry {
    pub tool_use_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

pub(crate) async fn fetch_session_context(
    client: &mut IpcClient,
    session_id: &str,
) -> anyhow::Result<SessionContextSnapshot> {
    let params = serde_json::json!({ "session_id": session_id });
    let result = client
        .call(methods::GET_SESSION_CONTEXT, Some(params))
        .await?;
    let value = result.map_err(|message| anyhow::anyhow!(message))?;
    Ok(serde_json::from_value(value)?)
}

pub(crate) async fn render_session_context<R: Renderer>(
    renderer: &mut R,
    client: &mut IpcClient,
    session_id: &str,
) -> anyhow::Result<()> {
    let snapshot = fetch_session_context(client, session_id).await?;
    renderer
        .render_info(&serde_json::to_string_pretty(&snapshot)?)
        .await
}
