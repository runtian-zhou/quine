use quine_core::conversation::*;
use quine_core::log::ConversationLog;
use serde_json::json;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn conversation_log_new_has_version_1() {
    let log = ConversationLog::new(
        "test-model".to_string(),
        "test-provider".to_string(),
        "system prompt".to_string(),
    );
    assert_eq!(log.version, 1, "new log should have version 1");
    assert_eq!(log.model, "test-model");
    assert_eq!(log.provider, "test-provider");
    assert_eq!(log.system_prompt, "system prompt");
    assert_eq!(log.entries.len(), 0, "new log should start with 0 entries");
}

#[test]
fn conversation_log_push_adds_entry() {
    let mut log = ConversationLog::new(
        "model".to_string(),
        "provider".to_string(),
        "prompt".to_string(),
    );
    log.push(Entry::UserMessage {
        content: "hello".to_string(),
    });
    log.push(Entry::AssistantMessage {
        content: "hi".to_string(),
        tool_calls: vec![],
    });
    assert_eq!(log.entries.len(), 2, "should have exactly 2 entries");
}

#[test]
fn conversation_log_save_and_load_roundtrip() {
    let tmp = TempDir::new().unwrap();
    let log_path = tmp.path().join("test_log.json");

    let mut log = ConversationLog::new(
        "claude-sonnet".to_string(),
        "anthropic".to_string(),
        "You are helpful.".to_string(),
    );
    log.push(Entry::UserMessage {
        content: "What is Rust?".to_string(),
    });
    log.push(Entry::AssistantMessage {
        content: "Rust is a systems programming language.".to_string(),
        tool_calls: vec![],
    });
    log.push(Entry::ToolExecution {
        tool_call_id: "tc_1".to_string(),
        tool_name: "Read".to_string(),
        arguments: json!({"file_path": "main.rs"}),
        result: ToolOutput {
            success: true,
            output: "fn main() {}".to_string(),
        },
    });

    log.save(&log_path).unwrap();

    let loaded = ConversationLog::load(&log_path).unwrap();
    assert_eq!(loaded.version, 1, "loaded version should be 1");
    assert_eq!(loaded.model, "claude-sonnet");
    assert_eq!(loaded.provider, "anthropic");
    assert_eq!(loaded.system_prompt, "You are helpful.");
    assert_eq!(loaded.entries.len(), 3, "should have exactly 3 entries after roundtrip");

    // Verify entry types survived
    match &loaded.entries[0] {
        Entry::UserMessage { content } => assert_eq!(content, "What is Rust?"),
        _ => panic!("entry 0 should be UserMessage"),
    }
    match &loaded.entries[1] {
        Entry::AssistantMessage { content, tool_calls } => {
            assert_eq!(content, "Rust is a systems programming language.");
            assert_eq!(tool_calls.len(), 0, "should have 0 tool calls");
        }
        _ => panic!("entry 1 should be AssistantMessage"),
    }
    match &loaded.entries[2] {
        Entry::ToolExecution {
            tool_call_id,
            tool_name,
            result,
            ..
        } => {
            assert_eq!(tool_call_id, "tc_1");
            assert_eq!(tool_name, "Read");
            assert_eq!(result.success, true);
        }
        _ => panic!("entry 2 should be ToolExecution"),
    }
}

#[test]
fn conversation_log_save_creates_parent_dirs() {
    let tmp = TempDir::new().unwrap();
    let nested_path = tmp.path().join("a").join("b").join("c").join("log.json");

    let log = ConversationLog::new(
        "m".to_string(),
        "p".to_string(),
        "s".to_string(),
    );
    log.save(&nested_path).unwrap();

    assert!(nested_path.exists(), "log file should exist at nested path");
}

#[test]
fn conversation_log_load_nonexistent_file_errors() {
    let result = ConversationLog::load(Path::new("/nonexistent/path/log.json"));
    assert!(result.is_err(), "loading nonexistent file should return error");
}

#[test]
fn conversation_log_json_format_matches_spec() {
    let log = ConversationLog::new(
        "test-model".to_string(),
        "test-provider".to_string(),
        "prompt".to_string(),
    );
    let json_value: serde_json::Value = serde_json::to_value(&log).unwrap();
    assert_eq!(json_value["version"], 1);
    assert_eq!(json_value["model"], "test-model");
    assert_eq!(json_value["provider"], "test-provider");
    assert_eq!(json_value["system_prompt"], "prompt");
    assert!(json_value["created_at"].is_string(), "created_at should be a string timestamp");
    assert!(json_value["entries"].is_array(), "entries should be an array");
}
