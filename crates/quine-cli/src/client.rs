use std::path::Path;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use quine_harness::protocol::{
    JsonRpcErrorResponse, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};

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
        let id = self.next_id;
        self.next_id += 1;

        let request = JsonRpcRequest::new(id, method, params);
        let mut line = serde_json::to_string(&request)?;
        line.push('\n');

        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await?;

        // Wait for a response with matching id.
        match tokio::time::timeout(std::time::Duration::from_secs(30), self.response_rx.recv())
            .await
        {
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
}

#[cfg(test)]
mod tests {
    use quine_harness::protocol::JsonRpcRequest;

    #[test]
    fn request_id_increments() {
        let req1 = JsonRpcRequest::new(1, "test", None);
        let req2 = JsonRpcRequest::new(2, "test", None);
        assert_eq!(req1.id, serde_json::json!(1));
        assert_eq!(req2.id, serde_json::json!(2));
    }
}
