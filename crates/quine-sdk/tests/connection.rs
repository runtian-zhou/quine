use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use quine_harness::local::LocalHarness;
use quine_harness::server::run_ipc_server;
use quine_harness::StorageManager;
use quine_llm::{LlmEvent, LlmProvider, Message, ToolDefinition};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;

use quine_sdk::{ConnectionConfig, QuineClient, RequestError};

fn temp_socket_path(name: &str) -> std::path::PathBuf {
    let short_id = uuid::Uuid::new_v4().simple().to_string();
    std::path::PathBuf::from(format!("/tmp/{}-{}.sock", name, &short_id[..8]))
}

fn temp_storage_root(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{name}-{}", uuid::Uuid::new_v4()))
}

fn socket_setup_is_unsupported(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::Unsupported
    )
}

async fn require_unix_socket_support(test_name: &str) -> Option<()> {
    let socket_path = temp_socket_path(test_name);
    let listener = match UnixListener::bind(&socket_path) {
        Ok(listener) => listener,
        Err(error) if socket_setup_is_unsupported(&error) => {
            eprintln!(
                "skipping {test_name}: unix socket setup unavailable in this environment: {error}"
            );
            return None;
        }
        Err(error) => panic!(
            "failed to bind test unix socket {}: {error}",
            socket_path.display()
        ),
    };
    drop(listener);
    let _ = tokio::fs::remove_file(&socket_path).await;
    Some(())
}

fn bind_test_listener(socket_path: &std::path::Path) -> Option<UnixListener> {
    match UnixListener::bind(socket_path) {
        Ok(listener) => Some(listener),
        Err(error) if socket_setup_is_unsupported(&error) => {
            eprintln!(
                "skipping unix socket test for {}: {error}",
                socket_path.display()
            );
            None
        }
        Err(error) => panic!(
            "failed to bind test unix socket {}: {error}",
            socket_path.display()
        ),
    }
}

async fn wait_for_socket(socket_path: &std::path::Path) {
    for _ in 0..50 {
        if socket_path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("timed out waiting for socket {}", socket_path.display());
}

#[tokio::test]
async fn connect_and_close_cleanly() {
    if require_unix_socket_support("quine-sdk-connect-support")
        .await
        .is_none()
    {
        return;
    }
    let socket_path = temp_socket_path("quine-sdk-connect");
    let Some(listener) = bind_test_listener(&socket_path) else {
        return;
    };
    let accept_task = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    });

    let mut client = QuineClient::connect(&socket_path).await.unwrap();
    assert!(client.is_connected());
    client.close().await.unwrap();
    assert!(!client.is_connected());

    accept_task.await.unwrap();
    let _ = tokio::fs::remove_file(&socket_path).await;
}

#[tokio::test]
async fn connect_fails_for_missing_socket() {
    let socket_path = temp_socket_path("quine-sdk-missing");
    let error = match QuineClient::connect(&socket_path).await {
        Ok(_) => panic!("expected connection failure"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("failed to connect"));
}

#[tokio::test]
async fn early_disconnect() {
    if require_unix_socket_support("quine-sdk-early-disconnect-support")
        .await
        .is_none()
    {
        return;
    }
    let socket_path = temp_socket_path("quine-sdk-early-disconnect");
    let Some(listener) = bind_test_listener(&socket_path) else {
        return;
    };
    let accept_task = tokio::spawn(async move {
        let (_stream, _) = listener.accept().await.unwrap();
    });

    let mut client = QuineClient::connect(&socket_path).await.unwrap();
    let error = client
        .request_raw::<serde_json::Value>("ping", None)
        .await
        .unwrap_err();
    assert!(matches!(error, RequestError::Disconnected));

    accept_task.await.unwrap();
    let _ = tokio::fs::remove_file(&socket_path).await;
}

#[tokio::test]
async fn malformed_response() {
    if require_unix_socket_support("quine-sdk-malformed-support")
        .await
        .is_none()
    {
        return;
    }
    let socket_path = temp_socket_path("quine-sdk-malformed");
    let Some(listener) = bind_test_listener(&socket_path) else {
        return;
    };
    let accept_task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (_reader, mut writer) = tokio::io::split(stream);
        writer.write_all(b"{not-json}\n").await.unwrap();
        writer.flush().await.unwrap();
    });

    let mut client = QuineClient::connect(&socket_path).await.unwrap();
    let error = client
        .request_raw::<serde_json::Value>("ping", None)
        .await
        .unwrap_err();
    assert!(matches!(error, RequestError::MalformedResponse(_)));

    accept_task.await.unwrap();
    let _ = tokio::fs::remove_file(&socket_path).await;
}

#[tokio::test]
async fn real_daemon_connect() {
    if require_unix_socket_support("quine-sdk-daemon-connect-support")
        .await
        .is_none()
    {
        return;
    }
    let socket_path = temp_socket_path("quine-sdk-daemon-connect");
    let storage_root = temp_storage_root("quine-sdk-daemon-storage");
    let harness = Arc::new(
        LocalHarness::new(
            Arc::new(MockProvider),
            Some(StorageManager::new(storage_root)),
        )
        .await
        .unwrap(),
    );
    let server_socket_path = socket_path.clone();
    let server_task =
        tokio::spawn(async move { run_ipc_server(&server_socket_path, harness).await });

    wait_for_socket(&socket_path).await;

    let mut client = QuineClient::connect(&socket_path).await.unwrap();
    assert!(client.is_connected());
    client.close().await.unwrap();

    server_task.abort();
    let _ = server_task.await;
    let _ = tokio::fs::remove_file(&socket_path).await;
}

#[tokio::test]
async fn real_daemon_create_session() {
    if require_unix_socket_support("quine-sdk-daemon-request-support")
        .await
        .is_none()
    {
        return;
    }
    let socket_path = temp_socket_path("quine-sdk-daemon-request");
    let storage_root = temp_storage_root("quine-sdk-daemon-request-storage");
    let harness = Arc::new(
        LocalHarness::new(
            Arc::new(MockProvider),
            Some(StorageManager::new(storage_root)),
        )
        .await
        .unwrap(),
    );
    let server_socket_path = socket_path.clone();
    let server_task =
        tokio::spawn(async move { run_ipc_server(&server_socket_path, harness).await });

    wait_for_socket(&socket_path).await;

    let config = ConnectionConfig::new(&socket_path).with_request_timeout(Duration::from_secs(5));
    let mut client = QuineClient::connect_with_config(config).await.unwrap();
    let result = client
        .request_raw("create_session", Some(serde_json::json!({})))
        .await
        .unwrap();

    assert!(result
        .get("session_id")
        .and_then(|value| value.as_str())
        .is_some());

    server_task.abort();
    let _ = server_task.await;
    let _ = tokio::fs::remove_file(&socket_path).await;
}

struct MockProvider;

#[async_trait]
impl LlmProvider for MockProvider {
    async fn send(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
    ) -> anyhow::Result<Pin<Box<dyn Stream<Item = anyhow::Result<LlmEvent>> + Send>>> {
        let stream = futures::stream::iter(vec![Ok(LlmEvent::Done { usage: None })]);
        Ok(Box::pin(stream))
    }
}
