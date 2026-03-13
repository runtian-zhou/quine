use quine_llm::types::*;
use serde_json::json;

#[test]
fn chat_content_text_as_text() {
    let content = ChatContent::Text("hello world".to_string());
    assert_eq!(content.as_text(), "hello world");
}

#[test]
fn chat_content_blocks_as_text_extracts_text_only() {
    let content = ChatContent::Blocks(vec![
        ContentBlock::Text {
            text: "part one ".to_string(),
        },
        ContentBlock::ToolUse {
            id: "tc_1".to_string(),
            name: "Read".to_string(),
            input: json!({}),
        },
        ContentBlock::Text {
            text: "part two".to_string(),
        },
    ]);
    assert_eq!(
        content.as_text(),
        "part one part two",
        "as_text should concatenate only Text blocks"
    );
}

#[test]
fn chat_content_empty_blocks_as_text() {
    let content = ChatContent::Blocks(vec![]);
    assert_eq!(content.as_text(), "", "empty blocks should produce empty string");
}

#[test]
fn chat_content_text_serialization_roundtrip() {
    let content = ChatContent::Text("test".to_string());
    let json = serde_json::to_string(&content).unwrap();
    let deserialized: ChatContent = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.as_text(), "test");
}

#[test]
fn content_block_text_serialization() {
    let block = ContentBlock::Text {
        text: "hello".to_string(),
    };
    let value: serde_json::Value = serde_json::to_value(&block).unwrap();
    assert_eq!(value["type"], "text", "type tag should be 'text'");
    assert_eq!(value["text"], "hello");
}

#[test]
fn content_block_tool_use_serialization() {
    let block = ContentBlock::ToolUse {
        id: "call_123".to_string(),
        name: "Write".to_string(),
        input: json!({"file_path": "test.txt"}),
    };
    let value: serde_json::Value = serde_json::to_value(&block).unwrap();
    assert_eq!(value["type"], "tool_use");
    assert_eq!(value["id"], "call_123");
    assert_eq!(value["name"], "Write");
    assert_eq!(value["input"]["file_path"], "test.txt");
}

#[test]
fn content_block_tool_result_serialization() {
    let block = ContentBlock::ToolResult {
        tool_use_id: "call_123".to_string(),
        content: "file contents here".to_string(),
    };
    let value: serde_json::Value = serde_json::to_value(&block).unwrap();
    assert_eq!(value["type"], "tool_result");
    assert_eq!(value["tool_use_id"], "call_123");
    assert_eq!(value["content"], "file contents here");
}

#[test]
fn stop_reason_equality() {
    assert_eq!(StopReason::EndTurn, StopReason::EndTurn);
    assert_eq!(StopReason::ToolUse, StopReason::ToolUse);
    assert_eq!(StopReason::MaxTokens, StopReason::MaxTokens);
    assert_eq!(
        StopReason::Unknown("custom".to_string()),
        StopReason::Unknown("custom".to_string())
    );
    assert_ne!(StopReason::EndTurn, StopReason::ToolUse);
    assert_ne!(
        StopReason::Unknown("a".to_string()),
        StopReason::Unknown("b".to_string())
    );
}

#[test]
fn chat_message_serialization_roundtrip() {
    let msg = ChatMessage {
        role: "user".to_string(),
        content: ChatContent::Text("What is Rust?".to_string()),
    };
    let json = serde_json::to_string(&msg).unwrap();
    let deserialized: ChatMessage = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.role, "user");
    assert_eq!(deserialized.content.as_text(), "What is Rust?");
}

#[test]
fn completion_request_serialization() {
    let req = CompletionRequest {
        model: "test-model".to_string(),
        system: "You are helpful.".to_string(),
        messages: vec![ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Text("hi".to_string()),
        }],
        tools: vec![],
        max_tokens: 1024,
    };
    let value: serde_json::Value = serde_json::to_value(&req).unwrap();
    assert_eq!(value["model"], "test-model");
    assert_eq!(value["system"], "You are helpful.");
    assert_eq!(value["max_tokens"], 1024);
    assert!(value.get("tools").is_none(), "empty tools should be omitted");
}

#[test]
fn usage_default_is_all_zeros() {
    let usage = quine_llm::types::Usage::default();
    assert_eq!(usage.input_tokens, 0, "default input_tokens should be exactly 0");
    assert_eq!(usage.output_tokens, 0, "default output_tokens should be exactly 0");
    assert_eq!(
        usage.cache_creation_input_tokens, 0,
        "default cache_creation_input_tokens should be exactly 0"
    );
    assert_eq!(
        usage.cache_read_input_tokens, 0,
        "default cache_read_input_tokens should be exactly 0"
    );
}

#[test]
fn usage_clone() {
    let usage = quine_llm::types::Usage {
        input_tokens: 42,
        output_tokens: 99,
        cache_creation_input_tokens: 7,
        cache_read_input_tokens: 13,
    };
    let cloned = usage.clone();
    assert_eq!(cloned.input_tokens, 42, "cloned input_tokens should be exactly 42");
    assert_eq!(cloned.output_tokens, 99, "cloned output_tokens should be exactly 99");
    assert_eq!(
        cloned.cache_creation_input_tokens, 7,
        "cloned cache_creation_input_tokens should be exactly 7"
    );
    assert_eq!(
        cloned.cache_read_input_tokens, 13,
        "cloned cache_read_input_tokens should be exactly 13"
    );
}
