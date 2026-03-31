use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

pub(crate) type ResponseReceiver = mpsc::Receiver<String>;

pub(crate) struct Transport {
    writer: tokio::io::WriteHalf<UnixStream>,
}

impl Transport {
    pub(crate) async fn connect(
        socket_path: &std::path::Path,
    ) -> Result<(Self, ResponseReceiver), std::io::Error> {
        let stream = UnixStream::connect(socket_path).await?;
        let (reader, writer) = tokio::io::split(stream);
        let (response_tx, response_rx) = mpsc::channel::<String>(64);

        tokio::spawn(async move {
            let mut lines = BufReader::new(reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.trim().is_empty() {
                    continue;
                }
                if response_tx.send(line).await.is_err() {
                    break;
                }
            }
        });

        Ok((Self { writer }, response_rx))
    }

    pub(crate) async fn send_line(&mut self, line: &str) -> Result<(), std::io::Error> {
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await
    }

    pub(crate) async fn shutdown(&mut self) -> Result<(), std::io::Error> {
        self.writer.shutdown().await
    }
}
