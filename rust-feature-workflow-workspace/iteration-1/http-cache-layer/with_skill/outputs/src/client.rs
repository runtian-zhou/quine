use crate::error::HttpError;

/// An HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status code.
    pub status: u16,
    /// Response headers as key-value pairs.
    pub headers: Vec<(String, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
}

/// Trait abstracting an HTTP client.
///
/// Implementations handle the actual network I/O. The [`CachedClient`](crate::CachedClient)
/// wraps any `HttpClient` to add transparent caching for GET requests.
pub trait HttpClient: Send + Sync {
    /// Send a GET request.
    fn get(&self, url: &str) -> Result<Response, HttpError>;

    /// Send a POST request with a body.
    fn post(&self, url: &str, body: &[u8]) -> Result<Response, HttpError>;

    /// Send a PUT request with a body.
    fn put(&self, url: &str, body: &[u8]) -> Result<Response, HttpError>;

    /// Send a DELETE request.
    fn delete(&self, url: &str) -> Result<Response, HttpError>;
}

/// A minimal HTTP client that performs real network requests.
///
/// In a real project this would wrap `reqwest`, `ureq`, or similar.
/// Here it serves as a placeholder demonstrating the trait.
pub struct SimpleHttpClient;

impl SimpleHttpClient {
    pub fn new() -> Self {
        SimpleHttpClient
    }
}

impl Default for SimpleHttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient for SimpleHttpClient {
    fn get(&self, _url: &str) -> Result<Response, HttpError> {
        // Placeholder -- a real implementation would use reqwest/ureq.
        Err(HttpError::Internal("SimpleHttpClient is a stub".into()))
    }

    fn post(&self, _url: &str, _body: &[u8]) -> Result<Response, HttpError> {
        Err(HttpError::Internal("SimpleHttpClient is a stub".into()))
    }

    fn put(&self, _url: &str, _body: &[u8]) -> Result<Response, HttpError> {
        Err(HttpError::Internal("SimpleHttpClient is a stub".into()))
    }

    fn delete(&self, _url: &str) -> Result<Response, HttpError> {
        Err(HttpError::Internal("SimpleHttpClient is a stub".into()))
    }
}
