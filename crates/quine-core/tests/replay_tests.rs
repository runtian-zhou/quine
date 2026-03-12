use quine_core::conversation::*;
use quine_core::log::ConversationLog;
use quine_core::replay::{ReplayEngine, ReplayOptions};
use quine_core::tool::ToolRegistry;
use serde_json::json;
use tempfile::TempDir;

fn make_log_with_write(tmp: &TempDir, file_name: &str, content: &str) -> ConversationLog {
    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );
    log.push(Entry::UserMessage {
        content: format!("Create {}", file_name),
    });
    log.push(Entry::AssistantMessage {
        content: "I'll create that file.".to_string(),
        tool_calls: vec![ToolCall {
            id: "tc_1".to_string(),
            name: "Write".to_string(),
            arguments: json!({"file_path": file_name, "content": content}),
        }],
    });
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "Write".to_string(),
        arguments: json!({"file_path": file_name, "content": content}),
        result: ToolOutput {
            success: true,
            output: format!(
                "File written successfully: {}",
                tmp.path().join(file_name).display()
            ),
        },
    });
    log
}

#[test]
fn replay_executes_tools_and_creates_files() {
    let tmp = TempDir::new().unwrap();
    let log = make_log_with_write(&tmp, "replay_test.txt", "replay content");
    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions::default();

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().unwrap();

    assert_eq!(result.entries_processed, 3, "should process all 3 entries");
    assert_eq!(result.tools_executed, 1, "should execute exactly 1 tool");
    assert_eq!(result.drift_warnings.len(), 0, "should have 0 drift warnings");

    let content = std::fs::read_to_string(tmp.path().join("replay_test.txt")).unwrap();
    assert_eq!(content, "replay content", "file should be created by replay");
}

#[test]
fn replay_detects_drift_when_output_differs() {
    let tmp = TempDir::new().unwrap();

    // Pre-create the file with different content so Write succeeds but Read output differs
    std::fs::write(tmp.path().join("drifted.txt"), "actual content").unwrap();

    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "Read".to_string(),
        arguments: json!({"file_path": "drifted.txt"}),
        result: ToolOutput {
            success: true,
            output: "     1\texpected content".to_string(), // different from actual
        },
    });

    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions {
        strict: false,
        dry_run: false,
    };

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().unwrap();

    assert_eq!(result.tools_executed, 1, "should execute exactly 1 tool");
    assert_eq!(result.drift_warnings.len(), 1, "should detect exactly 1 drift");
    assert_eq!(result.drift_warnings[0].tool_name, "Read");
}

#[test]
fn replay_strict_mode_aborts_on_drift() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("strict.txt"), "actual").unwrap();

    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "Read".to_string(),
        arguments: json!({"file_path": "strict.txt"}),
        result: ToolOutput {
            success: true,
            output: "wrong output".to_string(),
        },
    });

    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions {
        strict: true,
        dry_run: false,
    };

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run();

    assert!(result.is_err(), "strict mode should abort on drift");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Drift detected"),
        "error message should mention drift: got '{}'",
        err_msg
    );
}

#[test]
fn replay_dry_run_does_not_execute_tools() {
    let tmp = TempDir::new().unwrap();
    let log = make_log_with_write(&tmp, "should_not_exist.txt", "data");
    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions {
        strict: false,
        dry_run: true,
    };

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().unwrap();

    assert_eq!(result.tools_executed, 1, "dry-run should still count the tool");
    assert_eq!(result.drift_warnings.len(), 0, "dry-run produces 0 drift warnings");
    assert!(
        !tmp.path().join("should_not_exist.txt").exists(),
        "file should not be created in dry-run mode"
    );
}

#[test]
fn replay_no_drift_when_output_matches() {
    let tmp = TempDir::new().unwrap();
    let log = make_log_with_write(&tmp, "match.txt", "exact content");
    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions::default();

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().unwrap();

    assert_eq!(result.drift_warnings.len(), 0, "no drift when output matches");
}

#[test]
fn replay_unknown_tool_in_non_strict_mode() {
    let tmp = TempDir::new().unwrap();
    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "FakeTool".to_string(),
        arguments: json!({}),
        result: ToolOutput {
            success: true,
            output: "anything".to_string(),
        },
    });

    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions::default();

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run().unwrap();

    // Unknown tool is skipped in non-strict mode (not counted as executed)
    assert_eq!(result.tools_executed, 0, "unknown tool should not be counted as executed");
}

#[test]
fn replay_unknown_tool_in_strict_mode_errors() {
    let tmp = TempDir::new().unwrap();
    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "FakeTool".to_string(),
        arguments: json!({}),
        result: ToolOutput {
            success: true,
            output: "anything".to_string(),
        },
    });

    let registry = ToolRegistry::register_defaults(tmp.path());
    let options = ReplayOptions {
        strict: true,
        dry_run: false,
    };

    let engine = ReplayEngine::new(log, registry, options);
    let result = engine.run();

    assert!(result.is_err(), "strict mode should error on unknown tool");
}

#[test]
fn replay_multiple_tool_executions() {
    let tmp = TempDir::new().unwrap();
    let mut log = ConversationLog::new(
        "test".to_string(),
        "test".to_string(),
        "system".to_string(),
    );

    // Write two files
    for i in 1..=3 {
        let name = format!("file{}.txt", i);
        let content = format!("content {}", i);
        log.push(Entry::ToolExecution {
            tool_call_id: format!("tc_{}", i),
            tool_name: "Write".to_string(),
            arguments: json!({"file_path": name, "content": content}),
            result: ToolOutput {
                success: true,
                output: format!(
                    "File written successfully: {}",
                    tmp.path().join(&name).display()
                ),
            },
        });
    }

    let registry = ToolRegistry::register_defaults(tmp.path());
    let engine = ReplayEngine::new(log, registry, ReplayOptions::default());
    let result = engine.run().unwrap();

    assert_eq!(result.entries_processed, 3, "should process all 3 entries");
    assert_eq!(result.tools_executed, 3, "should execute all 3 tools");

    for i in 1..=3 {
        let content = std::fs::read_to_string(tmp.path().join(format!("file{}.txt", i))).unwrap();
        assert_eq!(content, format!("content {}", i), "file{}.txt should have correct content", i);
    }
}
