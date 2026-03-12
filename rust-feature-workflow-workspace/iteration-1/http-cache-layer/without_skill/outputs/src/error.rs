use thiserror::Error;

/// Errors that can occur in the cached HTTP client.
#[derive(Debug, Error)]
pub enum CacheError {
    #[error("HTTP request failed: {0}")]
    RequestError(#[from] reqwest::Error),

    #[error("Lock poisoned: {0}")]
    LockPoisoned(String),
}
