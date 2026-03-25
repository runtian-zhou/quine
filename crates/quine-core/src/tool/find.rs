use std::path::Path;

use async_trait::async_trait;

use super::{ExecutionContext, Tool, ToolError, ToolOutput};

/// Default directories to skip during traversal.
const HIDDEN_DIRS: &[&str] = &[".git", ".hg", "node_modules", "target"];

/// Tool for finding files and directories by name pattern, type, or content.
///
/// Walks the directory tree using `SessionFilesystem::list_dir()` and returns
/// matching paths as a newline-separated list.
pub(crate) struct FindTool;

/// Check whether a file/directory name matches a simple glob pattern.
///
/// Supports `*` (matches any sequence of characters) and `?` (matches a single character).
fn glob_matches(pattern: &str, name: &str) -> bool {
    glob_matches_inner(pattern.as_bytes(), name.as_bytes())
}

fn glob_matches_inner(pattern: &[u8], name: &[u8]) -> bool {
    let mut pi = 0;
    let mut ni = 0;
    let mut star_pi = usize::MAX;
    let mut star_ni = usize::MAX;

    while ni < name.len() {
        if pi < pattern.len() && (pattern[pi] == b'?' || pattern[pi] == name[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < pattern.len() && pattern[pi] == b'*' {
            star_pi = pi;
            star_ni = ni;
            pi += 1;
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            star_ni += 1;
            ni = star_ni;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == b'*' {
        pi += 1;
    }

    pi == pattern.len()
}

/// Parameters for the recursive directory walk.
struct WalkParams<'a> {
    pattern: &'a str,
    type_filter: &'a str,
    content_filter: &'a Option<String>,
    max_depth: Option<usize>,
    max_results: usize,
}

/// Recursively walk a directory tree, collecting matching entries.
///
/// All paths passed to the filesystem are relative (to the overlay root).
/// `fs_path` is the relative path to the current directory being listed.
/// `display_prefix` is the relative path from the user's search root for display.
async fn walk_directory(
    context: &ExecutionContext,
    fs_path: &Path,
    display_prefix: &Path,
    params: &WalkParams<'_>,
    current_depth: usize,
    results: &mut Vec<String>,
) {
    if results.len() >= params.max_results {
        return;
    }

    let entries = match context.filesystem.list_dir(fs_path).await {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries {
        if results.len() >= params.max_results {
            return;
        }

        // Skip hidden directories by default.
        if entry.is_dir && HIDDEN_DIRS.contains(&entry.name.as_str()) {
            continue;
        }

        let entry_fs_path = fs_path.join(&entry.name);
        let entry_display = display_prefix.join(&entry.name);
        let relative = entry_display.to_string_lossy().to_string();

        // Check type filter.
        let type_ok = match params.type_filter {
            "file" => !entry.is_dir,
            "directory" => entry.is_dir,
            _ => true,
        };

        // Check glob pattern against the entry name.
        let pattern_ok = glob_matches(params.pattern, &entry.name);

        if type_ok && pattern_ok {
            // If content filter is set, only include files whose content contains the string.
            if let Some(ref content) = params.content_filter {
                if !entry.is_dir {
                    if let Ok(file_content) = context.filesystem.read_file(&entry_fs_path).await {
                        if file_content.contains(content.as_str()) {
                            results.push(relative.clone());
                        }
                    }
                }
                // Directories don't have content, skip them when content filter is active.
            } else {
                results.push(relative.clone());
            }
        }

        // Recurse into directories.
        if entry.is_dir {
            let within_depth = match params.max_depth {
                Some(md) => current_depth < md,
                None => true,
            };
            if within_depth {
                Box::pin(walk_directory(
                    context,
                    &entry_fs_path,
                    &entry_display,
                    params,
                    current_depth + 1,
                    results,
                ))
                .await;
            }
        }
    }
}

#[async_trait]
impl Tool for FindTool {
    fn name(&self) -> &str {
        "find"
    }

    fn description(&self) -> &str {
        "Search for files and directories by name pattern, type, or content. \
         Walks the directory tree using glob matching. Results are returned as \
         a newline-separated list of relative paths."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "The directory to search in. Defaults to the working directory."
                },
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match file/directory names (e.g., '*.rs', 'test_*'). Defaults to '*' (all)."
                },
                "type": {
                    "type": "string",
                    "enum": ["file", "directory", "any"],
                    "description": "Filter by entry type. Defaults to 'any'."
                },
                "content": {
                    "type": "string",
                    "description": "Optional text to search for within file contents (simple substring match). Only files whose contents contain this string are returned."
                },
                "max_depth": {
                    "type": "integer",
                    "description": "Maximum directory depth to recurse. 0 means only the given path itself. Defaults to no limit."
                },
                "max_results": {
                    "type": "integer",
                    "description": "Maximum number of results to return. Defaults to 200."
                }
            }
        })
    }

    async fn execute(
        &self,
        arguments: serde_json::Value,
        context: &ExecutionContext,
    ) -> Result<ToolOutput, ToolError> {
        let path_str = arguments.get("path").and_then(|v| v.as_str());
        let pattern = arguments
            .get("pattern")
            .and_then(|v| v.as_str())
            .unwrap_or("*");
        let type_filter = arguments
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("any");
        let content_filter = arguments
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);
        let max_depth = arguments
            .get("max_depth")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize);
        let max_results = arguments
            .get("max_results")
            .and_then(|v| v.as_u64())
            .map(|v| v as usize)
            .unwrap_or(200);

        // Resolve path as a relative path for the overlay filesystem.
        // If the user provides a path, use it as-is (relative to the filesystem root).
        // If no path is provided, use "" (the root of the filesystem / working directory).
        let fs_path = match path_str {
            Some(p) => {
                let p = Path::new(p);
                if p.is_absolute() {
                    // Strip the working directory prefix if possible to get a relative path.
                    p.strip_prefix(&context.working_directory)
                        .unwrap_or(p)
                        .to_path_buf()
                } else {
                    p.to_path_buf()
                }
            }
            None => Path::new("").to_path_buf(),
        };

        // Verify the search path exists and is a directory.
        if !context.filesystem.exists(&fs_path).await.unwrap_or(false) {
            return Ok(ToolOutput::error(format!(
                "Directory not found: {}",
                fs_path.display()
            )));
        }

        let mut results = Vec::new();
        let params = WalkParams {
            pattern,
            type_filter,
            content_filter: &content_filter,
            max_depth,
            max_results,
        };

        walk_directory(context, &fs_path, Path::new(""), &params, 0, &mut results).await;

        results.sort();

        let display_path = path_str.unwrap_or(".");

        if results.is_empty() {
            Ok(ToolOutput::success(format!(
                "No files found matching pattern '{pattern}' in '{display_path}'"
            )))
        } else {
            let count = results.len();
            let listing = results.join("\n");
            Ok(ToolOutput::success(format!(
                "Found {count} matches in {display_path}:\n{listing}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::OverlayFilesystem;
    use crate::session::SessionId;
    use std::sync::Arc;
    use tempfile::TempDir;

    async fn make_context(base: &TempDir) -> (TempDir, ExecutionContext) {
        let session_dir = TempDir::new().unwrap();
        let fs =
            OverlayFilesystem::new(base.path().to_path_buf(), session_dir.path().to_path_buf())
                .await
                .unwrap();
        let ctx = ExecutionContext {
            session_id: SessionId::new(),
            filesystem: Arc::new(fs),
            working_directory: base.path().to_path_buf(),
            interaction_channel: None,
            plan_store: crate::tool::plan::new_plan_store(),
            core_input: None,
        };
        (session_dir, ctx)
    }

    #[tokio::test]
    async fn find_all_files() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("a.txt"), "hello").unwrap();
        std::fs::write(base.path().join("b.rs"), "world").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("a.txt"), "should contain a.txt");
        assert!(result.content.contains("b.rs"), "should contain b.rs");
        assert!(
            result.content.contains("Found 2 matches"),
            "should report exactly 2 matches, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_by_glob_pattern() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::write(base.path().join("lib.rs"), "pub fn lib() {}").unwrap();
        std::fs::write(base.path().join("readme.md"), "# Readme").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("main.rs"), "should contain main.rs");
        assert!(result.content.contains("lib.rs"), "should contain lib.rs");
        assert!(
            !result.content.contains("readme.md"),
            "should not contain readme.md"
        );
        assert!(
            result.content.contains("Found 2 matches"),
            "should report exactly 2 matches, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_by_type_file() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("file.txt"), "content").unwrap();
        std::fs::create_dir(base.path().join("subdir")).unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"type": "file"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("file.txt"),
            "should contain file.txt"
        );
        assert!(
            !result.content.contains("subdir"),
            "should not contain subdir directory"
        );
    }

    #[tokio::test]
    async fn find_by_type_directory() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("file.txt"), "content").unwrap();
        std::fs::create_dir(base.path().join("subdir")).unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"type": "directory"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("subdir"),
            "should contain subdir directory"
        );
        assert!(
            !result.content.contains("file.txt"),
            "should not contain file.txt"
        );
    }

    #[tokio::test]
    async fn find_with_content() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("match.txt"), "needle in haystack").unwrap();
        std::fs::write(base.path().join("nomatch.txt"), "just haystack").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"content": "needle"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("match.txt"),
            "should contain match.txt which has 'needle'"
        );
        assert!(
            !result.content.contains("nomatch.txt"),
            "should not contain nomatch.txt which lacks 'needle'"
        );
        assert!(
            result.content.contains("Found 1 matches"),
            "should report exactly 1 match, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_with_max_depth() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("root.txt"), "root").unwrap();
        std::fs::create_dir(base.path().join("sub")).unwrap();
        std::fs::write(base.path().join("sub").join("deep.txt"), "deep").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        // max_depth=0 means only the given path itself (no recursion into subdirs).
        let result = tool
            .execute(serde_json::json!({"max_depth": 0, "type": "file"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("root.txt"),
            "should contain root.txt at depth 0"
        );
        assert!(
            !result.content.contains("deep.txt"),
            "should not contain deep.txt which is at depth 1"
        );
    }

    #[tokio::test]
    async fn find_with_max_results() {
        let base = TempDir::new().unwrap();
        for i in 0..10 {
            std::fs::write(base.path().join(format!("file{i}.txt")), "x").unwrap();
        }

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"max_results": 3}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("Found 3 matches"),
            "should report exactly 3 matches (limited by max_results), got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_empty_directory() {
        let base = TempDir::new().unwrap();
        // No files created — directory is empty.

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool.execute(serde_json::json!({}), &ctx).await.unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("No files found"),
            "should report no files found for empty directory, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_nested() {
        let base = TempDir::new().unwrap();
        std::fs::create_dir_all(base.path().join("a").join("b")).unwrap();
        std::fs::write(base.path().join("a").join("b").join("nested.rs"), "code").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        let result = tool
            .execute(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("nested.rs"),
            "should find nested.rs in subdirectories"
        );
        assert!(
            result.content.contains("a/b/nested.rs"),
            "should show relative path a/b/nested.rs, got: {}",
            result.content
        );
    }

    #[tokio::test]
    async fn find_default_path() {
        let base = TempDir::new().unwrap();
        std::fs::write(base.path().join("hello.txt"), "hi").unwrap();

        let (_session, ctx) = make_context(&base).await;
        let tool = FindTool;

        // No "path" parameter — should use working directory.
        let result = tool
            .execute(serde_json::json!({"pattern": "hello.txt"}), &ctx)
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(
            result.content.contains("hello.txt"),
            "should find hello.txt using default working directory"
        );
    }

    #[test]
    fn glob_matches_star() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(glob_matches("*.rs", "lib.rs"));
        assert!(!glob_matches("*.rs", "readme.md"));
        assert!(glob_matches("*", "anything"));
    }

    #[test]
    fn glob_matches_question_mark() {
        assert!(glob_matches("?.rs", "a.rs"));
        assert!(!glob_matches("?.rs", "ab.rs"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_matches("hello", "hello"));
        assert!(!glob_matches("hello", "world"));
    }

    #[test]
    fn glob_matches_complex() {
        assert!(glob_matches("test_*", "test_foo"));
        assert!(glob_matches("test_*", "test_"));
        assert!(!glob_matches("test_*", "tes"));
        assert!(glob_matches("*_test.*", "my_test.rs"));
    }
}
