use anyhow::Result;
use eventsource_stream::Eventsource;
use futures::stream::{BoxStream, StreamExt};

use super::api_types::{AnthropicBlock, AnthropicStreamEvent, AnthropicUsage, DeltaBlock};
use crate::types::{StopReason, StreamEvent, Usage};

pub fn parse_anthropic_sse(response: reqwest::Response) -> BoxStream<'static, Result<StreamEvent>> {
    let stream = futures::stream::unfold(
        (
            response.bytes_stream().eventsource(),
            std::collections::VecDeque::<Result<StreamEvent>>::new(),
        ),
        |(mut es, mut pending)| async move {
            loop {
                // Drain any pending events from a previous multi-event SSE message.
                if let Some(event) = pending.pop_front() {
                    return Some((event, (es, pending)));
                }
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
                                let mut events = convert_stream_event(ev);
                                if events.is_empty() {
                                    continue;
                                }
                                let first = events.remove(0);
                                pending.extend(events);
                                return Some((first, (es, pending)));
                            }
                            Err(e) => {
                                return Some((
                                    Err(anyhow::anyhow!("Failed to parse SSE: {}", e)),
                                    (es, pending),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        return Some((Err(anyhow::anyhow!("SSE error: {}", e)), (es, pending)));
                    }
                }
            }
        },
    );

    Box::pin(stream)
}

fn convert_anthropic_usage(u: &AnthropicUsage) -> Usage {
    Usage {
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_creation_input_tokens: u.cache_creation_input_tokens.unwrap_or(0),
        cache_read_input_tokens: u.cache_read_input_tokens.unwrap_or(0),
    }
}

fn convert_stream_event(event: AnthropicStreamEvent) -> Vec<Result<StreamEvent>> {
    match event {
        AnthropicStreamEvent::ContentBlockStart {
            content_block: AnthropicBlock::Text { .. },
            ..
        } => vec![],
        AnthropicStreamEvent::ContentBlockStart {
            content_block: AnthropicBlock::ToolUse { id, name, .. },
            ..
        } => vec![Ok(StreamEvent::ToolCallStart { id, name })],
        AnthropicStreamEvent::ContentBlockDelta { delta, .. } => match delta {
            DeltaBlock::TextDelta { text } => vec![Ok(StreamEvent::ContentDelta(text))],
            DeltaBlock::InputJsonDelta { partial_json } => {
                vec![Ok(StreamEvent::ToolCallDelta(partial_json))]
            }
        },
        AnthropicStreamEvent::ContentBlockStop { .. } => vec![Ok(StreamEvent::ToolCallEnd)],
        AnthropicStreamEvent::MessageStart { message } => match message.usage {
            Some(u) => vec![Ok(StreamEvent::Usage(convert_anthropic_usage(&u)))],
            None => vec![],
        },
        AnthropicStreamEvent::MessageDelta { delta } => {
            let mut events = Vec::new();
            if let Some(u) = &delta.usage {
                events.push(Ok(StreamEvent::Usage(convert_anthropic_usage(u))));
            }
            let reason = match delta.stop_reason.as_deref() {
                Some("end_turn") => StopReason::EndTurn,
                Some("tool_use") => StopReason::ToolUse,
                Some("max_tokens") => StopReason::MaxTokens,
                Some(other) => StopReason::Unknown(other.to_string()),
                None => StopReason::Unknown("none".to_string()),
            };
            events.push(Ok(StreamEvent::Done(reason)));
            events
        }
        AnthropicStreamEvent::Ping => vec![],
        AnthropicStreamEvent::MessageStop => vec![],
        AnthropicStreamEvent::Error { error } => {
            vec![Err(anyhow::anyhow!("Anthropic error: {}", error.message))]
        }
        _ => vec![],
    }
}
