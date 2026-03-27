use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;

use super::{DirEntry, FsError, SessionFilesystem};

/// A rooted filesystem that operates directly on the working directory.
///
/// The constructor keeps the historical `OverlayFilesystem` name to avoid a
/// broad refactor, but the implementation no longer provides a virtual
/// copy-on-write layer or delete whiteouts.
pub struct OverlayFilesystem {
    root_dir: PathBuf,
}

impl OverlayFilesystem {
    /// Create a new rooted filesystem.
    ///
    /// The `session_dir` argument is ignored and retained only for API
    /// compatibility with older callers and tests.
    pub async fn new(base_dir: PathBuf, _session_dir: PathBuf) -> Result<Self, FsError> {
        tokio::fs::create_dir_all(&base_dir)
            .await
            .map_err(|e| FsError::Io {
                message: format!("failed to create root dir: {e}"),
            })?;
        Ok(Self { root_dir: base_dir })
    }

    /// Normalize a path to a root-relative path while preventing traversal.
    fn normalize_relative(&self, path: &Path) -> Result<PathBuf, FsError> {
        let candidate = if path.is_absolute() {
            path.strip_prefix(&self.root_dir)
                .map_err(|_| FsError::PathTraversal {
                    path: path.display().to_string(),
                })?
        } else {
            path
        };

        let mut result = PathBuf::new();
        for component in candidate.components() {
            match component {
                Component::Normal(c) => result.push(c),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !result.pop() {
                        return Err(FsError::PathTraversal {
                            path: path.display().to_string(),
                        });
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(FsError::PathTraversal {
                        path: path.display().to_string(),
                    });
                }
            }
        }
        Ok(result)
    }

    fn root_path(&self, relative: &Path) -> PathBuf {
        self.root_dir.join(relative)
    }
}

#[async_trait]
impl SessionFilesystem for OverlayFilesystem {
    async fn read_file(&self, path: &Path) -> Result<String, FsError> {
        let resolved = self.resolve_path(path)?;
        tokio::fs::read_to_string(&resolved)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FsError::NotFound {
                    path: path.display().to_string(),
                },
                std::io::ErrorKind::PermissionDenied => FsError::PermissionDenied {
                    path: path.display().to_string(),
                },
                _ => FsError::Io {
                    message: e.to_string(),
                },
            })
    }

    async fn write_file(&self, path: &Path, contents: &str) -> Result<(), FsError> {
        let resolved = self.resolve_path(path)?;
        if let Some(parent) = resolved.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| FsError::Io {
                    message: e.to_string(),
                })?;
        }
        tokio::fs::write(&resolved, contents)
            .await
            .map_err(|e| FsError::Io {
                message: e.to_string(),
            })
    }

    async fn exists(&self, path: &Path) -> Result<bool, FsError> {
        let resolved = self.resolve_path(path)?;
        Ok(resolved.exists())
    }

    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        let resolved = self.resolve_path(path)?;
        if !resolved.is_dir() {
            return Err(FsError::NotFound {
                path: path.display().to_string(),
            });
        }

        let mut entries = Vec::new();
        let mut read_dir = tokio::fs::read_dir(&resolved)
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
            entries.push(DirEntry { name, is_dir });
        }

        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let resolved = self.resolve_path(path)?;
        tokio::fs::create_dir_all(&resolved)
            .await
            .map_err(|e| FsError::Io {
                message: e.to_string(),
            })
    }

    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        let resolved = self.resolve_path(path)?;
        tokio::fs::remove_file(&resolved)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FsError::NotFound {
                    path: path.display().to_string(),
                },
                _ => FsError::Io {
                    message: e.to_string(),
                },
            })
    }

    async fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        let resolved = self.resolve_path(path)?;
        tokio::fs::remove_dir_all(&resolved)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => FsError::NotFound {
                    path: path.display().to_string(),
                },
                _ => FsError::Io {
                    message: e.to_string(),
                },
            })
    }

    fn resolve_path(&self, path: &Path) -> Result<PathBuf, FsError> {
        let relative = self.normalize_relative(path)?;
        Ok(self.root_path(&relative))
    }

    fn root(&self) -> &Path {
        &self.root_dir
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
    async fn read_from_root_directory() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("hello.txt"), "world").unwrap();

        let content = fs.read_file(Path::new("hello.txt")).await.unwrap();
        assert_eq!(content, "world");
    }

    #[tokio::test]
    async fn write_updates_real_file() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("hello.txt"), "base").unwrap();

        fs.write_file(Path::new("hello.txt"), "updated")
            .await
            .unwrap();

        let content = fs.read_file(Path::new("hello.txt")).await.unwrap();
        assert_eq!(content, "updated");

        let base_content = std::fs::read_to_string(base.path().join("hello.txt")).unwrap();
        assert_eq!(base_content, "updated");
    }

    #[tokio::test]
    async fn remove_deletes_real_file() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("secret.txt"), "hidden").unwrap();

        fs.remove_file(Path::new("secret.txt")).await.unwrap();

        assert!(!fs.exists(Path::new("secret.txt")).await.unwrap());
        assert!(!base.path().join("secret.txt").exists());
    }

    #[tokio::test]
    async fn path_traversal_denied() {
        let (_base, _session, fs) = setup().await;

        let result = fs.read_file(Path::new("../../etc/passwd")).await;
        assert!(matches!(result, Err(FsError::PathTraversal { .. })));
    }

    #[tokio::test]
    async fn list_dir_reads_real_directory() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("a.txt"), "a").unwrap();
        std::fs::write(base.path().join("b.txt"), "b").unwrap();

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
    async fn exists_checks_root_directory() {
        let (base, _session, fs) = setup().await;
        std::fs::write(base.path().join("base.txt"), "").unwrap();

        assert!(fs.exists(Path::new("base.txt")).await.unwrap());
        assert!(!fs.exists(Path::new("nonexistent.txt")).await.unwrap());

        fs.write_file(Path::new("new.txt"), "").await.unwrap();
        assert!(fs.exists(Path::new("new.txt")).await.unwrap());
    }
}
