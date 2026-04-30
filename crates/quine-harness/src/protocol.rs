use serde::{Deserialize, Serialize};
use serde_json::Value;

/// JSON-RPC 2.0 request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    /// Create a new JSON-RPC 2.0 request.
    pub fn new(id: impl Into<Value>, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id: id.into(),
            method: method.into(),
            params,
        }
    }
}

/// JSON-RPC 2.0 success response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    pub result: Value,
}

impl JsonRpcResponse {
    /// Create a success response for the given request ID.
    pub fn success(id: Value, result: impl Serialize) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: serde_json::to_value(result).unwrap_or(Value::Null),
        }
    }
}

/// JSON-RPC 2.0 error response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcErrorResponse {
    pub jsonrpc: String,
    pub id: Value,
    pub error: JsonRpcError,
}

impl JsonRpcErrorResponse {
    /// Create an error response.
    pub fn new(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            error: JsonRpcError {
                code,
                message: message.into(),
                data: None,
            },
        }
    }
}

/// JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// JSON-RPC 2.0 notification (no id, no response expected).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    /// Create a new notification.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            method: method.into(),
            params,
        }
    }
}

/// Standard JSON-RPC error codes.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// Well-known RPC method names for the harness protocol.
pub mod methods {
    pub const PING: &str = "ping";
    pub const CREATE_SESSION: &str = "create_session";
    pub const EXIT_PLAN_MODE: &str = "exit_plan_mode";
    pub const SET_SESSION_MODEL_PROFILE: &str = "set_session_model_profile";
    pub const SEND_MESSAGE: &str = "send_message";
    pub const UNWIND_SESSION: &str = "unwind_session";
    pub const COMPACT_SESSION: &str = "compact_session";
    pub const SUBMIT_TOOL_RESULT: &str = "submit_tool_result";
    pub const CANCEL: &str = "cancel";
    pub const SHUTDOWN: &str = "shutdown";
    pub const SUBSCRIBE: &str = "subscribe";
    pub const LIST_SESSIONS: &str = "list_sessions";
    pub const GET_SESSION_LOG: &str = "get_session_log";
    pub const GET_SESSION_CONTEXT: &str = "get_session_context";
    pub const SUBMIT_INTERACTION_RESPONSE: &str = "submit_interaction_response";
    pub const SPAWN_SESSION: &str = "spawn_session";
    pub const SIGNAL_SESSION: &str = "signal_session";
    pub const SEND_IPC_MESSAGE: &str = "send_ipc_message";
    pub const RECV_IPC_MESSAGE: &str = "recv_ipc_message";
    pub const PYTHON_EXEC: &str = "python_exec";
    pub const PYTHON_LIST_GLOBALS: &str = "python_list_globals";
    pub const PYTHON_INSPECT_GLOBAL: &str = "python_inspect_global";
    pub const LIST_SKILLS: &str = "list_skills";
    pub const GET_SKILL: &str = "get_skill";
    pub const SCHEDULE_AGENT: &str = "schedule_agent";
}

/// Notification method names.
pub mod notifications {
    pub const REASONING_DELTA: &str = "reasoning_delta";
    pub const STREAM_DELTA: &str = "stream_delta";
    pub const TEXT_COMPLETE: &str = "text_complete";
    pub const TURN_STARTED: &str = "turn_started";
    pub const TOOL_REQUEST: &str = "tool_request";
    pub const TOOL_RESULT: &str = "tool_result";
    pub const SESSION_STATE_CHANGED: &str = "session_state_changed";
    pub const SESSION_ERROR: &str = "session_error";
    pub const INTERACTION_NEEDED: &str = "interaction_needed";
    pub const PLAN_PROGRESS: &str = "plan_progress";
    pub const SESSION_STATUS_REPORT: &str = "session_status_report";
    pub const TURN_COMPLETE: &str = "turn_complete";
    pub const CHILD_SPAWNED: &str = "child_spawned";
    pub const CHILD_EXITED: &str = "child_exited";
    pub const MESSAGE_RECEIVED: &str = "message_received";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_serialization() {
        let req = JsonRpcRequest::new(
            1,
            "create_session",
            Some(serde_json::json!({"system_prompt": "hello"})),
        );
        let json = serde_json::to_string(&req).unwrap();
        let parsed: JsonRpcRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "create_session");
        assert_eq!(parsed.jsonrpc, "2.0");
    }

    #[test]
    fn response_serialization() {
        let resp = JsonRpcResponse::success(serde_json::json!(1), "ok");
        let json = serde_json::to_string(&resp).unwrap();
        let parsed: JsonRpcResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.result, serde_json::json!("ok"));
    }

    #[test]
    fn error_response_serialization() {
        let err = JsonRpcErrorResponse::new(
            serde_json::json!(1),
            error_codes::METHOD_NOT_FOUND,
            "not found",
        );
        let json = serde_json::to_string(&err).unwrap();
        let parsed: JsonRpcErrorResponse = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.error.code, error_codes::METHOD_NOT_FOUND);
        assert_eq!(parsed.error.message, "not found");
    }

    #[test]
    fn notification_serialization() {
        let notif =
            JsonRpcNotification::new("stream_delta", Some(serde_json::json!({"delta": "hello"})));
        let json = serde_json::to_string(&notif).unwrap();
        let parsed: JsonRpcNotification = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.method, "stream_delta");
        assert!(parsed.params.is_some());
    }
}
