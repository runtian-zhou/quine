use std::path::Path;
use std::process::Stdio;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use quine_harness::protocol::{
    methods, JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};

const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);
const HEALTHCHECK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const DEBUG_INIT_ENV: &str = "QUINE_DEBUG_INIT";

fn debug_init_enabled() -> bool {
    std::env::var_os(DEBUG_INIT_ENV).is_some()
}

fn debug_init(message: impl AsRef<str>) {
    if debug_init_enabled() {
        eprintln!("[init] {}", message.as_ref());
    }
}

fn connect_error_kind(error: &anyhow::Error) -> Option<std::io::ErrorKind> {
    error
        .downcast_ref::<std::io::Error>()
        .map(std::io::Error::kind)
}

fn should_launch_daemon(error: &anyhow::Error) -> bool {
    matches!(
        connect_error_kind(error),
        Some(std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused)
    )
}

fn should_remove_stale_socket(socket_path: &Path, error: &anyhow::Error) -> bool {
    socket_path.exists()
        && matches!(
            connect_error_kind(error),
            Some(std::io::ErrorKind::ConnectionRefused)
        )
}

/// IPC client that connects to the harness daemon via Unix domain socket.
pub struct IpcClient {
    writer: tokio::io::WriteHalf<UnixStream>,
    /// Receives JSON-RPC responses (with an `id`).
    response_rx: mpsc::Receiver<String>,
    /// Receives JSON-RPC notifications (no `id`).
    notification_rx: mpsc::Receiver<JsonRpcNotification>,
    next_id: u64,
}

impl IpcClient {
    /// Connect to the harness daemon at the given socket path.
    pub async fn connect(socket_path: &Path) -> anyhow::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        let (reader, writer) = tokio::io::split(stream);

        let (response_tx, response_rx) = mpsc::channel::<String>(64);
        let (notification_tx, notification_rx) = mpsc::channel::<JsonRpcNotification>(256);

        // Spawn a reader task that routes incoming lines to either response or notification channel.
        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                // Try to determine if it's a response or notification by checking for "id".
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) {
                    if value.get("id").is_some() && !value.get("id").unwrap().is_null() {
                        // This is a response.
                        let _ = response_tx.send(line).await;
                    } else if value.get("method").is_some() {
                        // This is a notification.
                        if let Ok(notif) = serde_json::from_str::<JsonRpcNotification>(&line) {
                            let _ = notification_tx.send(notif).await;
                        }
                    }
                }
            }
        });

        Ok(Self {
            writer,
            response_rx,
            notification_rx,
            next_id: 1,
        })
    }

    /// Send a JSON-RPC request and wait for the response.
    pub async fn call(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> anyhow::Result<Result<serde_json::Value, String>> {
        self.call_with_timeout(method, params, REQUEST_TIMEOUT)
            .await
    }

    async fn call_with_timeout(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
        timeout: std::time::Duration,
    ) -> anyhow::Result<Result<serde_json::Value, String>> {
        let id = self.next_id;
        self.next_id += 1;

        let request = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');

        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        // Wait for a response with matching id.
        match tokio::time::timeout(timeout, self.response_rx.recv()).await {
            Ok(Some(resp_line)) => {
                // Try as success response first, then error.
                if let Ok(resp) = serde_json::from_str::<JsonRpcResponse>(&resp_line) {
                    Ok(Ok(resp.result))
                } else if let Ok(err) = serde_json::from_str::<JsonRpcErrorResponse>(&resp_line) {
                    Ok(Err(err.error.message))
                } else {
                    anyhow::bail!("unexpected response: {resp_line}")
                }
            }
            Ok(None) => anyhow::bail!("connection closed"),
            Err(_) => anyhow::bail!("request timed out"),
        }
    }

    /// Receive the next notification from the server.
    pub async fn recv_notification(&mut self) -> Option<JsonRpcNotification> {
        self.notification_rx.recv().await
    }

    async fn health_check(&mut self) -> anyhow::Result<()> {
        match self
            .call_with_timeout(methods::PING, None, HEALTHCHECK_TIMEOUT)
            .await?
        {
            Ok(_) => Ok(()),
            Err(message) => anyhow::bail!("daemon health check failed: {message}"),
        }
    }

    /// Connect to the daemon, launching it automatically if it is not running.
    ///
    /// Tries to connect to the socket. If the connection fails, spawns the
    /// daemon as a detached background process and polls until the socket
    /// accepts connections.
    /// Returns `(client, daemon_spawned)` where `daemon_spawned` is true if
    /// this call launched the daemon process.
    pub async fn connect_or_launch(socket_path: &Path) -> anyhow::Result<(Self, bool)> {
        match Self::connect(socket_path).await {
            Ok(mut client) => {
                debug_init(format!(
                    "connected to existing daemon socket at {}",
                    socket_path.display()
                ));
                client.health_check().await.map_err(|error| {
                    anyhow::anyhow!(
                        "connected to existing daemon at {} but it did not respond to a health check: {error}",
                        socket_path.display()
                    )
                })?;
                debug_init("existing daemon health check passed");
                return Ok((client, false));
            }
            Err(error) if should_launch_daemon(&error) => {
                if should_remove_stale_socket(socket_path, &error) {
                    let _ = tokio::fs::remove_file(socket_path).await;
                }
                debug_init(format!(
                    "launching daemon because initial connect failed at {}: {error}",
                    socket_path.display()
                ));
            }
            Err(error) => {
                return Err(error.context(format!(
                    "failed to connect to daemon at {}",
                    socket_path.display()
                )));
            }
        }

        eprintln!("Starting daemon...");

        let exe = std::env::current_exe()?;
        let socket_str = socket_path.to_string_lossy();
        let inherit_startup_logs = debug_init_enabled();

        let mut child = tokio::process::Command::new(&exe)
            .args(["daemon", "start", "--socket", &socket_str])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(if inherit_startup_logs {
                Stdio::inherit()
            } else {
                Stdio::null()
            })
            .process_group(0)
            .spawn()
            .map_err(|e| anyhow::anyhow!("failed to spawn daemon: {e}"))?;

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut interval = std::time::Duration::from_millis(50);

        loop {
            tokio::time::sleep(interval).await;

            match Self::connect(socket_path).await {
                Ok(mut client) => {
                    client.health_check().await.map_err(|error| {
                        anyhow::anyhow!(
                            "daemon at {} started listening but did not become healthy: {error}",
                            socket_path.display()
                        )
                    })?;
                    debug_init("launched daemon health check passed");
                    return Ok((client, true));
                }
                Err(error) if should_launch_daemon(&error) => {}
                Err(error) => {
                    return Err(error.context(format!(
                        "failed to connect to daemon at {} after launch",
                        socket_path.display()
                    )));
                }
            }

            if let Some(status) = child.try_wait()? {
                anyhow::bail!(
                    "daemon exited before socket was ready at {} with status {}",
                    socket_path.display(),
                    status
                );
            }

            if tokio::time::Instant::now() >= deadline {
                anyhow::bail!(
                    "timed out waiting for daemon to start at {}",
                    socket_path.display()
                );
            }

            interval = (interval * 2).min(std::time::Duration::from_millis(500));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quine_harness::protocol::JsonRpcRequest;
    use std::sync::{LazyLock, Mutex};
    use tempfile::{tempdir, NamedTempFile};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    fn with_env_lock<T>(f: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK.lock().unwrap();
        f()
    }

    #[test]
    fn request_id_increments() {
        let req1 = JsonRpcRequest::new(1, "test", None);
        let req2 = JsonRpcRequest::new(2, "test", None);
        assert_eq!(req1.id, serde_json::json!(1));
        assert_eq!(req2.id, serde_json::json!(2));
    }

    #[test]
    fn only_stale_socket_errors_trigger_cleanup_or_launch() {
        let not_found = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::NotFound));
        let refused =
            anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::ConnectionRefused));
        let denied = anyhow::Error::new(std::io::Error::from(std::io::ErrorKind::PermissionDenied));

        assert!(should_launch_daemon(&not_found));
        assert!(should_launch_daemon(&refused));
        assert!(!should_launch_daemon(&denied));

        let missing_dir = tempdir().unwrap();
        let missing_socket = missing_dir.path().join("missing.sock");
        assert!(!should_remove_stale_socket(
            missing_socket.as_path(),
            &not_found
        ));
        assert!(!should_remove_stale_socket(
            missing_socket.as_path(),
            &refused
        ));

        let existing_socket = NamedTempFile::new().unwrap();
        assert!(should_remove_stale_socket(existing_socket.path(), &refused));
        assert!(!should_remove_stale_socket(
            existing_socket.path(),
            &not_found
        ));
    }

    #[test]
    fn debug_init_env_flag_is_opt_in() {
        with_env_lock(|| {
            let previous = std::env::var_os(DEBUG_INIT_ENV);
            unsafe { std::env::remove_var(DEBUG_INIT_ENV) };
            assert!(!debug_init_enabled());
            unsafe { std::env::set_var(DEBUG_INIT_ENV, "1") };
            assert!(debug_init_enabled());
            match previous {
                Some(value) => unsafe { std::env::set_var(DEBUG_INIT_ENV, value) },
                None => unsafe { std::env::remove_var(DEBUG_INIT_ENV) },
            }
        });
    }
}
