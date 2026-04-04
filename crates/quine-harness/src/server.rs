use std::path::Path;
use std::sync::Arc;

use chrono::Utc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Notify;

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

    let shutdown_signal = Arc::new(Notify::new());

    loop {
        tokio::select! {
            result = listener.accept() => {
                let (stream, _addr) = result?;
                let svc = Arc::clone(&service);
                let shutdown = Arc::clone(&shutdown_signal);

                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, svc, shutdown).await {
                        tracing::error!("connection error: {e}");
                    }
                });
            }
            _ = shutdown_signal.notified() => {
                tracing::info!("IPC server shutting down");
                // Clean up socket file.
                let _ = std::fs::remove_file(socket_path);
                break;
            }
        }
    }

    Ok(())
}

/// Handle a single client connection.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    service: Arc<dyn HarnessService>,
    shutdown_signal: Arc<Notify>,
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
                        let response = handle_request(&line, &*service, &shutdown_signal).await;
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
        quine_core::CoreOutput::ReasoningDelta { session_id, delta } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "reasoning_delta",
            serde_json::json!({"delta": delta}),
        ),
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
                "options": request.options,
                "allow_freeform": request.allow_freeform,
            }),
        ),
        quine_core::CoreOutput::PlanProgress {
            session_id,
            plan_id,
            action_id,
            status,
            remaining,
            total,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "plan_progress",
            serde_json::json!({
                "plan_id": plan_id,
                "action_id": action_id,
                "status": status,
                "remaining": remaining,
                "total": total,
            }),
        ),
        quine_core::CoreOutput::ToolResult {
            session_id,
            tool_use_id,
            tool_name,
            content,
            is_error,
            duration_us,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "tool_result",
            serde_json::json!({
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "content": content,
                "is_error": is_error,
                "duration_us": duration_us,
            }),
        ),
        quine_core::CoreOutput::TurnComplete {
            session_id,
            duration_us,
            usage,
        } => {
            let mut payload = serde_json::json!({
                "duration_us": duration_us,
            });
            if let Some(u) = usage {
                payload["usage"] = serde_json::json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                });
            }
            (
                serde_json::to_value(session_id)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                "turn_complete",
                payload,
            )
        }
        quine_core::CoreOutput::ChildSpawned {
            parent_id,
            child_id,
        } => (
            serde_json::to_value(parent_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "child_spawned",
            serde_json::json!({"child_id": child_id}),
        ),
        quine_core::CoreOutput::ChildExited {
            parent_id,
            child_id,
            status,
        } => (
            serde_json::to_value(parent_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "child_exited",
            serde_json::json!({"child_id": child_id, "status": status}),
        ),
        quine_core::CoreOutput::MessageReceived {
            session_id,
            from,
            content,
        } => (
            serde_json::to_value(session_id)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default(),
            "message_received",
            serde_json::json!({"from": from, "content": content}),
        ),
        quine_core::CoreOutput::CheckpointRequested { .. } => {
            return;
        }
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
async fn handle_request(
    line: &str,
    service: &dyn HarnessService,
    shutdown_signal: &Notify,
) -> Option<String> {
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

            let skills: Vec<String> = request
                .params
                .as_ref()
                .and_then(|p| p.get("skills"))
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();

            let plan_mode = request
                .params
                .as_ref()
                .and_then(|p| p.get("plan_mode"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let initial_messages = request
                .params
                .as_ref()
                .and_then(|p| p.get("initial_messages"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .unwrap_or_default()
                .unwrap_or_default();

            let agent_key = request
                .params
                .as_ref()
                .and_then(|p| p.get("agent_key"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let team_key = request
                .params
                .as_ref()
                .and_then(|p| p.get("team_key"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let memory_policy = request
                .params
                .as_ref()
                .and_then(|p| p.get("memory_policy"))
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .unwrap_or_default()
                .unwrap_or_default();

            let config = crate::config::SessionConfig {
                system_prompt,
                working_directory: None,
                skills,
                plan_mode,
                initial_messages,
                agent_key,
                team_key,
                memory_policy,
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
                        session_id: sid_str.clone(),
                        event_type: "session_created".to_string(),
                        direction: EventDirection::Inbound,
                        payload: serde_json::json!({}),
                    };
                    let _ = session_log::append_log_entry(&entry).await;

                    let resp = JsonRpcResponse::success(
                        id,
                        serde_json::json!({
                            "session_id": sid_str,
                            "max_context_window": crate::config::max_context_window_from_env(),
                        }),
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
                Err(e) => {
                    let resp =
                        JsonRpcErrorResponse::new(id, error_codes::INTERNAL_ERROR, e.to_string());
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::EXIT_PLAN_MODE => {
            let session_id = match request
                .params
                .as_ref()
                .and_then(|p| p.get("session_id"))
                .cloned()
                .map(serde_json::from_value::<quine_core::SessionId>)
                .transpose()
            {
                Ok(Some(session_id)) => session_id,
                Ok(None) => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        "missing session_id",
                    );
                    return Some(serde_json::to_string(&resp).unwrap_or_default());
                }
                Err(error) => {
                    let resp = JsonRpcErrorResponse::new(
                        id,
                        error_codes::INVALID_PARAMS,
                        format!("invalid session_id: {error}"),
                    );
                    return Some(serde_json::to_string(&resp).unwrap_or_default());
                }
            };

            match service.exit_plan_mode(session_id).await {
                Ok(()) => Some(
                    serde_json::to_string(&JsonRpcResponse::success(id, serde_json::json!(null)))
                        .unwrap_or_default(),
                ),
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

        methods::COMPACT_SESSION => {
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

                    match service.compact_session(session_id).await {
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

        methods::GET_SESSION_CONTEXT => {
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

                    match service.get_session_context(session_id).await {
                        Ok(checkpoint) => {
                            let live_states = service
                                .list_sessions()
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .filter_map(|value| {
                                    Some((
                                        serde_json::from_value(serde_json::Value::String(
                                            value.get("session_id")?.as_str()?.to_string(),
                                        ))
                                        .ok()?,
                                        value.get("status")?.as_str()?.to_string(),
                                    ))
                                })
                                .collect();
                            match crate::storage::session_context_from_checkpoint(
                                &checkpoint,
                                session_id,
                                &live_states,
                            ) {
                                Some(snapshot) => {
                                    let resp = JsonRpcResponse::success(id, snapshot);
                                    Some(serde_json::to_string(&resp).unwrap_or_default())
                                }
                                None => {
                                    let resp = JsonRpcErrorResponse::new(
                                        id,
                                        error_codes::INTERNAL_ERROR,
                                        "session context missing from checkpoint",
                                    );
                                    Some(serde_json::to_string(&resp).unwrap_or_default())
                                }
                            }
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

        methods::SHUTDOWN => match service.shutdown().await {
            Ok(()) => {
                let resp = JsonRpcResponse::success(id, "ok");
                let result = Some(serde_json::to_string(&resp).unwrap_or_default());
                // Signal the IPC server to stop accepting connections and exit.
                shutdown_signal.notify_one();
                result
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
                        selected_indices: Vec::new(),
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

        methods::LIST_SESSIONS => match service.list_sessions().await {
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

        methods::SPAWN_SESSION => {
            let params = request.params.as_ref();
            let task = params
                .and_then(|p| p.get("task"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let parent_id_str = params
                .and_then(|p| p.get("parent_id"))
                .and_then(|v| v.as_str());
            let system_prompt = params
                .and_then(|p| p.get("system_prompt"))
                .and_then(|v| v.as_str())
                .map(String::from);

            match task {
                Some(task) => {
                    let parent_id: Option<quine_core::SessionId> = parent_id_str.and_then(|s| {
                        serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
                    });

                    match service
                        .spawn_child_session(parent_id, task, system_prompt)
                        .await
                    {
                        Ok(session_id) => {
                            let resp = JsonRpcResponse::success(id, session_id);
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
                        "missing task parameter",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SIGNAL_SESSION => {
            let params = request.params.as_ref();
            let session_id_str = params
                .and_then(|p| p.get("session_id"))
                .and_then(|v| v.as_str());
            let signal_str = params
                .and_then(|p| p.get("signal"))
                .and_then(|v| v.as_str());

            match (session_id_str, signal_str) {
                (Some(sid), Some(sig)) => {
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

                    let signal: quine_core::SessionSignal = match sig {
                        "term" | "Term" => quine_core::SessionSignal::Term,
                        "kill" | "Kill" => quine_core::SessionSignal::Kill,
                        "stop" | "Stop" => quine_core::SessionSignal::Stop,
                        "continue" | "Continue" => quine_core::SessionSignal::Continue,
                        _ => {
                            let resp = JsonRpcErrorResponse::new(
                                id,
                                error_codes::INVALID_PARAMS,
                                format!("unknown signal: {sig}"),
                            );
                            return Some(serde_json::to_string(&resp).unwrap_or_default());
                        }
                    };

                    match service.signal_session(session_id, signal).await {
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
                        "missing session_id or signal",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SEND_IPC_MESSAGE => {
            let params = request.params.as_ref();
            let target = params
                .and_then(|p| p.get("target"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let content = params
                .and_then(|p| p.get("content"))
                .and_then(|v| v.as_str())
                .map(String::from);

            match (target, content) {
                (Some(target), Some(content)) => {
                    match service.send_ipc_message(target, content).await {
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
                        "missing target or content",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::RECV_IPC_MESSAGE => {
            let params = request.params.as_ref();
            let source = params
                .and_then(|p| p.get("source"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let non_blocking = params
                .and_then(|p| p.get("non_blocking"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            match source {
                Some(source) => match service.recv_ipc_message(source, non_blocking).await {
                    Ok(msg) => {
                        let resp = JsonRpcResponse::success(id, msg);
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
                        "missing source",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::SCHEDULE_AGENT => {
            let params = request.params.as_ref();
            let task = params
                .and_then(|p| p.get("task"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let system_prompt = params
                .and_then(|p| p.get("system_prompt"))
                .and_then(|v| v.as_str())
                .map(String::from);
            let parent_id = params
                .and_then(|p| p.get("parent_id"))
                .and_then(|v| v.as_str())
                .and_then(|s| {
                    serde_json::from_value(serde_json::Value::String(s.to_string())).ok()
                });
            let delay_secs = params
                .and_then(|p| p.get("delay_secs"))
                .and_then(|v| v.as_u64());
            let cadence_secs = params
                .and_then(|p| p.get("cadence_secs"))
                .and_then(|v| v.as_u64());

            match (task, delay_secs) {
                (Some(task), Some(delay_secs)) => {
                    match service
                        .schedule_agent(
                            parent_id,
                            task,
                            system_prompt,
                            tokio::time::Duration::from_secs(delay_secs),
                            cadence_secs.map(tokio::time::Duration::from_secs),
                        )
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
                        "missing task or delay_secs",
                    );
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::LIST_SKILLS => {
            let project_root = std::env::current_dir().unwrap_or_default();
            match quine_core::list_available_skills(&project_root).await {
                Ok(skills) => {
                    let result = skills
                        .into_iter()
                        .map(|skill| {
                            serde_json::json!({
                                "name": skill.name,
                                "description": skill.description,
                                "version": skill.version,
                            })
                        })
                        .collect::<Vec<_>>();
                    let resp = JsonRpcResponse::success(id, result);
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
                Err(e) => {
                    let resp =
                        JsonRpcErrorResponse::new(id, error_codes::INTERNAL_ERROR, e.to_string());
                    Some(serde_json::to_string(&resp).unwrap_or_default())
                }
            }
        }

        methods::GET_SKILL => {
            let skill_name = request
                .params
                .as_ref()
                .and_then(|p| p.get("name"))
                .and_then(|v| v.as_str());

            match skill_name {
                Some(name) => {
                    let project_root = std::env::current_dir().unwrap_or_default();
                    match quine_core::load_skill(&project_root, name).await {
                        Ok(skill) => {
                            let result = serde_json::json!({
                                "name": skill.meta.name,
                                "description": skill.meta.description,
                                "version": skill.meta.version,
                                "source_path": skill.source_path.to_string_lossy(),
                                "system_prompt": skill.system_prompt,
                                "tools": skill.tool_definitions.iter().map(|t| {
                                    serde_json::json!({
                                        "name": t.name,
                                        "description": t.description,
                                        "handler": t.handler,
                                        "parameters": t.parameters,
                                    })
                                }).collect::<Vec<_>>(),
                            });
                            let resp = JsonRpcResponse::success(id, result);
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
                        "missing name parameter",
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
        quine_core::CoreOutput::ReasoningDelta { session_id, delta } => JsonRpcNotification::new(
            notifications::REASONING_DELTA,
            Some(serde_json::json!({
                "session_id": session_id,
                "delta": delta,
            })),
        ),
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
        } => {
            let mut params = serde_json::json!({
                "session_id": session_id,
                "prompt": request.prompt,
                "kind": request.kind,
                "options": request.options,
                "allow_freeform": request.allow_freeform,
            });
            if let Some(label) = &request.source_label {
                params["source_label"] = serde_json::Value::String(label.clone());
            }
            JsonRpcNotification::new(notifications::INTERACTION_NEEDED, Some(params))
        }
        quine_core::CoreOutput::PlanProgress {
            session_id,
            plan_id,
            action_id,
            status,
            remaining,
            total,
        } => JsonRpcNotification::new(
            notifications::PLAN_PROGRESS,
            Some(serde_json::json!({
                "session_id": session_id,
                "plan_id": plan_id,
                "action_id": action_id,
                "status": status,
                "remaining": remaining,
                "total": total,
            })),
        ),
        quine_core::CoreOutput::ToolResult {
            session_id,
            tool_use_id,
            tool_name,
            content,
            is_error,
            duration_us,
        } => JsonRpcNotification::new(
            notifications::TOOL_RESULT,
            Some(serde_json::json!({
                "session_id": session_id,
                "tool_use_id": tool_use_id,
                "tool_name": tool_name,
                "content": content,
                "is_error": is_error,
                "duration_us": duration_us,
            })),
        ),
        quine_core::CoreOutput::TurnComplete {
            session_id,
            duration_us,
            usage,
        } => {
            let mut params = serde_json::json!({
                "session_id": session_id,
                "duration_us": duration_us,
            });
            if let Some(u) = usage {
                params["usage"] = serde_json::json!({
                    "input_tokens": u.input_tokens,
                    "output_tokens": u.output_tokens,
                });
            }
            JsonRpcNotification::new(notifications::TURN_COMPLETE, Some(params))
        }
        quine_core::CoreOutput::ChildSpawned {
            parent_id,
            child_id,
        } => JsonRpcNotification::new(
            notifications::CHILD_SPAWNED,
            Some(serde_json::json!({
                "parent_id": parent_id,
                "child_id": child_id,
            })),
        ),
        quine_core::CoreOutput::ChildExited {
            parent_id,
            child_id,
            status,
        } => JsonRpcNotification::new(
            notifications::CHILD_EXITED,
            Some(serde_json::json!({
                "parent_id": parent_id,
                "child_id": child_id,
                "status": status,
            })),
        ),
        quine_core::CoreOutput::MessageReceived {
            session_id,
            from,
            content,
        } => JsonRpcNotification::new(
            notifications::MESSAGE_RECEIVED,
            Some(serde_json::json!({
                "session_id": session_id,
                "from": from,
                "content": content,
            })),
        ),
        quine_core::CoreOutput::CheckpointRequested { .. } => {
            JsonRpcNotification::new("internal/checkpoint_requested", None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interaction_needed_notification_includes_options() {
        let session_id = quine_core::SessionId::new();
        let event = quine_core::CoreOutput::InteractionNeeded {
            session_id,
            request: quine_core::tool::InteractionRequest {
                prompt: "Pick a color".into(),
                kind: quine_core::tool::InteractionKind::SingleSelect,
                options: vec![
                    quine_core::tool::SelectOption {
                        label: "red".into(),
                        description: None,
                    },
                    quine_core::tool::SelectOption {
                        label: "green".into(),
                        description: None,
                    },
                    quine_core::tool::SelectOption {
                        label: "blue".into(),
                        description: None,
                    },
                ],
                allow_freeform: true,
                source_label: None,
            },
        };

        let notif = core_output_to_notification(&event);
        let params = notif.params.expect("params should be present");

        // Verify options are included.
        let options = params.get("options").expect("options field missing");
        let arr = options.as_array().expect("options should be an array");
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0]["label"], "red");
        assert_eq!(arr[1]["label"], "green");
        assert_eq!(arr[2]["label"], "blue");

        // Verify allow_freeform is included.
        let allow_freeform = params
            .get("allow_freeform")
            .expect("allow_freeform field missing");
        assert_eq!(allow_freeform, true);
    }
}
