use std::sync::Arc;

use quine_core::conversation::Entry;
use quine_llm::anthropic::AnthropicProvider;
use quine_llm::openai::OpenAiProvider;
use quine_llm::provider::LlmProvider;
use quine_llm::types::*;

/// Convert conversation log entries into ChatMessage list for the LLM.
pub fn entries_to_messages(entries: &[Entry]) -> Vec<ChatMessage> {
    let mut messages: Vec<ChatMessage> = Vec::new();
    let mut pending_tool_results: Vec<ContentBlock> = Vec::new();

    for entry in entries {
        match entry {
            Entry::ToolExecution {
                tool_call_id,
                result,
                ..
            } => {
                pending_tool_results.push(ContentBlock::ToolResult {
                    tool_use_id: tool_call_id.clone(),
                    content: result.output.clone(),
                });
            }
            other => {
                if !pending_tool_results.is_empty() {
                    messages.push(ChatMessage {
                        role: "user".to_string(),
                        content: ChatContent::Blocks(std::mem::take(&mut pending_tool_results)),
                    });
                }
                match other {
                    Entry::UserMessage { content } => {
                        messages.push(ChatMessage {
                            role: "user".to_string(),
                            content: ChatContent::Text(content.clone()),
                        });
                    }
                    Entry::AssistantMessage {
                        content,
                        tool_calls,
                    } => {
                        let mut blocks = Vec::new();
                        if !content.is_empty() {
                            blocks.push(ContentBlock::Text {
                                text: content.clone(),
                            });
                        }
                        for tc in tool_calls {
                            blocks.push(ContentBlock::ToolUse {
                                id: tc.id.clone(),
                                name: tc.name.clone(),
                                input: tc.arguments.clone(),
                            });
                        }
                        messages.push(ChatMessage {
                            role: "assistant".to_string(),
                            content: if blocks.len() == 1
                                && matches!(&blocks[0], ContentBlock::Text { .. })
                            {
                                ChatContent::Text(content.clone())
                            } else {
                                ChatContent::Blocks(blocks)
                            },
                        });
                    }
                    _ => {}
                }
            }
        }
    }
    // Flush any remaining tool results
    if !pending_tool_results.is_empty() {
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: ChatContent::Blocks(pending_tool_results),
        });
    }
    messages
}

pub fn create_provider(
    provider_name: &str,
    base_url: Option<&str>,
) -> anyhow::Result<Arc<dyn LlmProvider>> {
    match provider_name {
        "anthropic" => {
            let api_key = std::env::var("ANTHROPIC_API_KEY")
                .map_err(|_| anyhow::anyhow!("ANTHROPIC_API_KEY environment variable not set"))?;
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("ANTHROPIC_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        "openai" => {
            let api_key = std::env::var("OPENAI_API_KEY")
                .map_err(|_| anyhow::anyhow!("OPENAI_API_KEY environment variable not set"))?;
            let mut provider = OpenAiProvider::new(api_key);
            if let Some(url) = base_url.or(std::env::var("OPENAI_BASE_URL").ok().as_deref()) {
                provider = provider.with_base_url(url.to_string());
            }
            Ok(Arc::new(provider))
        }
        _ => anyhow::bail!(
            "Unknown provider: {}. Use 'anthropic' or 'openai'.",
            provider_name
        ),
    }
}
