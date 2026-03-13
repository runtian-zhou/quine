use anyhow::Result;
use futures::stream::{BoxStream, StreamExt};
use reqwest_eventsource::{Event, EventSource};

use super::api_types::OpenAiStreamChunk;
use crate::types::{StopReason, StreamEvent};

pub fn parse_openai_stream(es: EventSource) -> BoxStream<'static, Result<StreamEvent>> {
    let stream = futures::stream::unfold(es, |mut es| async move {
        loop {
            let event = es.next().await?;
            match event {
                Ok(Event::Message(msg)) => {
                    if msg.data == "[DONE]" {
                        es.close();
                        return None;
                    }
                    let parsed: std::result::Result<OpenAiStreamChunk, _> =
                        serde_json::from_str(&msg.data);
                    match parsed {
                        Ok(chunk) => {
                            if let Some(stream_event) = convert_chunk(chunk) {
                                return Some((stream_event, es));
                            }
                            continue;
                        }
                        Err(e) => {
                            es.close();
                            return Some((
                                Err(anyhow::anyhow!("Failed to parse SSE: {}", e)),
                                es,
                            ));
                        }
                    }
                }
                Ok(Event::Open) => continue,
                Err(reqwest_eventsource::Error::StreamEnded) => {
                    return None;
                }
                Err(e) => {
                    es.close();
                    return Some((Err(anyhow::anyhow!("SSE error: {}", e)), es));
                }
            }
        }
    });

    Box::pin(stream)
}

fn convert_chunk(chunk: OpenAiStreamChunk) -> Option<Result<StreamEvent>> {
    let choice = chunk.choices.into_iter().next()?;

    if let Some(reason) = &choice.finish_reason {
        let stop = match reason.as_str() {
            "stop" => StopReason::EndTurn,
            "tool_calls" => StopReason::ToolUse,
            "length" => StopReason::MaxTokens,
            other => StopReason::Unknown(other.to_string()),
        };
        return Some(Ok(StreamEvent::Done(stop)));
    }

    if let Some(content) = choice.delta.content {
        if !content.is_empty() {
            return Some(Ok(StreamEvent::ContentDelta(content)));
        }
    }

    if let Some(tool_calls) = choice.delta.tool_calls {
        for tc in tool_calls {
            if let Some(id) = tc.id {
                let name = tc
                    .function
                    .as_ref()
                    .and_then(|f| f.name.clone())
                    .unwrap_or_default();
                return Some(Ok(StreamEvent::ToolCallStart { id, name }));
            }
            if let Some(func) = tc.function {
                if let Some(args) = func.arguments {
                    return Some(Ok(StreamEvent::ToolCallDelta(args)));
                }
            }
        }
    }

    None
}
