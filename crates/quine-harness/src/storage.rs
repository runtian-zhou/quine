use std::path::{Path, PathBuf};

use quine_core::CoreCheckpoint;
use serde::{Deserialize, Serialize};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[derive(Debug, Clone)]
pub struct StorageManager {
    root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Manifest {
    format_version: u32,
    current_generation: u64,
}

const MANIFEST_FILE_NAME: &str = "manifest.json";
const TMP_EXTENSION: &str = ".tmp";
const STORAGE_FORMAT_VERSION: u32 = 1;

impl StorageManager {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn load_latest_checkpoint(&self) -> anyhow::Result<Option<CoreCheckpoint>> {
        let manifest_path = self.manifest_path();
        let manifest_bytes = match fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };

        let manifest: Manifest = serde_json::from_slice(&manifest_bytes)?;
        let checkpoint_bytes = fs::read(self.checkpoint_path(manifest.current_generation)).await?;
        let checkpoint = serde_json::from_slice(&checkpoint_bytes)?;
        Ok(Some(checkpoint))
    }

    pub async fn commit_checkpoint(&self, checkpoint: &CoreCheckpoint) -> anyhow::Result<()> {
        self.ensure_root().await?;
        let next_generation = self.next_generation().await?;
        let checkpoint_path = self.checkpoint_path(next_generation);
        let checkpoint_tmp_path = self.temporary_path(&checkpoint_path);
        let manifest = Manifest {
            format_version: STORAGE_FORMAT_VERSION,
            current_generation: next_generation,
        };
        let manifest_path = self.manifest_path();
        let manifest_tmp_path = self.temporary_path(&manifest_path);

        self.write_json_atomic(&checkpoint_tmp_path, &checkpoint_path, checkpoint)
            .await?;
        self.write_json_atomic(&manifest_tmp_path, &manifest_path, &manifest)
            .await?;
        Ok(())
    }

    async fn next_generation(&self) -> anyhow::Result<u64> {
        let manifest = self.load_manifest().await?;
        Ok(manifest.map_or(1, |entry| entry.current_generation + 1))
    }

    async fn load_manifest(&self) -> anyhow::Result<Option<Manifest>> {
        let manifest_path = self.manifest_path();
        let manifest_bytes = match fs::read(&manifest_path).await {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let manifest = serde_json::from_slice(&manifest_bytes)?;
        Ok(Some(manifest))
    }

    async fn ensure_root(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.root).await?;
        Ok(())
    }

    async fn write_json_atomic<T: Serialize>(
        &self,
        tmp_path: &Path,
        final_path: &Path,
        value: &T,
    ) -> anyhow::Result<()> {
        let payload = serde_json::to_vec_pretty(value)?;
        let mut file = fs::File::create(tmp_path).await?;
        file.write_all(&payload).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        fs::rename(tmp_path, final_path).await?;
        Ok(())
    }

    fn manifest_path(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE_NAME)
    }

    fn checkpoint_path(&self, generation: u64) -> PathBuf {
        self.root.join(format!("checkpoint-{generation}.json"))
    }

    fn temporary_path(&self, final_path: &Path) -> PathBuf {
        let file_name = final_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("checkpoint.json");
        final_path.with_file_name(format!("{file_name}{TMP_EXTENSION}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use quine_core::{
        CoreCheckpoint, PersistedPlanStore, PersistedSession, PersistedSessionConfig,
        PersistedSessionState, PersistedSessionTree, SessionId,
    };

    fn make_temp_storage() -> StorageManager {
        let root =
            std::env::temp_dir().join(format!("quine-harness-storage-{}", uuid::Uuid::new_v4()));
        StorageManager::new(root)
    }

    fn sample_checkpoint() -> CoreCheckpoint {
        CoreCheckpoint::new(
            vec![PersistedSession {
                session_id: SessionId::new(),
                created_at: Utc::now(),
                state: PersistedSessionState::Idle,
                config: PersistedSessionConfig {
                    system_prompt: Some("prompt".into()),
                    skill_names: Vec::new(),
                    working_directory: PathBuf::from("/tmp/project"),
                    plan_mode: false,
                    auto_approve_permissions: false,
                },
                history: vec![quine_llm::Message::user("hello")],
                plan_store: PersistedPlanStore::default(),
            }],
            PersistedSessionTree {
                parents: Default::default(),
                children: Default::default(),
                exit_statuses: Default::default(),
            },
        )
    }

    #[tokio::test]
    async fn commit_and_load_roundtrip() {
        let storage = make_temp_storage();
        let checkpoint = sample_checkpoint();

        storage.commit_checkpoint(&checkpoint).await.unwrap();
        let loaded = storage.load_latest_checkpoint().await.unwrap().unwrap();

        assert_eq!(loaded.format_version, checkpoint.format_version);
        assert_eq!(loaded.sessions.len(), 1);
        assert_eq!(loaded.sessions[0].history.len(), 1);
    }

    #[tokio::test]
    async fn manifest_controls_visible_generation() {
        let storage = make_temp_storage();
        let checkpoint = sample_checkpoint();
        storage.commit_checkpoint(&checkpoint).await.unwrap();

        let stray_path = storage.checkpoint_path(99);
        fs::write(&stray_path, serde_json::to_vec_pretty(&checkpoint).unwrap())
            .await
            .unwrap();

        let loaded = storage.load_latest_checkpoint().await.unwrap().unwrap();
        assert_eq!(loaded.sessions.len(), 1);
        assert!(storage.manifest_path().exists());
    }
}
