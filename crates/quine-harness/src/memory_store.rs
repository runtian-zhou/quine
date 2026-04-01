use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use quine_core::{MemoryDocument, MemoryRecord, MemoryScope, MemoryService, SessionId};
use tokio::sync::Mutex;

const MEMORY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct FilesystemMemoryStore {
    root: PathBuf,
    write_lock: Arc<Mutex<()>>,
}

impl FilesystemMemoryStore {
    pub fn new(state_dir: PathBuf) -> Self {
        Self {
            root: state_dir.join("memory"),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn memory_root(&self) -> &Path {
        &self.root
    }

    async fn read_document(&self, path: &Path) -> Result<MemoryDocument> {
        match tokio::fs::read_to_string(path).await {
            Ok(contents) => {
                let mut document: MemoryDocument =
                    serde_json::from_str(&contents).with_context(|| {
                        format!("failed to parse memory document at {}", path.display())
                    })?;
                if document.schema_version == 0 {
                    document.schema_version = MEMORY_SCHEMA_VERSION;
                }
                Ok(document)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(MemoryDocument {
                schema_version: MEMORY_SCHEMA_VERSION,
                records: Vec::new(),
            }),
            Err(error) => Err(error)
                .with_context(|| format!("failed to read memory document at {}", path.display())),
        }
    }

    async fn write_document(&self, path: &Path, document: &MemoryDocument) -> Result<()> {
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.with_context(|| {
                format!("failed to create memory directory {}", parent.display())
            })?;
        }

        let json = serde_json::to_vec_pretty(document)?;
        let temp_path = path.with_extension("json.tmp");
        tokio::fs::write(&temp_path, json).await.with_context(|| {
            format!(
                "failed to write temporary memory file {}",
                temp_path.display()
            )
        })?;
        tokio::fs::rename(&temp_path, path)
            .await
            .with_context(|| format!("failed to replace memory file {}", path.display()))?;
        Ok(())
    }

    async fn canonical_project_root(path: &Path) -> PathBuf {
        match tokio::fs::canonicalize(path).await {
            Ok(canonical) => canonical,
            Err(_) => path.to_path_buf(),
        }
    }

    fn document_path_for_scope(&self, scope: &MemoryScope) -> PathBuf {
        match scope {
            MemoryScope::User => self.root.join("user.json"),
            MemoryScope::Project { root } => self
                .root
                .join("projects")
                .join(format!("{}.json", project_key(root))),
            MemoryScope::Session { session_id } => self.root.join("sessions").join(format!(
                "{}.json",
                serde_json::to_value(session_id)
                    .ok()
                    .and_then(|value| value.as_str().map(str::to_owned))
                    .unwrap_or_else(|| format!("{session_id:?}"))
            )),
        }
    }
}

#[async_trait]
impl MemoryService for FilesystemMemoryStore {
    async fn load_applicable(
        &self,
        working_directory: &Path,
        session_id: SessionId,
    ) -> Result<Vec<MemoryRecord>> {
        let project_root = Self::canonical_project_root(working_directory).await;
        let scopes = [
            MemoryScope::User,
            MemoryScope::Project { root: project_root },
            MemoryScope::Session { session_id },
        ];

        let mut records = Vec::new();
        for scope in scopes {
            let mut scoped = self.list(&scope).await?;
            records.append(&mut scoped);
        }

        records.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(records)
    }

    async fn list(&self, scope: &MemoryScope) -> Result<Vec<MemoryRecord>> {
        let path = self.document_path_for_scope(scope);
        let mut document = self.read_document(&path).await?;
        document.records.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.id.cmp(&right.id))
        });
        Ok(document.records)
    }

    async fn upsert(&self, record: MemoryRecord) -> Result<MemoryRecord> {
        let _guard = self.write_lock.lock().await;
        let path = self.document_path_for_scope(&record.scope);
        let mut document = self.read_document(&path).await?;
        document.schema_version = MEMORY_SCHEMA_VERSION;

        if let Some(existing) = document
            .records
            .iter_mut()
            .find(|existing| existing.id == record.id)
        {
            *existing = record.clone();
        } else {
            document.records.push(record.clone());
        }
        document.records.sort_by(|left, right| {
            left.title
                .cmp(&right.title)
                .then_with(|| left.id.cmp(&right.id))
        });
        self.write_document(&path, &document).await?;
        Ok(record)
    }

    async fn delete(&self, scope: &MemoryScope, id: &str) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let path = self.document_path_for_scope(scope);
        let mut document = self.read_document(&path).await?;
        document.records.retain(|record| record.id != id);
        document.schema_version = MEMORY_SCHEMA_VERSION;
        self.write_document(&path, &document).await
    }
}

fn project_key(path: &Path) -> String {
    let normalized = path.to_string_lossy();
    let hash = fnv1a64(normalized.as_bytes());
    let label: String = normalized
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let compact_label = label
        .split('-')
        .filter(|part| !part.is_empty())
        .take(6)
        .collect::<Vec<_>>()
        .join("-");

    if compact_label.is_empty() {
        format!("project-{hash:016x}")
    } else {
        format!("{compact_label}-{hash:016x}")
    }
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    fn record(scope: MemoryScope, id: &str, title: &str, body: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.into(),
            scope,
            title: title.into(),
            body: body.into(),
            tags: vec!["test".into()],
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn missing_scope_returns_empty_list() {
        let temp = tempdir().unwrap();
        let store = FilesystemMemoryStore::new(temp.path().to_path_buf());
        let records = store.list(&MemoryScope::User).await.unwrap();
        assert!(records.is_empty());
    }

    #[tokio::test]
    async fn upsert_replaces_existing_record_by_id() {
        let temp = tempdir().unwrap();
        let store = FilesystemMemoryStore::new(temp.path().to_path_buf());
        let scope = MemoryScope::User;
        store
            .upsert(record(scope.clone(), "style", "Style", "First"))
            .await
            .unwrap();
        store
            .upsert(record(scope.clone(), "style", "Style", "Second"))
            .await
            .unwrap();

        let records = store.list(&scope).await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].body, "Second");
    }

    #[tokio::test]
    async fn load_applicable_combines_user_project_and_session_memory() {
        let temp = tempdir().unwrap();
        let store = FilesystemMemoryStore::new(temp.path().to_path_buf());
        let working_directory = temp.path().join("workspace");
        tokio::fs::create_dir_all(&working_directory).await.unwrap();
        let canonical_root =
            FilesystemMemoryStore::canonical_project_root(&working_directory).await;
        let session_id = SessionId::new();

        store
            .upsert(record(MemoryScope::User, "user", "User", "Global"))
            .await
            .unwrap();
        store
            .upsert(record(
                MemoryScope::Project {
                    root: canonical_root,
                },
                "project",
                "Project",
                "Repo",
            ))
            .await
            .unwrap();
        store
            .upsert(record(
                MemoryScope::Session { session_id },
                "session",
                "Session",
                "Thread",
            ))
            .await
            .unwrap();

        let records = store
            .load_applicable(&working_directory, session_id)
            .await
            .unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(
            records
                .iter()
                .map(|record| record.title.as_str())
                .collect::<Vec<_>>(),
            vec!["Project", "Session", "User"]
        );
    }

    #[test]
    fn project_key_is_stable_and_non_empty() {
        let path = PathBuf::from("/tmp/example/project");
        assert_eq!(project_key(&path), project_key(&path));
        assert!(!project_key(&path).is_empty());
    }
}
