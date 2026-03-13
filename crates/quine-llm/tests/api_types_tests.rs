use quine_llm::anthropic::api_types::*;
use quine_llm::openai::api_types::*;

#[test]
fn anthropic_usage_deserializes_with_cache() {
    let json = r#"{
        "input_tokens": 100,
        "output_tokens": 50,
        "cache_creation_input_tokens": 10,
        "cache_read_input_tokens": 20
    }"#;
    let usage: AnthropicUsage = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(usage.input_tokens, 100, "input_tokens should be exactly 100");
    assert_eq!(usage.output_tokens, 50, "output_tokens should be exactly 50");
    assert_eq!(
        usage.cache_creation_input_tokens,
        Some(10),
        "cache_creation_input_tokens should be exactly Some(10)"
    );
    assert_eq!(
        usage.cache_read_input_tokens,
        Some(20),
        "cache_read_input_tokens should be exactly Some(20)"
    );
}

#[test]
fn anthropic_usage_deserializes_without_cache() {
    let json = r#"{"input_tokens": 100, "output_tokens": 50}"#;
    let usage: AnthropicUsage = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(usage.input_tokens, 100, "input_tokens should be exactly 100");
    assert_eq!(usage.output_tokens, 50, "output_tokens should be exactly 50");
    assert_eq!(
        usage.cache_creation_input_tokens, None,
        "cache_creation_input_tokens should be None when absent from JSON"
    );
    assert_eq!(
        usage.cache_read_input_tokens, None,
        "cache_read_input_tokens should be None when absent from JSON"
    );
}

#[test]
fn anthropic_response_deserializes_with_usage() {
    let json = r#"{
        "content": [
            {"type": "text", "text": "Hello there"}
        ],
        "stop_reason": "end_turn",
        "usage": {
            "input_tokens": 300,
            "output_tokens": 150
        }
    }"#;
    let response: AnthropicResponse = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(response.content.len(), 1, "content should have exactly 1 block");
    assert_eq!(
        response.stop_reason,
        Some("end_turn".to_string()),
        "stop_reason should be exactly 'end_turn'"
    );
    let usage = response.usage.expect("usage should be Some");
    assert_eq!(usage.input_tokens, 300, "usage input_tokens should be exactly 300");
    assert_eq!(usage.output_tokens, 150, "usage output_tokens should be exactly 150");
}

#[test]
fn openai_usage_deserializes() {
    let json = r#"{"prompt_tokens": 200, "completion_tokens": 100}"#;
    let usage: OpenAiUsage = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(usage.prompt_tokens, 200, "prompt_tokens should be exactly 200");
    assert_eq!(usage.completion_tokens, 100, "completion_tokens should be exactly 100");
}

#[test]
fn openai_response_deserializes_with_usage() {
    let json = r#"{
        "choices": [
            {
                "message": {
                    "role": "assistant",
                    "content": "Hi!"
                },
                "finish_reason": "stop"
            }
        ],
        "usage": {
            "prompt_tokens": 500,
            "completion_tokens": 250
        }
    }"#;
    let response: OpenAiResponse = serde_json::from_str(json).expect("should deserialize");
    assert_eq!(response.choices.len(), 1, "choices should have exactly 1 entry");
    assert_eq!(
        response.choices[0].finish_reason,
        Some("stop".to_string()),
        "finish_reason should be exactly 'stop'"
    );
    let usage = response.usage.expect("usage should be Some");
    assert_eq!(usage.prompt_tokens, 500, "usage prompt_tokens should be exactly 500");
    assert_eq!(usage.completion_tokens, 250, "usage completion_tokens should be exactly 250");
}
