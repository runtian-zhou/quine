use anyhow::Result;
use eventsource_stream::Eventsource;
use futures::stream::{BoxStream, StreamExt};

use super::api_types::{AnthropicBlock, AnthropicStreamEvent, DeltaBlock};
use crate::types::{StopReason, StreamEvent};

pub fn parse_anthropic_sse(response: reqwest::Response) -> BoxStream<'static, Result<StreamEvent>> {
    let stream = futures::stream::unfold(
        response.bytes_stream().eventsource(),
        |mut es| async move {
            loop {
                let event = es.next().await?;
                match event {
                    Ok(ev) => {
                        let parsed: std::result::Result<AnthropicStreamEvent, _> =
                            serde_json::from_str(&ev.data);
                        match parsed {
                            Ok(AnthropicStreamEvent::MessageStop) => {
                                return None;
                            }
                            Ok(ev) => {
                                if let Some(stream_event) = convert_stream_event(ev) {
                                    return Some((stream_event, es));
                                }
                                continue;
                            }
                            Err(e) => {
                                return Some((
                                    Err(anyhow::anyhow!("Failed to parse SSE: {}", e)),
                                    es,
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        return Some((Err(anyhow::anyhow!("SSE error: {}", e)), es));
                    }
                }
            }
        },
    );

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
        AnthropicStreamEvent::Ping => None,
        AnthropicStreamEvent::MessageStart { .. } => None,
        AnthropicStreamEvent::MessageStop => None,
        AnthropicStreamEvent::Error { error } => {
            Some(Err(anyhow::anyhow!("Anthropic error: {}", error.message)))
        }
        _ => None,
    }
}
