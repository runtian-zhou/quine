use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use tokio::sync::Mutex;

use crate::persistence::{PersistedMemoryState, PersistedSessionMemoryState};
use crate::session::SessionId;

pub(crate) const SESSION_MEMORY_TEMPLATE_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionMemoryPaths {
    pub(crate) directory: PathBuf,
    pub(crate) summary_path: PathBuf,
    pub(crate) metadata_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct PersistedRefreshHandle {
    pub(crate) lock: Arc<Mutex<()>>,
}

impl Default for PersistedRefreshHandle {
    fn default() -> Self {
        Self {
            lock: Arc::new(Mutex::new(())),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SessionMemoryState {
    pub(crate) enabled: bool,
    pub(crate) paths: SessionMemoryPaths,
    pub(crate) refresh_in_flight: bool,
    pub(crate) last_summarized_message_index: Option<usize>,
    #[allow(dead_code)]
    pub(crate) last_refresh_at: Option<DateTime<Utc>>,
    pub(crate) template_version: u32,
    pub(crate) refresh_handle: PersistedRefreshHandle,
}

pub(crate) fn session_memory_paths(state_root: &Path, session_id: SessionId) -> SessionMemoryPaths {
    let directory = state_root
        .join("sessions")
        .join(session_id.to_string())
        .join("session-memory");
    SessionMemoryPaths {
        summary_path: directory.join("summary.md"),
        metadata_path: directory.join("summary.meta.json"),
        directory,
    }
}

pub(crate) fn restore_memory_state(
    state_root: &Path,
    session_id: SessionId,
    persisted: Option<&PersistedMemoryState>,
) -> SessionMemoryState {
    let paths = session_memory_paths(state_root, session_id);
    let persisted_session = persisted.and_then(|state| state.session_memory.as_ref());
    SessionMemoryState {
        enabled: persisted_session.map(|state| state.enabled).unwrap_or(true),
        paths,
        refresh_in_flight: false,
        last_summarized_message_index: persisted_session
            .and_then(|state| state.last_summarized_message_index),
        last_refresh_at: None,
        template_version: persisted_session
            .map(|state| state.template_version)
            .unwrap_or(SESSION_MEMORY_TEMPLATE_VERSION),
        refresh_handle: PersistedRefreshHandle::default(),
    }
}

pub(crate) fn snapshot_memory_state(state: &SessionMemoryState) -> PersistedMemoryState {
    PersistedMemoryState {
        session_memory: Some(PersistedSessionMemoryState {
            enabled: state.enabled,
            last_summarized_message_index: state.last_summarized_message_index,
            template_version: state.template_version,
        }),
    }
}

pub(crate) fn next_unsummarized_message_index(
    last_summarized_message_index: Option<usize>,
) -> usize {
    last_summarized_message_index.map_or(0, |index| index.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::{
        next_unsummarized_message_index, restore_memory_state, session_memory_paths,
        snapshot_memory_state, SessionMemoryState, SESSION_MEMORY_TEMPLATE_VERSION,
    };
    use crate::persistence::{PersistedMemoryState, PersistedSessionMemoryState};
    use crate::session::SessionId;

    #[test]
    fn session_memory_paths_match_expected_layout() {
        let root = std::path::Path::new("/tmp/quine-state");
        let session_id = SessionId::new();
        let paths = session_memory_paths(root, session_id);
        let rendered = paths.summary_path.to_string_lossy();
        assert!(rendered.contains("/sessions/"));
        assert!(rendered.ends_with("/session-memory/summary.md"));
        assert!(paths
            .metadata_path
            .to_string_lossy()
            .ends_with("/session-memory/summary.meta.json"));
    }

    #[test]
    fn restore_and_snapshot_memory_state_round_trip() {
        let root = std::env::temp_dir();
        let session_id = SessionId::new();
        let persisted = PersistedMemoryState {
            session_memory: Some(PersistedSessionMemoryState {
                enabled: false,
                last_summarized_message_index: Some(9),
                template_version: 3,
            }),
        };
        let state = restore_memory_state(&root, session_id, Some(&persisted));
        assert!(!state.enabled);
        assert_eq!(state.last_summarized_message_index, Some(9));
        assert_eq!(state.template_version, 3);

        let snapshot = snapshot_memory_state(&state);
        assert_eq!(
            snapshot
                .session_memory
                .as_ref()
                .and_then(|item| item.last_summarized_message_index),
            Some(9)
        );
    }

    #[test]
    fn next_unsummarized_index_advances_past_boundary() {
        assert_eq!(next_unsummarized_message_index(None), 0);
        assert_eq!(next_unsummarized_message_index(Some(0)), 1);
        assert_eq!(next_unsummarized_message_index(Some(5)), 6);
    }

    #[tokio::test]
    async fn refresh_handle_serializes_writers() {
        let state = SessionMemoryState {
            enabled: true,
            paths: session_memory_paths(&std::env::temp_dir(), SessionId::new()),
            refresh_in_flight: false,
            last_summarized_message_index: None,
            last_refresh_at: None,
            template_version: SESSION_MEMORY_TEMPLATE_VERSION,
            refresh_handle: Default::default(),
        };
        let handle_a = state.refresh_handle.clone();
        let handle_b = state.refresh_handle.clone();
        let active = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let max_seen = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

        let join_a = {
            let active = active.clone();
            let max_seen = max_seen.clone();
            tokio::spawn(async move {
                let _guard = handle_a.lock.lock().await;
                let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_seen.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
        };
        let join_b = {
            let active = active.clone();
            let max_seen = max_seen.clone();
            tokio::spawn(async move {
                let _guard = handle_b.lock.lock().await;
                let current = active.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                max_seen.fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                active.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            })
        };

        join_a.await.unwrap();
        join_b.await.unwrap();
        assert_eq!(max_seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
