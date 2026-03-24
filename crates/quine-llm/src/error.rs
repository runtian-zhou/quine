/// Errors specific to the LLM adapter layer.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    /// The provider returned an HTTP error.
    #[error("provider HTTP error (status {status}): {body}")]
    ProviderHttp { status: u16, body: String },

    /// Failed to parse a streaming event from the provider.
    #[error("failed to parse streaming event: {message}")]
    ParseError { message: String },

    /// The provider configuration is invalid.
    #[error("invalid provider config: {message}")]
    InvalidConfig { message: String },

    /// Network or connection error.
    #[error("connection error: {source}")]
    Connection {
        #[from]
        source: reqwest::Error,
    },
}
