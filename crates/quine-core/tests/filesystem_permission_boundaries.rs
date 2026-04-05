use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use quine_core::{
    create_channels, run_core_loop, ChannelConfig, CoreInput, CoreOutput, MemoryPolicyConfig,
    SessionId, ToolOutcome,
};
use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
use tokio::sync::oneshot;
use tokio::time::{timeout, Duration};

struct ScriptedToolProvider {
    call_count: AtomicU32,
    tool_use_id: String,
    tool_name: String,
    arguments: serde_json::Value,
    final_text: String,
}

#[async_trait::async_trait]
impl LlmProvider for ScriptedToolProvider {
    async fn send(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn futures::Stream<Item = anyhow::Result<LlmEvent>> + Send>>> {
        let count = self.call_count.fetch_add(1, Ordering::SeqCst);
        let events = if count == 0 {
            vec![
                Ok(LlmEvent::ToolCall {
                    tool_use_id: self.tool_use_id.clone(),
                    tool_name: self.tool_name.clone(),
                    arguments: self.arguments.clone(),
                }),
                Ok(LlmEvent::Done { usage: None }),
            ]
        } else {
            vec![
                Ok(LlmEvent::TextDelta {
                    text: self.final_text.clone(),
                }),
                Ok(LlmEvent::Done { usage: None }),
            ]
        };
        Ok(Box::pin(futures::stream::iter(events)))
    }
}

struct TurnResult {
    tool_outcome: ToolOutcome,
}

async fn run_tool_turn(
    workspace_root: PathBuf,
    tool_use_id: &str,
    tool_name: &str,
    arguments: serde_json::Value,
    auto_approve: bool,
) -> TurnResult {
    let (mut harness, core) = create_channels(ChannelConfig::default());
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedToolProvider {
        call_count: AtomicU32::new(0),
        tool_use_id: tool_use_id.into(),
        tool_name: tool_name.into(),
        arguments,
        final_text: format!("{tool_name} completed"),
    });
    let core_task = tokio::spawn(run_core_loop(core, provider, None));

    let session_id = SessionId::new();
    let (reply_tx, reply_rx) = oneshot::channel();
    harness
        .input
        .send(CoreInput::CreateSession {
            session_id,
            system_prompt: None,
            working_directory: Some(workspace_root),
            skills: Vec::new(),
            plan_mode: false,
            initial_messages: Vec::new(),
            agent_key: None,
            team_key: None,
            memory_policy: MemoryPolicyConfig::default(),
            reply: reply_tx,
        })
        .await
        .unwrap();
    reply_rx.await.unwrap().unwrap();

    harness
        .input
        .send(CoreInput::UserMessage {
            session_id,
            content: "run the scripted tool".into(),
        })
        .await
        .unwrap();

    let tool_outcome = loop {
        let event = timeout(Duration::from_secs(5), harness.output.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for core output for {tool_name}"))
            .expect("core output channel closed unexpectedly");
        match event {
            CoreOutput::InteractionNeeded {
                session_id: event_session_id,
                ..
            } if event_session_id == session_id && auto_approve => {
                harness
                    .input
                    .send(CoreInput::InteractionResponse {
                        session_id,
                        response: quine_core::InteractionResponse {
                            response: "approve once".into(),
                            selected_indices: vec![0],
                        },
                    })
                    .await
                    .unwrap();
            }
            CoreOutput::ToolResult {
                session_id: event_session_id,
                tool_use_id: event_tool_use_id,
                tool_name: event_tool_name,
                content,
                is_error,
                ..
            } if event_session_id == session_id && event_tool_use_id == tool_use_id => {
                assert_eq!(event_tool_name, tool_name);
                break if is_error {
                    ToolOutcome::Error { message: content }
                } else {
                    ToolOutcome::Success { output: content }
                };
            }
            _ => {}
        }
    };

    harness.input.send(CoreInput::Shutdown).await.unwrap();
    core_task.await.unwrap();

    TurnResult { tool_outcome }
}

#[tokio::test]
async fn workspace_paths_remain_allowed_across_filesystem_tools() {
    let workspace = tempfile::TempDir::new().unwrap();
    let allowed_dir = workspace.path().join("allowed");
    std::fs::create_dir_all(&allowed_dir).unwrap();
    std::fs::write(allowed_dir.join("readable.txt"), "allowed-read").unwrap();
    std::fs::write(allowed_dir.join("writable.txt"), "before").unwrap();

    let read_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_read",
        "read_file",
        serde_json::json!({
            "file_path": "allowed/readable.txt"
        }),
        false,
    )
    .await;
    match &read_result.tool_outcome {
        ToolOutcome::Success { output } => assert!(
            output.contains("allowed-read"),
            "unexpected read_file output: {output}"
        ),
        outcome => panic!("read_file should succeed, got {outcome:?}"),
    }

    let find_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_find",
        "find",
        serde_json::json!({
            "path": ".",
            "pattern": "*.txt",
            "type": "file"
        }),
        false,
    )
    .await;
    match &find_result.tool_outcome {
        ToolOutcome::Success { output } => assert!(
            output.contains("allowed/readable.txt") && output.contains("allowed/writable.txt"),
            "unexpected find output: {output}"
        ),
        outcome => panic!("find should succeed, got {outcome:?}"),
    }

    let write_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_write",
        "apply_patch",
        serde_json::json!({
            "file_path": "allowed/writable.txt",
            "edits": [
                {
                    "old_text": "before",
                    "new_text": "after"
                }
            ]
        }),
        true,
    )
    .await;
    match &write_result.tool_outcome {
        ToolOutcome::Success { output } => assert!(
            output.contains("Successfully applied 1 patch operation"),
            "unexpected apply_patch output: {output}"
        ),
        outcome => panic!("apply_patch should succeed, got {outcome:?}"),
    }
    assert_eq!(
        std::fs::read_to_string(allowed_dir.join("writable.txt")).unwrap(),
        "after"
    );
}

#[tokio::test]
async fn outside_workspace_paths_are_denied_consistently_across_filesystem_tools() {
    let workspace = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    let forbidden_read = outside.path().join("forbidden-read.txt");
    let forbidden_write = outside.path().join("forbidden-write.txt");
    std::fs::write(&forbidden_read, "outside-read").unwrap();

    let read_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_read",
        "read_file",
        serde_json::json!({
            "file_path": forbidden_read
        }),
        false,
    )
    .await;
    match &read_result.tool_outcome {
        ToolOutcome::Error { message } => assert!(
            message.contains("permission denied") && message.contains("outside approved roots"),
            "unexpected read_file denial: {message}"
        ),
        outcome => panic!("read_file should be denied, got {outcome:?}"),
    }

    let find_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_find",
        "find",
        serde_json::json!({
            "path": outside.path(),
            "pattern": "*.txt",
            "type": "file"
        }),
        false,
    )
    .await;
    match &find_result.tool_outcome {
        ToolOutcome::Error { message } => assert!(
            message.contains("permission denied") && message.contains("outside approved roots"),
            "unexpected find denial: {message}"
        ),
        outcome => panic!("find should be denied, got {outcome:?}"),
    }

    let write_result = run_tool_turn(
        workspace.path().to_path_buf(),
        "toolu_write",
        "apply_patch",
        serde_json::json!({
            "file_path": forbidden_write,
            "new_file_content": "outside-write"
        }),
        false,
    )
    .await;
    match &write_result.tool_outcome {
        ToolOutcome::Error { message } => assert!(
            message.contains("permission denied") && message.contains("outside approved roots"),
            "unexpected apply_patch denial: {message}"
        ),
        outcome => panic!("apply_patch should be denied, got {outcome:?}"),
    }
    assert!(!forbidden_write.exists());
}
