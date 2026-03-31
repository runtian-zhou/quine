use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    socket_path: PathBuf,
    request_timeout: Duration,
}

impl ConnectionConfig {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
            request_timeout: Duration::from_secs(30),
        }
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    pub fn with_request_timeout(mut self, request_timeout: Duration) -> Self {
        self.request_timeout = request_timeout;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_defaults_are_stable() {
        let config = ConnectionConfig::new("/tmp/quine.sock");
        assert_eq!(config.socket_path(), Path::new("/tmp/quine.sock"));
        assert_eq!(config.request_timeout(), Duration::from_secs(30));
    }

    #[test]
    fn config_allows_timeout_override() {
        let config =
            ConnectionConfig::new("/tmp/quine.sock").with_request_timeout(Duration::from_secs(5));
        assert_eq!(config.request_timeout(), Duration::from_secs(5));
    }
}
