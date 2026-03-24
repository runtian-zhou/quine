use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::RwLock;

use super::{DirEntry, FsError, SessionFilesystem};

/// An overlay filesystem with copy-on-write semantics.
///
/// Reads check the session layer first, then fall through to the base layer.
/// Writes always go to the session layer. Deletes are tracked via whiteout
/// markers so that a deleted base-layer file appears gone.
pub struct OverlayFilesystem {
    /// Base directory (read-only). Typically the project root.
    base_dir: PathBuf,
    /// Session-specific directory (read-write). Overlay on top of base.
    session_dir: PathBuf,
    /// Set of paths (relative to root) that have been whited out (deleted).
    whiteouts: RwLock<HashSet<PathBuf>>,
}

impl OverlayFilesystem {
    /// Create a new overlay filesystem.
    ///
    /// - `base_dir`: the read-only base directory.
    /// - `session_dir`: the read-write session layer directory.
    ///   Will be created if it doesn't exist.
    pub async fn new(base_dir: PathBuf, session_dir: PathBuf) -> Result<Self, FsError> {
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|e| FsError::Io {
                message: format!("failed to create session dir: {e}"),
            })?;
        Ok(Self {
            base_dir,
            session_dir,
            whiteouts: RwLock::new(HashSet::new()),
        })
    }

    /// Normalize a path to a relative path from root, preventing traversal attacks.
    fn normalize_relative(&self, path: &Path) -> Result<PathBuf, FsError> {
        let mut result = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(c) => result.push(c),
                Component::CurDir => {} // skip "."
                Component::ParentDir => {
                    if !result.pop() {
                        return Err(FsError::PathTraversal {
                            path: path.display().to_string(),
                        });
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    // Absolute paths: strip the root prefix and rebuild relative
                }
            }
        }
        Ok(result)
    }

    /// Get the session-layer path for a relative path.
    fn session_path(&self, relative: &Path) -> PathBuf {
        self.session_dir.join(relative)
    }

    /// Get the base-layer path for a relative path.
    fn base_path(&self, relative: &Path) -> PathBuf {
        self.base_dir.join(relative)
    }

    /// Check if a relative path has been whited out.
    async fn is_whited_out(&self, relative: &Path) -> bool {
        let whiteouts = self.whiteouts.read().await;
        whiteouts.contains(relative)
    }
}

#[async_trait]
impl SessionFilesystem for OverlayFilesystem {
    async fn read_file(&self, path: &Path) -> Result<String, FsError> {
        let relative = self.normalize_relative(path)?;

        if self.is_whited_out(&relative).await {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        // Check session layer first
        let session_file = self.session_path(&relative);
        if session_file.exists() {
            return tokio::fs::read_to_string(&session_file)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                });
        }

        // Fall through to base layer
        let base_file = self.base_path(&relative);
        if base_file.exists() {
            return tokio::fs::read_to_string(&base_file)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                });
        }

        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }

    async fn write_file(&self, path: &Path, contents: &str) -> Result<(), FsError> {
        let relative = self.normalize_relative(path)?;
        let session_file = self.session_path(&relative);

        // Create parent directories in session layer
        if let Some(parent) = session_file.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                })?;
        }

        // Remove whiteout if exists
        {
            let mut whiteouts = self.whiteouts.write().await;
            whiteouts.remove(&relative);
        }

        tokio::fs::write(&session_file, contents)
            .await
            .map_err(|e| FsError::Io {
                message: e.to_string(),
            })
    }

    async fn exists(&self, path: &Path) -> Result<bool, FsError> {
        let relative = self.normalize_relative(path)?;

        if self.is_whited_out(&relative).await {
            return Ok(false);
        }

        let session_file = self.session_path(&relative);
        if session_file.exists() {
            return Ok(true);
        }

        let base_file = self.base_path(&relative);
        Ok(base_file.exists())
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let relative = self.normalize_relative(path)?;

        if self.is_whited_out(&relative).await {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        let mut entries = std::collections::HashMap::new();

        // Read from base layer first
        let base_dir = self.base_path(&relative);
        if base_dir.is_dir() {
            let mut read_dir = tokio::fs::read_dir(&base_dir)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                })?;
            while let Some(entry) = read_dir.next_entry().await.map_err(|e| FsError::Io {
                message: e.to_string(),
            })? {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry
                    .file_type()
                    .await
                    .map(|ft| ft.is_dir())
                    .unwrap_or(false);

                // Check if this entry is whited out
                let entry_relative = relative.join(&name);
                if !self.is_whited_out(&entry_relative).await {
                    entries.insert(name.clone(), DirEntry { name, is_dir });
                }
            }
        }

        // Overlay session layer entries
        let session_dir = self.session_path(&relative);
        if session_dir.is_dir() {
            let mut read_dir =
                tokio::fs::read_dir(&session_dir)
                    .await
                    .map_err(|e| FsError::Io {
                        message: e.to_string(),
                    })?;
            while let Some(entry) = read_dir.next_entry().await.map_err(|e| FsError::Io {
                message: e.to_string(),
            })? {
                let name = entry.file_name().to_string_lossy().to_string();
                let is_dir = entry
                    .file_type()
                    .await
                    .map(|ft| ft.is_dir())
                    .unwrap_or(false);
                entries.insert(name.clone(), DirEntry { name, is_dir });
            }
        }

        if entries.is_empty() && !base_dir.is_dir() && !session_dir.is_dir() {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        let mut result: Vec<DirEntry> = entries.into_values().collect();
        result.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(result)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let relative = self.normalize_relative(path)?;
        let session_dir = self.session_path(&relative);
        tokio::fs::create_dir_all(&session_dir)
            .await
            .map_err(|e| FsError::Io {
                message: e.to_string(),
            })
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        let relative = self.normalize_relative(path)?;

        if self.is_whited_out(&relative).await {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        // Remove from session layer if present
        let session_file = self.session_path(&relative);
        if session_file.exists() {
            tokio::fs::remove_file(&session_file)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                })?;
        }

        // If it exists in the base layer, add a whiteout
        let base_file = self.base_path(&relative);
        if base_file.exists() {
            let mut whiteouts = self.whiteouts.write().await;
            whiteouts.insert(relative);
        }

        Ok(())
    }

    async fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let relative = self.normalize_relative(path)?;

        if self.is_whited_out(&relative).await {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        // Remove from session layer if present
        let session_dir_path = self.session_path(&relative);
        if session_dir_path.exists() {
            tokio::fs::remove_dir_all(&session_dir_path)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                })?;
        }

        // If it exists in the base layer, add a whiteout
        let base_dir_path = self.base_path(&relative);
        if base_dir_path.exists() {
            let mut whiteouts = self.whiteouts.write().await;
            whiteouts.insert(relative);
        }

        Ok(())
    }

    fn resolve_path(&self, path: &Path) -> Result<PathBuf, FsError> {
        let relative = self.normalize_relative(path)?;
        Ok(self.base_dir.join(relative))
    }

    fn root(&self) -> &Path {
        &self.base_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup() -> (TempDir, TempDir, OverlayFilesystem) {
        let base = TempDir::new().unwrap();
        let session = TempDir::new().unwrap();
        let fs = OverlayFilesystem::new(base.path().to_path_buf(), session.path().to_path_buf())
            .await
            .unwrap();
        (base, session, fs)
    }

    #[tokio::test]
    async fn read_from_base_layer() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("hello.txt"), "world").unwrap();

        let content = fs.read_file(Path::new("hello.txt")).await.unwrap();
        assert_eq!(content, "world");
    }

    #[tokio::test]
    async fn write_to_session_layer() {
        let (base, session, fs) = setup().await;
        std::fs::write(base.path().join("hello.txt"), "base").unwrap();

        fs.write_file(Path::new("hello.txt"), "session")
            .await
            .unwrap();

        // Session layer should shadow base layer
        let content = fs.read_file(Path::new("hello.txt")).await.unwrap();
        assert_eq!(content, "session");

        // Base layer unchanged
        let base_content = std::fs::read_to_string(base.path().join("hello.txt")).unwrap();
        assert_eq!(base_content, "base");

        // Session layer has the write
        let session_content = std::fs::read_to_string(session.path().join("hello.txt")).unwrap();
        assert_eq!(session_content, "session");
    }

    #[tokio::test]
    async fn whiteout_hides_base_file() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("secret.txt"), "hidden").unwrap();

        fs.remove_file(Path::new("secret.txt")).await.unwrap();

        assert!(!fs.exists(Path::new("secret.txt")).await.unwrap());
        let result = fs.read_file(Path::new("secret.txt")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn path_traversal_denied() {
        let (_base, _session, fs) = setup().await;

        let result = fs.read_file(Path::new("../../etc/passwd")).await;
        assert!(matches!(result, Err(FsError::PathTraversal { .. })));
    }

    #[tokio::test]
    async fn list_dir_merges_layers() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("a.txt"), "a").unwrap();

        fs.write_file(Path::new("b.txt"), "b").await.unwrap();

        let entries = fs.list_dir(Path::new("")).await.unwrap();
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let (_base, _session, fs) = setup().await;

        fs.write_file(Path::new("deep/nested/file.txt"), "content")
            .await
            .unwrap();

        let content = fs
            .read_file(Path::new("deep/nested/file.txt"))
            .await
            .unwrap();
        assert_eq!(content, "content");
    }

    #[tokio::test]
    async fn exists_checks_both_layers() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("base.txt"), "").unwrap();

        assert!(fs.exists(Path::new("base.txt")).await.unwrap());
        assert!(!fs.exists(Path::new("nonexistent.txt")).await.unwrap());

        fs.write_file(Path::new("session.txt"), "").await.unwrap();
        assert!(fs.exists(Path::new("session.txt")).await.unwrap());
    }
}
