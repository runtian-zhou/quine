use quine_core::conversation::*;
use serde_json::json;

#[test]
fn conversation_starts_empty() {
    let conv = Conversation::new();
    assert_eq!(
        conv.entries.len(),
        0,
        "new conversation should have 0 entries"
    );
}

#[test]
fn conversation_push_entries() {
    let mut conv = Conversation::new();
    conv.push(Entry::UserMessage {
        content: "hello".to_string(),
    });
    conv.push(Entry::AssistantMessage {
        content: "hi there".to_string(),
        tool_calls: vec![],
    });
    assert_eq!(
        conv.entries.len(),
        2,
        "should have exactly 2 entries after two pushes"
    );
}

#[test]
fn entry_user_message_serialization_roundtrip() {
    let entry = Entry::UserMessage {
        content: "test message".to_string(),
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: Entry = serde_json::from_str(&json).unwrap();
    match deserialized {
        Entry::UserMessage { content } => {
            assert_eq!(content, "test message", "content should survive roundtrip");
        }
        _ => panic!("expected UserMessage variant"),
    }
}

#[test]
fn entry_assistant_message_serialization_roundtrip() {
    let entry = Entry::AssistantMessage {
        content: "response text".to_string(),
        tool_calls: vec![ToolCall {
            id: "call_1".to_string(),
            name: "Read".to_string(),
            arguments: json!({"file_path": "test.rs"}),
        }],
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: Entry = serde_json::from_str(&json).unwrap();
    match deserialized {
        Entry::AssistantMessage {
            content,
            tool_calls,
        } => {
            assert_eq!(content, "response text");
            assert_eq!(tool_calls.len(), 1, "should have exactly 1 tool call");
            assert_eq!(tool_calls[0].id, "call_1");
            assert_eq!(tool_calls[0].name, "Read");
            assert_eq!(tool_calls[0].arguments["file_path"], "test.rs");
        }
        _ => panic!("expected AssistantMessage variant"),
    }
}

#[test]
fn entry_tool_execution_serialization_roundtrip() {
    let entry = Entry::ToolExecution {
        tool_call_id: "call_1".to_string(),
        tool_name: "Write".to_string(),
        arguments: json!({"file_path": "out.txt", "content": "data"}),
        result: ToolOutput {
            success: true,
            output: "File written successfully: out.txt".to_string(),
        },
    };
    let json = serde_json::to_string(&entry).unwrap();
    let deserialized: Entry = serde_json::from_str(&json).unwrap();
    match deserialized {
        Entry::ToolExecution {
            tool_call_id,
            tool_name,
            arguments,
            result,
        } => {
            assert_eq!(tool_call_id, "call_1");
            assert_eq!(tool_name, "Write");
            assert_eq!(arguments["file_path"], "out.txt");
            assert!(result.success);
            assert_eq!(result.output, "File written successfully: out.txt");
        }
        _ => panic!("expected ToolExecution variant"),
    }
}

#[test]
fn entry_tagged_type_field_present_in_json() {
    let entry = Entry::UserMessage {
        content: "hi".to_string(),
    };
    let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
    assert_eq!(
        value["type"], "UserMessage",
        "JSON should contain 'type' tag for internally-tagged enum"
    );
}

#[test]
fn assistant_message_without_tool_calls_omits_field() {
    let entry = Entry::AssistantMessage {
        content: "plain text".to_string(),
        tool_calls: vec![],
    };
    let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
    assert!(
        value.get("tool_calls").is_none(),
        "empty tool_calls should be omitted from JSON via skip_serializing_if"
    );
}
