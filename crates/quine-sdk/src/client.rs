use std::path::Path;

use serde::Serialize;
use serde_json::Value;

use quine_harness::protocol::{JsonRpcErrorResponse, JsonRpcRequest, JsonRpcResponse};

use crate::config::ConnectionConfig;
use crate::error::{ConnectionError, RequestError};
use crate::transport::{ResponseReceiver, Transport};

pub struct QuineClient {
    transport: Option<Transport>,
    response_rx: ResponseReceiver,
    next_id: u64,
    config: ConnectionConfig,
}

impl QuineClient {
    pub async fn connect(socket_path: impl AsRef<Path>) -> Result<Self, ConnectionError> {
        Self::connect_with_config(ConnectionConfig::new(socket_path.as_ref().to_path_buf())).await
    }

    pub async fn connect_with_config(config: ConnectionConfig) -> Result<Self, ConnectionError> {
        let (transport, response_rx) =
            Transport::connect(config.socket_path())
                .await
                .map_err(|source| ConnectionError::Connect {
                    socket_path: config.socket_path().to_path_buf(),
                    source,
                })?;

        Ok(Self {
            transport: Some(transport),
            response_rx,
            next_id: 1,
            config,
        })
    }

    pub fn connection_config(&self) -> &ConnectionConfig {
        &self.config
    }

    pub fn is_connected(&self) -> bool {
        self.transport.is_some()
    }

    pub async fn close(&mut self) -> Result<(), RequestError> {
        let Some(transport) = self.transport.as_mut() else {
            return Ok(());
        };
        transport.shutdown().await.map_err(RequestError::Write)?;
        self.transport = None;
        Ok(())
    }

    pub async fn request_raw<P: Serialize>(
        &mut self,
        method: &str,
        params: Option<P>,
    ) -> Result<Value, RequestError> {
        let id = self.next_id;
        self.next_id += 1;

        let params = params.map(serde_json::to_value).transpose()?;
        let request = JsonRpcRequest::new(id, method, params);
        let line = serde_json::to_string(&request)?;

        let transport = self.transport.as_mut().ok_or(RequestError::Closed)?;
        transport
            .send_line(&line)
            .await
            .map_err(RequestError::Write)?;

        let timeout = self.config.request_timeout();
        let response = tokio::time::timeout(timeout, self.response_rx.recv())
            .await
            .map_err(|_| RequestError::Timeout {
                method: method.to_string(),
                timeout_secs: timeout.as_secs(),
            })?;

        let Some(response_line) = response else {
            self.transport = None;
            return Err(RequestError::Disconnected);
        };

        parse_response(id, &response_line)
    }
}

fn parse_response(expected_id: u64, response_line: &str) -> Result<Value, RequestError> {
    let value: Value = serde_json::from_str(response_line)
        .map_err(|_| RequestError::MalformedResponse(response_line.to_string()))?;

    let response_id = value
        .get("id")
        .cloned()
        .ok_or_else(|| RequestError::MalformedResponse(response_line.to_string()))?;
    if response_id.as_u64() != Some(expected_id) {
        return Err(RequestError::MalformedResponse(response_line.to_string()));
    }

    if value.get("result").is_some() {
        let response: JsonRpcResponse = serde_json::from_value(value)
            .map_err(|error| RequestError::MalformedResponse(error.to_string()))?;
        return Ok(response.result);
    }

    if value.get("error").is_some() {
        let response: JsonRpcErrorResponse = serde_json::from_value(value)
            .map_err(|error| RequestError::MalformedResponse(error.to_string()))?;
        return Err(RequestError::Rpc {
            code: response.error.code,
            message: response.error.message,
        });
    }

    Err(RequestError::MalformedResponse(response_line.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_success_response() {
        let result = parse_response(1, r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap();
        assert_eq!(result, serde_json::json!({"ok": true}));
    }

    #[test]
    fn parse_error_response() {
        let error = parse_response(
            2,
            r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"missing"}}"#,
        )
        .unwrap_err();
        assert!(matches!(
            error,
            RequestError::Rpc {
                code: -32601,
                ref message
            } if message == "missing"
        ));
    }

    #[test]
    fn parse_rejects_wrong_id() {
        let error =
            parse_response(2, r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#).unwrap_err();
        assert!(matches!(error, RequestError::MalformedResponse(_)));
    }
}
