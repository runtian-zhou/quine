mod overlay;

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub use overlay::OverlayFilesystem;

/// Errors from filesystem operations.
#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
pub enum FsError {
    /// The requested file or directory was not found.
    #[error("not found: {path}")]
    NotFound { path: String },

    /// A permission or access error.
    #[error("permission denied: {path}")]
    PermissionDenied { path: String },

    /// The path escapes the allowed root directory.
    #[error("path traversal denied: {path}")]
    PathTraversal { path: String },

    /// An I/O error occurred.
    #[error("I/O error: {message}")]
    Io { message: String },

    /// The path is a directory, not a file (or vice versa).
    #[error("wrong entry type at {path}: {message}")]
    WrongEntryType { path: String, message: String },
}

impl From<std::io::Error> for FsError {
    fn from(err: std::io::Error) -> Self {
        match err.kind() {
            std::io::ErrorKind::NotFound => FsError::NotFound {
                path: String::new(),
            },
            std::io::ErrorKind::PermissionDenied => FsError::PermissionDenied {
                path: String::new(),
            },
            _ => FsError::Io {
                message: err.to_string(),
            },
        }
    }
}

/// A directory entry returned by `list_dir`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirEntry {
    /// Name of the entry (file or directory name, not full path).
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

/// Trait abstracting filesystem access for a session.
///
/// Implementations may be backed by the real filesystem, an overlay,
/// or an in-memory store for testing.
#[async_trait]
pub trait SessionFilesystem: Send + Sync {
    /// Read the contents of a file at the given path.
    async fn read_file(&self, path: &Path) -> Result<String, FsError>;

    /// Write contents to a file at the given path, creating it if necessary.
    async fn write_file(&self, path: &Path, contents: &str) -> Result<(), FsError>;

    /// Check whether a path exists.
    async fn exists(&self, path: &Path) -> Result<bool, FsError>;

    /// List entries in a directory.
    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError>;

    /// Recursively create directories.
    async fn create_dir_all(&self, path: &Path) -> Result<(), FsError>;

    /// Remove a file.
    async fn remove_file(&self, path: &Path) -> Result<(), FsError>;

    /// Recursively remove a directory and its contents.
    async fn remove_dir_all(&self, path: &Path) -> Result<(), FsError>;

    /// Resolve a possibly-relative path against the filesystem root.
    /// Returns the canonical absolute path within the filesystem.
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, FsError>;

    /// The root directory of this filesystem.
    fn root(&self) -> &Path;
}

/// A no-op filesystem for testing. All operations return errors.
pub struct NullFilesystem;

#[async_trait]
impl SessionFilesystem for NullFilesystem {
    async fn read_file(&self, path: &Path) -> Result<String, FsError> {
        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }
    async fn write_file(&self, path: &Path, _contents: &str) -> Result<(), FsError> {
        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }
    async fn exists(&self, _path: &Path) -> Result<bool, FsError> {
        Ok(false)
    }
    async fn list_dir(&self, path: &Path) -> Result<Vec<DirEntry>, FsError> {
        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }
    async fn create_dir_all(&self, _path: &Path) -> Result<(), FsError> {
        Ok(())
    }
    async fn remove_file(&self, path: &Path) -> Result<(), FsError> {
        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }
    async fn remove_dir_all(&self, path: &Path) -> Result<(), FsError> {
        Err(FsError::NotFound {
            path: path.display().to_string(),
        })
    }
    fn resolve_path(&self, path: &Path) -> Result<PathBuf, FsError> {
        Ok(path.to_path_buf())
    }
    fn root(&self) -> &Path {
        Path::new("/")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_error_display() {
        let err = FsError::NotFound {
            path: "/foo".into(),
        };
        assert!(err.to_string().contains("/foo"));
    }

    #[test]
    fn dir_entry_serialization() {
        let entry = DirEntry {
            name: "hello.rs".into(),
            is_dir: false,
        };
        let json = serde_json::to_string(&entry).unwrap();
        let de: DirEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(de.name, "hello.rs");
        assert!(!de.is_dir);
    }

    #[test]
    fn fs_error_from_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let fs_err: FsError = io_err.into();
        assert!(matches!(fs_err, FsError::NotFound { .. }));
    }
}
