use std::collections::HashMap;

use tokio::sync::oneshot;

use crate::persistence::PersistedSessionTree;
use crate::session::{ExitStatus, SessionId};

/// Tracks parent-child relationships between agent sessions.
pub struct SessionTree {
    parents: HashMap<SessionId, SessionId>,
    children: HashMap<SessionId, Vec<SessionId>>,
    exit_statuses: HashMap<SessionId, ExitStatus>,
    waiters: HashMap<SessionId, Vec<oneshot::Sender<ExitStatus>>>,
    active_waits: HashMap<SessionId, SessionId>,
}

impl SessionTree {
    /// Create an empty session tree.
    pub fn new() -> Self {
        Self {
            parents: HashMap::new(),
            children: HashMap::new(),
            exit_statuses: HashMap::new(),
            waiters: HashMap::new(),
            active_waits: HashMap::new(),
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

    /// Record that `waiter` is blocked on `dependency`.
    pub fn register_active_wait(
        &mut self,
        waiter: SessionId,
        dependency: SessionId,
    ) -> Result<(), String> {
        if self.wait_would_cycle(waiter, dependency) {
            return Err(format!(
                "deadlock detected: waiting for child {dependency:?} would create a wait cycle"
            ));
        }
        self.active_waits.insert(waiter, dependency);
        Ok(())
    }

    /// Clear any active wait registered for `waiter`.
    pub fn clear_active_wait(&mut self, waiter: SessionId) {
        self.active_waits.remove(&waiter);
    }

    /// Returns true when adding `waiter -> dependency` would introduce a cycle.
    pub fn wait_would_cycle(&self, waiter: SessionId, dependency: SessionId) -> bool {
        let mut current = dependency;
        let mut visited = std::collections::HashSet::new();
        while visited.insert(current) {
            if current == waiter {
                return true;
            }
            let Some(next) = self.active_waits.get(&current).copied() else {
                return false;
            };
            current = next;
        }
        false
    }

    /// Record that a session has exited. All registered waiters are notified.
    pub fn record_exit(&mut self, session: SessionId, status: ExitStatus) {
        self.active_waits.retain(|waiter, dependency| {
            *waiter != session && *dependency != session
        });
        if let Some(waiters) = self.waiters.remove(&session) {
            for waiter in waiters {
                // Ignore send errors — the receiver may have been dropped.
                let _ = waiter.send(status.clone());
            }
        }
        self.exit_statuses.insert(session, status);
    }

    pub fn exit_status(&self, session: SessionId) -> Option<&ExitStatus> {
        self.exit_statuses.get(&session)
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

    pub fn snapshot(&self) -> PersistedSessionTree {
        PersistedSessionTree {
            parents: self.parents.clone(),
            children: self.children.clone(),
            exit_statuses: self.exit_statuses.clone(),
        }
    }

    pub fn restore(snapshot: PersistedSessionTree) -> Self {
        Self {
            parents: snapshot.parents,
            children: snapshot.children,
            exit_statuses: snapshot.exit_statuses,
            waiters: HashMap::new(),
            active_waits: HashMap::new(),
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
    fn active_waits_detect_cycle_and_clear() {
        let mut tree = SessionTree::new();
        let session_a = SessionId::new();
        let session_b = SessionId::new();
        let session_c = SessionId::new();

        tree.register_active_wait(session_a, session_b).unwrap();
        tree.register_active_wait(session_b, session_c).unwrap();

        let error = tree.register_active_wait(session_c, session_a).unwrap_err();
        assert!(error.contains("deadlock detected"));
        assert!(tree.wait_would_cycle(session_c, session_a));

        tree.clear_active_wait(session_b);
        assert!(!tree.wait_would_cycle(session_c, session_a));
        tree.register_active_wait(session_c, session_a).unwrap();
    }

    #[test]
    fn default_creates_empty_tree() {
        let tree = SessionTree::default();
        let id = SessionId::new();
        assert!(tree.parent_of(id).is_none());
    }
}
