use anyhow::Result;
use futures::stream::{BoxStream, StreamExt};
use reqwest_eventsource::{Event, EventSource};

use super::api_types::{AnthropicBlock, AnthropicStreamEvent, DeltaBlock};
use crate::types::{StopReason, StreamEvent};

pub fn parse_anthropic_stream(es: EventSource) -> BoxStream<'static, Result<StreamEvent>> {
    let stream = es.filter_map(|event| async move {
        match event {
            Ok(Event::Message(msg)) => {
                let parsed: Result<AnthropicStreamEvent, _> =
                    serde_json::from_str(&msg.data);
                match parsed {
                    Ok(ev) => convert_stream_event(ev),
                    Err(e) => Some(Err(anyhow::anyhow!("Failed to parse SSE: {}", e))),
                }
            }
            Ok(Event::Open) => None,
            Err(reqwest_eventsource::Error::StreamEnded) => None,
            Err(e) => Some(Err(anyhow::anyhow!("SSE error: {}", e))),
        }
    });

    Box::pin(stream)
}

fn convert_stream_event(event: AnthropicStreamEvent) -> Option<Result<StreamEvent>> {
    match event {
        AnthropicStreamEvent::ContentBlockStart {
            content_block: AnthropicBlock::Text { .. },
            ..
        } => None,
        AnthropicStreamEvent::ContentBlockStart {
            content_block: AnthropicBlock::ToolUse { id, name, .. },
            ..
        } => Some(Ok(StreamEvent::ToolCallStart { id, name })),
        AnthropicStreamEvent::ContentBlockDelta { delta, .. } => match delta {
            DeltaBlock::TextDelta { text } => Some(Ok(StreamEvent::ContentDelta(text))),
            DeltaBlock::InputJsonDelta { partial_json } => {
                Some(Ok(StreamEvent::ToolCallDelta(partial_json)))
            }
        },
        AnthropicStreamEvent::ContentBlockStop { .. } => Some(Ok(StreamEvent::ToolCallEnd)),
        AnthropicStreamEvent::MessageDelta { delta } => {
            let reason = match delta.stop_reason.as_deref() {
                Some("end_turn") => StopReason::EndTurn,
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                Some(other) => StopReason::Unknown(other.to_string()),
                None => StopReason::Unknown("none".to_string()),
            };
            Some(Ok(StreamEvent::Done(reason)))
        }
        AnthropicStreamEvent::MessageStop | AnthropicStreamEvent::Ping => None,
        AnthropicStreamEvent::MessageStart { .. } => None,
        AnthropicStreamEvent::Error { error } => {
            Some(Err(anyhow::anyhow!("Anthropic error: {}", error.message)))
        }
        _ => None,
    }
}
