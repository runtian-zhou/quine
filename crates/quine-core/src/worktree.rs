use anyhow::Result;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A temporary git worktree for isolated work.
/// Cleans up the worktree and branch on drop.
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    repo_dir: PathBuf,
}

impl Worktree {
    /// Create a new git worktree under `.quine/worktrees/<uuid>`.
    pub fn create(repo_dir: &Path) -> Result<Self> {
        let id = uuid::Uuid::new_v4().to_string();
        let branch = format!("quine-worktree-{}", &id[..8]);
        let worktree_dir = repo_dir.join(".quine").join("worktrees").join(&id);

        std::fs::create_dir_all(worktree_dir.parent().unwrap())?;

        let output = Command::new("git")
            .args([
                "worktree",
                "add",
                &worktree_dir.to_string_lossy(),
                "-b",
                &branch,
            ])
            .current_dir(repo_dir)
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Failed to create worktree: {}", stderr);
        }

        Ok(Self {
            path: worktree_dir,
            branch,
            repo_dir: repo_dir.to_path_buf(),
        })
    }
}

impl Drop for Worktree {
    fn drop(&mut self) {
        // Remove the worktree
        let _ = Command::new("git")
            .args([
                "worktree",
                "remove",
                "--force",
                &self.path.to_string_lossy(),
            ])
            .current_dir(&self.repo_dir)
            .output();

        // Delete the branch
        let _ = Command::new("git")
            .args(["branch", "-D", &self.branch])
            .current_dir(&self.repo_dir)
            .output();
    }
}
