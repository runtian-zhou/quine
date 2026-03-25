use std::collections::HashMap;

use tokio::sync::oneshot;

use crate::session::{ExitStatus, SessionId};

/// Tracks parent-child relationships between agent sessions.
pub struct SessionTree {
    parents: HashMap<SessionId, SessionId>,
    children: HashMap<SessionId, Vec<SessionId>>,
    exit_statuses: HashMap<SessionId, ExitStatus>,
    waiters: HashMap<SessionId, Vec<oneshot::Sender<ExitStatus>>>,
}

impl SessionTree {
    /// Create an empty session tree.
    pub fn new() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            exit_statuses: HashMap::new(),
            waiters: HashMap::new(),
        }
    }

    /// Register a child session under the given parent.
    pub fn add_child(&mut self, parent: SessionId, child: SessionId) {
        self.parents.insert(child, parent);
        self.children.entry(parent).or_default().push(child);
    }

    /// Return the parent of a session, if any.
    pub fn parent_of(&self, session: SessionId) -> Option<SessionId> {
        self.parents.get(&session).copied()
    }

    /// Return the children of a session (empty slice if none).
    pub fn children_of(&self, session: SessionId) -> &[SessionId] {
        self.children
            .get(&session)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Record that a session has exited. All registered waiters are notified.
    pub fn record_exit(&mut self, session: SessionId, status: ExitStatus) {
        if let Some(waiters) = self.waiters.remove(&session) {
            for waiter in waiters {
                // Ignore send errors — the receiver may have been dropped.
                let _ = waiter.send(status.clone());
            }
        }
        self.exit_statuses.insert(session, status);
    }

    /// Register a waiter that will be notified when the given session exits.
    ///
    /// If the session has already exited, the exit status is sent immediately.
    /// Returns `true` if the session had already exited (status sent immediately),
    /// `false` if the waiter was registered for future notification.
    pub fn register_waiter(
        &mut self,
        session: SessionId,
        waiter: oneshot::Sender<ExitStatus>,
    ) -> bool {
        if let Some(status) = self.exit_statuses.get(&session) {
            let _ = waiter.send(status.clone());
            true
        } else {
            self.waiters.entry(session).or_default().push(waiter);
            false
        }
    }
}

impl Default for SessionTree {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tree_is_empty() {
        let tree = SessionTree::new();
        let id = SessionId::new();
        assert!(tree.parent_of(id).is_none());
        assert!(tree.children_of(id).is_empty());
    }

    #[test]
    fn add_child_establishes_relationship() {
        let mut tree = SessionTree::new();
        let parent = SessionId::new();
        let child = SessionId::new();

        tree.add_child(parent, child);

        assert_eq!(tree.parent_of(child), Some(parent));
        assert_eq!(tree.children_of(parent), &[child]);
    }

    #[test]
    fn multiple_children() {
        let mut tree = SessionTree::new();
        let parent = SessionId::new();
        let child1 = SessionId::new();
        let child2 = SessionId::new();

        tree.add_child(parent, child1);
        tree.add_child(parent, child2);

        assert_eq!(tree.children_of(parent).len(), 2);
        assert!(tree.children_of(parent).contains(&child1));
        assert!(tree.children_of(parent).contains(&child2));
    }

    #[test]
    fn parent_of_root_is_none() {
        let tree = SessionTree::new();
        let root = SessionId::new();
        assert!(tree.parent_of(root).is_none());
    }

    #[tokio::test]
    async fn record_exit_notifies_waiters() {
        let mut tree = SessionTree::new();
        let parent = SessionId::new();
        let child = SessionId::new();
        tree.add_child(parent, child);

        let (tx, rx) = oneshot::channel();
        tree.register_waiter(child, tx);

        tree.record_exit(
            child,
            ExitStatus::Success {
                output: "done".into(),
            },
        );

        let status = rx.await.unwrap();
        assert!(matches!(status, ExitStatus::Success { .. }));
    }

    #[tokio::test]
    async fn register_waiter_after_exit_sends_immediately() {
        let mut tree = SessionTree::new();
        let session = SessionId::new();

        tree.record_exit(
            session,
            ExitStatus::Failed {
                error: "boom".into(),
            },
        );

        let (tx, rx) = oneshot::channel();
        let already_exited = tree.register_waiter(session, tx);
        assert!(already_exited);

        let status = rx.await.unwrap();
        match status {
            ExitStatus::Failed { error } => assert_eq!(error, "boom"),
            _ => panic!("expected Failed"),
        }
    }

    #[tokio::test]
    async fn multiple_waiters_all_notified() {
        let mut tree = SessionTree::new();
        let session = SessionId::new();

        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        tree.register_waiter(session, tx1);
        tree.register_waiter(session, tx2);

        tree.record_exit(session, ExitStatus::Killed);

        assert!(matches!(rx1.await.unwrap(), ExitStatus::Killed));
        assert!(matches!(rx2.await.unwrap(), ExitStatus::Killed));
    }

    #[test]
    fn default_creates_empty_tree() {
        let tree = SessionTree::default();
        let id = SessionId::new();
        assert!(tree.parent_of(id).is_none());
    }
}
