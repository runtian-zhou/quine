use std::fmt;

/// Errors that can occur during HTTP operations.
#[derive(Debug, Clone)]
pub enum HttpError {
    /// A network-level error (connection refused, timeout, etc.).
    Network(String),
    /// The server returned a non-success status code.
    Status(u16, String),
    /// An unexpected internal error.
    Internal(String),
}

impl fmt::Display for HttpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HttpError::Network(msg) => write!(f, "network error: {msg}"),
            HttpError::Status(code, msg) => write!(f, "HTTP {code}: {msg}"),
            HttpError::Internal(msg) => write!(f, "internal error: {msg}"),
        }
    }
}

impl std::error::Error for HttpError {}
