use quine_core::worktree::Worktree;
use std::path::Path;
use tempfile::TempDir;

/// Helper to run a git command in the given directory and return stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git command failed");
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Initialize a git repo with an initial commit so that worktrees can be created.
fn init_repo_with_commit(dir: &Path) {
    git(dir, &["init"]);
    git(dir, &["config", "user.email", "test@test.com"]);
    git(dir, &["config", "user.name", "Test"]);
    std::fs::write(dir.join("README"), "init").expect("failed to write initial file");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-m", "initial commit"]);
}

#[test]
fn worktree_create_and_drop_cleans_up() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let repo = tmp.path();
    init_repo_with_commit(repo);

    let worktree_path: std::path::PathBuf;
    let branch_name: String;

    {
        let wt =
            Worktree::create(repo).expect("Worktree::create should succeed on a valid git repo");

        worktree_path = wt.path.clone();
        branch_name = wt.branch.clone();

        // Verify the worktree path exists and is a directory
        assert!(
            worktree_path.exists(),
            "Worktree path should exist after creation, but {:?} does not exist",
            worktree_path
        );
        assert!(
            worktree_path.is_dir(),
            "Worktree path should be a directory, but {:?} is not a directory",
            worktree_path
        );

        // Verify the branch name starts with the expected prefix
        assert!(
            branch_name.starts_with("quine-worktree-"),
            "Branch name should start with 'quine-worktree-', but got '{}'",
            branch_name
        );

        // wt is dropped here
    }

    // After drop, the worktree directory should no longer exist
    assert!(
        !worktree_path.exists(),
        "Worktree path should be removed after drop, but {:?} still exists",
        worktree_path
    );

    // After drop, the branch should no longer exist
    let branch_list = git(repo, &["branch", "--list", &branch_name]);
    assert_eq!(
        branch_list.trim(),
        "",
        "Branch '{}' should be deleted after drop, but 'git branch --list' returned: '{}'",
        branch_name,
        branch_list.trim()
    );
}

#[test]
fn worktree_create_fails_on_non_git_dir() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let non_git_dir = tmp.path();

    // Do NOT run git init -- this is just a plain directory
    let result = Worktree::create(non_git_dir);

    assert!(
        result.is_err(),
        "Worktree::create should return an error for a non-git directory, but it succeeded"
    );
}

#[test]
fn worktree_isolation_changes_dont_affect_main() {
    let tmp = TempDir::new().expect("failed to create temp dir");
    let repo = tmp.path();
    init_repo_with_commit(repo);

    let wt = Worktree::create(repo).expect("Worktree::create should succeed on a valid git repo");

    // Create a new file inside the worktree
    let test_file_name = "worktree_only_file.txt";
    let worktree_file = wt.path.join(test_file_name);
    std::fs::write(&worktree_file, "this file lives only in the worktree")
        .expect("failed to write file in worktree");

    // Verify the file exists in the worktree
    assert!(
        worktree_file.exists(),
        "File should exist in the worktree at {:?}, but it does not",
        worktree_file
    );

    // Verify the file does NOT exist in the main repo directory
    let main_repo_file = repo.join(test_file_name);
    assert!(
        !main_repo_file.exists(),
        "File should NOT exist in the main repo at {:?}, but it does -- worktree isolation is broken",
        main_repo_file
    );
}
