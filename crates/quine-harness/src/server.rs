use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use crate::protocol::{
    error_codes, methods, notifications, JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse,
};
use crate::service::HarnessService;
use crate::session_log::{self, EventDirection, SessionLogEntry};

/// Run the IPC server on a Unix domain socket.
///
/// Listens for newline-delimited JSON-RPC 2.0 requests and dispatches them
/// to the provided `HarnessService` implementation.
pub async fn run_ipc_server(
    socket_path: &Path,
    service: Arc<dyn HarnessService>,
) -> anyhow::Result<()> {
    // Remove stale socket file if it exists.
    if socket_path.exists() {
        std::fs::remove_file(socket_path)?;
    }

    // Ensure parent directory exists.
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(socket_path)?;
    tracing::info!("IPC server listening on {:?}", socket_path);

    loop {
        let (stream, _addr) = listener.accept().await?;
        let svc = Arc::clone(&service);

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, svc).await {
                tracing::error!("connection error: {e}");
            }
        });
    }
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    service: Arc<dyn HarnessService>,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    // Spawn a task to forward broadcast events as notifications.
    let mut event_rx = service.subscribe();
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<String>(256);

    tokio::spawn(async move {
        while let Ok(event) = event_rx.recv().await {
            // Log the event asynchronously.
            log_core_output(&event).await;

            let notification = core_output_to_notification(&event);
            if let Ok(json) = serde_json::to_string(&notification) {
                if notify_tx.send(format!("{json}\n")).await.is_err() {
                    break;
                }
            }
        }
    });

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) if line.trim().is_empty() => continue,
                    Some(line) => {
                        let response = handle_request(&line, &*service).await;
                        if let Some(resp_str) = response {
                            writer.write_all(resp_str.as_bytes()).await?;
                            writer.write_all(b"\n").await?;
                            writer.flush().await?;
                        }
                    }
                    None => break, // Client disconnected.
                }
            }
            Some(notification) = notify_rx.recv() => {
                writer.write_all(notification.as_bytes()).await?;
                writer.flush().await?;
            }
        }
    }

    Ok(())
}

/// Log a CoreOutput event to the session's JSONL log file.
async fn log_core_output(event: &quine_core::CoreOutput) {
    let (session_id_str, event_type, payload) = match event {
        quine_core::CoreOutput::StreamDelta { session_id, delta } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "stream_delta",
            serde_json::json!({"delta": delta}),
        ),
        quine_core::CoreOutput::TextComplete {
            session_id,
            full_text,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "text_complete",
            serde_json::json!({"full_text": full_text}),
        ),
        quine_core::CoreOutput::ToolRequest {
            session_id,
            tool_use_id,
            tool_name,
            arguments,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "tool_request",
            serde_json::json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "arguments": arguments,
            }),
        ),
        quine_core::CoreOutput::SessionStateChanged { session_id, state } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "session_state_changed",
            serde_json::json!({"state": state}),
        ),
        quine_core::CoreOutput::SessionError { session_id, error } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "session_error",
            serde_json::json!({"error": error.to_string()}),
        ),
        quine_core::CoreOutput::InteractionNeeded {
            session_id,
            request,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "interaction_needed",
            serde_json::json!({
                "prompt": request.prompt,
                "kind": request.kind,
            }),
        ),
        quine_core::CoreOutput::TurnComplete { session_id } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "turn_complete",
            serde_json::json!({}),
        ),
    };

    let entry = SessionLogEntry {
        timestamp: Utc::now(),
        session_id: session_id_str,
        event_type: event_type.to_string(),
        direction: EventDirection::Outbound,
        payload,
    };

    if let Err(e) = session_log::append_log_entry(&entry).await {
        tracing::warn!("failed to write session log: {e}");
    }
}

/// Parse and dispatch a JSON-RPC request, returning the serialized response.
async fn handle_request(line: &str, service: &dyn HarnessService) -> Option<String> {
    let request: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(req) => req,
        Err(e) => {
            let err = JsonRpcErrorResponse::new(
                serde_json::Value::Null,
                error_codes::PARSE_ERROR,
                format!("parse error: {e}"),
            );
            return Some(serde_json::to_string(&err).unwrap_or_default());
        }
    };

    let id = request.id.clone();

    match request.method.as_str() {
        methods::CREATE_SESSION => {
            let system_prompt = request
                .params
                .as_ref()
                .and_then(|p| p.get("system_prompt"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let config = crate::config::SessionConfig {
                system_prompt,
                working_directory: None,
            };

            match service.create_session(config).await {
                Ok(session_id) => {
                    // Log session creation.
                    let sid_str = serde_json::to_value(session_id)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from))
                        .unwrap_or_default();
                    let entry = SessionLogEntry {
                        timestamp: Utc::now(),
                        session_id: sid_str,
                        event_type: "session_created".to_string(),
                        direction: EventDirection::Inbound,
                        payload: serde_json::json!({}),
                    };
                    let _ = session_log::append_log_entry(&entry).await;

                    let resp = JsonRpcResponse::success(id, session_id);
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
                Err(e) => {
                    let resp =
                        JsonRpcErrorResponse::new(id, error_codes::INTERNAL_ERROR, e.to_string());
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SEND_MESSAGE => {
            let params = request.params.as_ref();
            let session_id_str = params
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());
            let content = params
                .and_then(|p| p.get("content"))
                .and_then(|v| v.as_str());

            match (session_id_str, content) {
                (Some(sid), Some(content)) => {
                    let session_id: quine_core::SessionId =
                        match serde_json::from_value(serde_json::Value::String(sid.to_string())) {
                            Ok(id) => id,
                            Err(e) => {
                                let resp = JsonRpcErrorResponse::new(
                                    id,
                                    error_codes::INVALID_PARAMS,
                                    format!("invalid session_id: {e}"),
                                );
                                return Some(serde_json::to_string(&resp).unwrap_or_default());
                            }
                        };

                    // Log user message.
                    let entry = SessionLogEntry {
                        timestamp: Utc::now(),
                        session_id: sid.to_string(),
                        event_type: "user_message".to_string(),
                        direction: EventDirection::Inbound,
                        payload: serde_json::json!({"content": content}),
                    };
                    let _ = session_log::append_log_entry(&entry).await;

                    match service.send_message(session_id, content.to_string()).await {
                        Ok(()) => {
                            let resp = JsonRpcResponse::success(id, "ok");
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                        Err(e) => {
                            let resp = JsonRpcErrorResponse::new(
                                id,
                                error_codes::INTERNAL_ERROR,
                                e.to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                    }
                }
                _ => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        "missing session_id or content",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SHUTDOWN => match service.shutdown().await {
            Ok(()) => {
                let resp = JsonRpcResponse::success(id, "ok");
                Some(serde_json::to_string(&resp).unwrap_or_default())
            }
            Err(e) => {
                let resp =
                    JsonRpcErrorResponse::new(id, error_codes::INTERNAL_ERROR, e.to_string());
                Some(serde_json::to_string(&resp).unwrap_or_default())
            }
        },

        methods::CANCEL => {
            let params = request.params.as_ref();
            let session_id_str = params
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());

            match session_id_str {
                Some(sid) => {
                    let session_id: quine_core::SessionId =
                        match serde_json::from_value(serde_json::Value::String(sid.to_string())) {
                            Ok(id) => id,
                            Err(e) => {
                                let resp = JsonRpcErrorResponse::new(
                                    id,
                                    error_codes::INVALID_PARAMS,
                                    format!("invalid session_id: {e}"),
                                );
                                return Some(serde_json::to_string(&resp).unwrap_or_default());
                            }
                        };

                    match service.cancel(session_id).await {
                        Ok(()) => {
                            let resp = JsonRpcResponse::success(id, "ok");
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                        Err(e) => {
                            let resp = JsonRpcErrorResponse::new(
                                id,
                                error_codes::INTERNAL_ERROR,
                                e.to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                    }
                }
                None => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        "missing session_id",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SUBMIT_INTERACTION_RESPONSE => {
            let params = request.params.as_ref();
            let session_id_str = params
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());
            let response_text = params
                .and_then(|p| p.get("response"))
                .and_then(|v| v.as_str());

            match (session_id_str, response_text) {
                (Some(sid), Some(response)) => {
                    let session_id: quine_core::SessionId =
                        match serde_json::from_value(serde_json::Value::String(sid.to_string())) {
                            Ok(id) => id,
                            Err(e) => {
                                let resp = JsonRpcErrorResponse::new(
                                    id,
                                    error_codes::INVALID_PARAMS,
                                    format!("invalid session_id: {e}"),
                                );
                                return Some(serde_json::to_string(&resp).unwrap_or_default());
                            }
                        };

                    let interaction_response = quine_core::InteractionResponse {
                        response: response.to_string(),
                    };

                    match service
                        .submit_interaction_response(session_id, interaction_response)
                        .await
                    {
                        Ok(()) => {
                            let resp = JsonRpcResponse::success(id, "ok");
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                        Err(e) => {
                            let resp = JsonRpcErrorResponse::new(
                                id,
                                error_codes::INTERNAL_ERROR,
                                e.to_string(),
                            );
                            Some(serde_json::to_string(&resp).unwrap_or_default())
                        }
                    }
                }
                _ => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        "missing session_id or response",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::LIST_SESSIONS => match session_log::list_sessions().await {
            Ok(summaries) => {
                let resp = JsonRpcResponse::success(
                    id,
                    serde_json::to_value(&summaries).unwrap_or_default(),
                );
                Some(serde_json::to_string(&resp).unwrap_or_default())
            }
            Err(e) => {
                let resp =
                    JsonRpcErrorResponse::new(id, error_codes::INTERNAL_ERROR, e.to_string());
                Some(serde_json::to_string(&resp).unwrap_or_default())
            }
        },

        methods::GET_SESSION_LOG => {
            let session_id_str = request
                .params
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());

            match session_id_str {
                Some(sid) => match session_log::read_session_log(sid).await {
                    Ok(entries) => {
                        let resp = JsonRpcResponse::success(
                            id,
                            serde_json::to_value(&entries).unwrap_or_default(),
                        );
                        Some(serde_json::to_string(&resp).unwrap_or_default())
                    }
                    Err(e) => {
                        let resp = JsonRpcErrorResponse::new(
                            id,
                            error_codes::INTERNAL_ERROR,
                            e.to_string(),
                        );
                        Some(serde_json::to_string(&resp).unwrap_or_default())
                    }
                },
                None => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        "missing session_id",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        _ => {
            let resp = JsonRpcErrorResponse::new(
                id,
                error_codes::METHOD_NOT_FOUND,
                format!("unknown method: {}", request.method),
            );
            Some(serde_json::to_string(&resp).unwrap_or_default())
        }
    }
}

/// Convert a `CoreOutput` event to a JSON-RPC notification.
fn core_output_to_notification(event: &quine_core::CoreOutput) -> JsonRpcNotification {
    match event {
        quine_core::CoreOutput::StreamDelta { session_id, delta } => JsonRpcNotification::new(
            notifications::STREAM_DELTA,
            Some(serde_json::json!({
                "session_id": session_id,
                "delta": delta,
            })),
        ),
        quine_core::CoreOutput::TextComplete {
            session_id,
            full_text,
        } => JsonRpcNotification::new(
            notifications::TEXT_COMPLETE,
            Some(serde_json::json!({
                "session_id": session_id,
                "full_text": full_text,
            })),
        ),
        quine_core::CoreOutput::ToolRequest {
            session_id,
            tool_use_id,
            tool_name,
            arguments,
        } => JsonRpcNotification::new(
            notifications::TOOL_REQUEST,
            Some(serde_json::json!({
                "session_id": session_id,
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "arguments": arguments,
            })),
        ),
        quine_core::CoreOutput::SessionStateChanged { session_id, state } => {
            JsonRpcNotification::new(
                notifications::SESSION_STATE_CHANGED,
                Some(serde_json::json!({
                    "session_id": session_id,
                    "state": state,
                })),
            )
        }
        quine_core::CoreOutput::SessionError { session_id, error } => JsonRpcNotification::new(
            notifications::SESSION_ERROR,
            Some(serde_json::json!({
                "session_id": session_id,
                "error": error.to_string(),
            })),
        ),
        quine_core::CoreOutput::InteractionNeeded {
            session_id,
            request,
        } => JsonRpcNotification::new(
            notifications::INTERACTION_NEEDED,
            Some(serde_json::json!({
                "session_id": session_id,
                "prompt": request.prompt,
                "kind": request.kind,
            })),
        ),
        quine_core::CoreOutput::TurnComplete { session_id } => JsonRpcNotification::new(
            notifications::TURN_COMPLETE,
            Some(serde_json::json!({
                "session_id": session_id,
            })),
        ),
    }
}
