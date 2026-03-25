use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, RwLock};

use crate::session::SessionId;

/// A message sent between agent sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessage {
    pub from: SessionId,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

/// Per-session message queue backed by a tokio mpsc channel.
pub struct MessageMailbox {
    sender: mpsc::Sender<AgentMessage>,
    receiver: mpsc::Receiver<AgentMessage>,
}

impl MessageMailbox {
    /// Create a new mailbox with the given buffer capacity.
    pub fn new(capacity: usize) -> Self {
        let (sender, receiver) = mpsc::channel(capacity);
        Self { sender, receiver }
    }

    /// Get a clone of the sender for delivering messages to this mailbox.
    pub fn sender(&self) -> mpsc::Sender<AgentMessage> {
        self.sender.clone()
    }

    /// Receive the next message, waiting until one is available.
    pub async fn recv(&mut self) -> Option<AgentMessage> {
        self.receiver.recv().await
    }

    /// Try to receive a message without blocking.
    /// Returns `None` if no message is currently available.
    pub fn try_recv(&mut self) -> Option<AgentMessage> {
        self.receiver.try_recv().ok()
    }

    /// Send a message to this mailbox.
    pub async fn send(
        &self,
        message: AgentMessage,
    ) -> Result<(), mpsc::error::SendError<AgentMessage>> {
        self.sender.send(message).await
    }
}

/// Shared key-value memory accessible by multiple sessions.
///
/// Provides a simple string-to-string store protected by an async read-write lock.
#[derive(Clone)]
pub struct SharedMemory {
    inner: Arc<RwLock<HashMap<String, String>>>,
}

impl SharedMemory {
    /// Create an empty shared memory store.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get a value by key. Returns `None` if the key does not exist.
    pub async fn get(&self, key: &str) -> Option<String> {
        let guard = self.inner.read().await;
        guard.get(key).cloned()
    }

    /// Set a key-value pair, returning the previous value if any.
    pub async fn set(&self, key: String, value: String) -> Option<String> {
        let mut guard = self.inner.write().await;
        guard.insert(key, value)
    }

    /// Remove a key, returning the value if it existed.
    pub async fn remove(&self, key: &str) -> Option<String> {
        let mut guard = self.inner.write().await;
        guard.remove(key)
    }
}

impl Default for SharedMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mailbox_send_and_recv() {
        let mut mailbox = MessageMailbox::new(16);
        let msg = AgentMessage {
            from: SessionId::new(),
            content: "hello".into(),
            timestamp: Utc::now(),
        };

        mailbox.send(msg.clone()).await.unwrap();
        let received = mailbox.recv().await.unwrap();
        assert_eq!(received.content, "hello");
    }

    #[tokio::test]
    async fn mailbox_try_recv_empty() {
        let mut mailbox = MessageMailbox::new(16);
        assert!(mailbox.try_recv().is_none());
    }

    #[tokio::test]
    async fn mailbox_try_recv_has_message() {
        let mut mailbox = MessageMailbox::new(16);
        let msg = AgentMessage {
            from: SessionId::new(),
            content: "ping".into(),
            timestamp: Utc::now(),
        };

        mailbox.send(msg).await.unwrap();
        let received = mailbox.try_recv();
        assert!(received.is_some());
        assert_eq!(received.unwrap().content, "ping");
    }

    #[tokio::test]
    async fn mailbox_multiple_messages_fifo() {
        let mut mailbox = MessageMailbox::new(16);
        let from = SessionId::new();

        for i in 0..3 {
            mailbox
                .send(AgentMessage {
                    from,
                    content: format!("msg-{i}"),
                    timestamp: Utc::now(),
                })
                .await
                .unwrap();
        }

        for i in 0..3 {
            let msg = mailbox.recv().await.unwrap();
            assert_eq!(msg.content, format!("msg-{i}"));
        }
    }

    #[tokio::test]
    async fn shared_memory_get_set() {
        let mem = SharedMemory::new();
        assert!(mem.get("key").await.is_none());

        mem.set("key".into(), "value".into()).await;
        assert_eq!(mem.get("key").await.unwrap(), "value");
    }

    #[tokio::test]
    async fn shared_memory_overwrite() {
        let mem = SharedMemory::new();
        mem.set("k".into(), "v1".into()).await;
        let old = mem.set("k".into(), "v2".into()).await;
        assert_eq!(old, Some("v1".into()));
        assert_eq!(mem.get("k").await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn shared_memory_remove() {
        let mem = SharedMemory::new();
        mem.set("k".into(), "v".into()).await;
        let removed = mem.remove("k").await;
        assert_eq!(removed, Some("v".into()));
        assert!(mem.get("k").await.is_none());
    }

    #[tokio::test]
    async fn shared_memory_concurrent_access() {
        let mem = SharedMemory::new();
        let mem2 = mem.clone();

        let writer = tokio::spawn(async move {
            for i in 0..100 {
                mem2.set("counter".into(), i.to_string()).await;
            }
        });

        let mem3 = mem.clone();
        let reader = tokio::spawn(async move {
            let mut reads = 0;
            for _ in 0..100 {
                if mem3.get("counter").await.is_some() {
                    reads += 1;
                }
            }
            reads
        });

        writer.await.unwrap();
        let reads = reader.await.unwrap();
        // The reader should have gotten at least some successful reads.
        // The final value should be "99".
        assert!(reads > 0);
        assert_eq!(mem.get("counter").await.unwrap(), "99");
    }

    #[tokio::test]
    async fn shared_memory_default() {
        let mem = SharedMemory::default();
        assert!(mem.get("anything").await.is_none());
    }

    #[test]
    fn agent_message_serde_roundtrip() {
        let msg = AgentMessage {
            from: SessionId::new(),
            content: "test".into(),
            timestamp: Utc::now(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let deserialized: AgentMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.content, "test");
    }
}
