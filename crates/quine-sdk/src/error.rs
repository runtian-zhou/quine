use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("failed to connect to harness socket {socket_path}: {source}")]
    Connect {
        socket_path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("connection is already closed")]
    Closed,
    #[error("request to method '{method}' timed out after {timeout_secs}s")]
    Timeout { method: String, timeout_secs: u64 },
    #[error("failed to write request: {0}")]
    Write(#[source] std::io::Error),
    #[error("failed to serialize request: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error("response channel closed")]
    ResponseChannelClosed,
    #[error("peer disconnected before responding")]
    Disconnected,
    #[error("malformed response payload: {0}")]
    MalformedResponse(String),
    #[error("json-rpc error {code}: {message}")]
    Rpc { code: i64, message: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_error_mentions_socket_path() {
        let error = ConnectionError::Connect {
            socket_path: PathBuf::from("/tmp/missing.sock"),
            source: std::io::Error::from(std::io::ErrorKind::NotFound),
        };
        assert!(error.to_string().contains("/tmp/missing.sock"));
    }

    #[test]
    fn timeout_error_mentions_method() {
        let error = RequestError::Timeout {
            method: "create_session".into(),
            timeout_secs: 3,
        };
        assert!(error.to_string().contains("create_session"));
    }
}
