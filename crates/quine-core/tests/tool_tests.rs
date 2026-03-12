use quine_core::tool::read::ReadTool;
use quine_core::tool::write::WriteTool;
use quine_core::tool::edit::EditTool;
use quine_core::tool::glob::GlobTool;
use quine_core::tool::grep::GrepTool;
use quine_core::tool::{Tool, ToolRegistry};
use serde_json::json;
use tempfile::TempDir;

// --- ReadTool ---

#[tokio::test]
async fn read_tool_reads_file_with_line_numbers() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("hello.txt"), "line one\nline two\nline three").unwrap();

    let tool = ReadTool::new(tmp.path());
    let result = tool.execute(json!({"file_path": "hello.txt"})).await.unwrap();

    assert_eq!(result.success, true);
    assert!(
        result.output.contains("line one"),
        "output should contain file content"
    );
    assert!(
        result.output.contains("line three"),
        "output should contain all lines"
    );
    // Verify line numbers are present
    assert!(result.output.contains("1\t"), "output should contain line number 1");
    assert!(result.output.contains("3\t"), "output should contain line number 3");
}

#[tokio::test]
async fn read_tool_with_offset_and_limit() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("nums.txt"),
        "alpha\nbeta\ngamma\ndelta\nepsilon",
    )
    .unwrap();

    let tool = ReadTool::new(tmp.path());
    let result = tool
        .execute(json!({"file_path": "nums.txt", "offset": 2, "limit": 2}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert!(result.output.contains("beta"), "should include line at offset 2");
    assert!(result.output.contains("gamma"), "should include line at offset 3");
    assert!(!result.output.contains("alpha"), "should not include line before offset");
    assert!(!result.output.contains("delta"), "should not include line beyond limit");
}

#[tokio::test]
async fn read_tool_nonexistent_file_returns_failure() {
    let tmp = TempDir::new().unwrap();
    let tool = ReadTool::new(tmp.path());
    let result = tool
        .execute(json!({"file_path": "no_such_file.txt"}))
        .await
        .unwrap();

    assert_eq!(result.success, false, "reading nonexistent file should fail");
    assert!(
        result.output.contains("Error reading"),
        "error message should describe the failure"
    );
}

#[tokio::test]
async fn read_tool_absolute_path() {
    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("abs.txt");
    std::fs::write(&file_path, "absolute content").unwrap();

    let tool = ReadTool::new(tmp.path());
    let result = tool
        .execute(json!({"file_path": file_path.to_str().unwrap()}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert!(result.output.contains("absolute content"));
}

// --- WriteTool ---

#[tokio::test]
async fn write_tool_creates_file() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteTool::new(tmp.path());

    let result = tool
        .execute(json!({"file_path": "new_file.txt", "content": "hello world"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    let content = std::fs::read_to_string(tmp.path().join("new_file.txt")).unwrap();
    assert_eq!(content, "hello world", "file content should match exactly");
}

#[tokio::test]
async fn write_tool_creates_parent_directories() {
    let tmp = TempDir::new().unwrap();
    let tool = WriteTool::new(tmp.path());

    let result = tool
        .execute(json!({"file_path": "sub/dir/file.txt", "content": "nested"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    let content = std::fs::read_to_string(tmp.path().join("sub/dir/file.txt")).unwrap();
    assert_eq!(content, "nested", "nested file content should match exactly");
}

#[tokio::test]
async fn write_tool_overwrites_existing_file() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("existing.txt"), "old content").unwrap();

    let tool = WriteTool::new(tmp.path());
    let result = tool
        .execute(json!({"file_path": "existing.txt", "content": "new content"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    let content = std::fs::read_to_string(tmp.path().join("existing.txt")).unwrap();
    assert_eq!(content, "new content", "file should be overwritten with new content");
}

// --- EditTool ---

#[tokio::test]
async fn edit_tool_replaces_unique_string() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("edit_me.txt"), "foo bar baz").unwrap();

    let tool = EditTool::new(tmp.path());
    let result = tool
        .execute(json!({
            "file_path": "edit_me.txt",
            "old_string": "bar",
            "new_string": "qux"
        }))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    let content = std::fs::read_to_string(tmp.path().join("edit_me.txt")).unwrap();
    assert_eq!(content, "foo qux baz", "should replace 'bar' with 'qux'");
}

#[tokio::test]
async fn edit_tool_rejects_ambiguous_match() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("dup.txt"), "aaa bbb aaa").unwrap();

    let tool = EditTool::new(tmp.path());
    let result = tool
        .execute(json!({
            "file_path": "dup.txt",
            "old_string": "aaa",
            "new_string": "ccc"
        }))
        .await
        .unwrap();

    assert_eq!(result.success, false, "should fail when old_string matches multiple times");
    assert!(
        result.output.contains("2 times"),
        "error should mention the match count"
    );
}

#[tokio::test]
async fn edit_tool_replace_all_replaces_all_occurrences() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("multi.txt"), "aaa bbb aaa").unwrap();

    let tool = EditTool::new(tmp.path());
    let result = tool
        .execute(json!({
            "file_path": "multi.txt",
            "old_string": "aaa",
            "new_string": "ccc",
            "replace_all": true
        }))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    let content = std::fs::read_to_string(tmp.path().join("multi.txt")).unwrap();
    assert_eq!(content, "ccc bbb ccc", "all occurrences of 'aaa' should be replaced");
}

#[tokio::test]
async fn edit_tool_old_string_not_found() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("nope.txt"), "hello world").unwrap();

    let tool = EditTool::new(tmp.path());
    let result = tool
        .execute(json!({
            "file_path": "nope.txt",
            "old_string": "xyz",
            "new_string": "abc"
        }))
        .await
        .unwrap();

    assert_eq!(result.success, false, "should fail when old_string is not found");
    assert!(result.output.contains("not found"));
}

#[tokio::test]
async fn edit_tool_nonexistent_file() {
    let tmp = TempDir::new().unwrap();
    let tool = EditTool::new(tmp.path());
    let result = tool
        .execute(json!({
            "file_path": "ghost.txt",
            "old_string": "a",
            "new_string": "b"
        }))
        .await
        .unwrap();

    assert_eq!(result.success, false, "editing nonexistent file should fail");
}

// --- GlobTool ---

#[tokio::test]
async fn glob_tool_finds_matching_files() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("a.rs"), "").unwrap();
    std::fs::write(tmp.path().join("b.rs"), "").unwrap();
    std::fs::write(tmp.path().join("c.txt"), "").unwrap();

    let tool = GlobTool::new(tmp.path());
    let result = tool.execute(json!({"pattern": "*.rs"})).await.unwrap();

    assert_eq!(result.success, true);
    assert!(result.output.contains("a.rs"), "should find a.rs");
    assert!(result.output.contains("b.rs"), "should find b.rs");
    assert!(!result.output.contains("c.txt"), "should not match c.txt");
}

#[tokio::test]
async fn glob_tool_no_matches() {
    let tmp = TempDir::new().unwrap();
    let tool = GlobTool::new(tmp.path());
    let result = tool.execute(json!({"pattern": "*.xyz"})).await.unwrap();

    assert_eq!(result.success, true);
    assert_eq!(
        result.output, "No files matched the pattern.",
        "should return exact no-match message"
    );
}

#[tokio::test]
async fn glob_tool_recursive_pattern() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("top.rs"), "").unwrap();
    std::fs::write(tmp.path().join("sub/nested.rs"), "").unwrap();

    let tool = GlobTool::new(tmp.path());
    let result = tool.execute(json!({"pattern": "**/*.rs"})).await.unwrap();

    assert_eq!(result.success, true);
    assert!(result.output.contains("nested.rs"), "should find nested files");
}

// --- GrepTool ---

#[tokio::test]
async fn grep_tool_finds_matching_lines() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("code.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();

    let tool = GrepTool::new(tmp.path());
    let result = tool
        .execute(json!({"pattern": "println", "file_pattern": "*.rs"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert!(result.output.contains("println"), "should find the matching line");
    assert!(
        result.output.contains(":2:"),
        "should report line number 2"
    );
}

#[tokio::test]
async fn grep_tool_no_matches() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("empty_match.txt"), "nothing here").unwrap();

    let tool = GrepTool::new(tmp.path());
    let result = tool
        .execute(json!({"pattern": "xyz123", "file_pattern": "*.txt"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert_eq!(
        result.output, "No matches found.",
        "should return exact no-match message"
    );
}

#[tokio::test]
async fn grep_tool_invalid_regex() {
    let tmp = TempDir::new().unwrap();
    let tool = GrepTool::new(tmp.path());
    let result = tool.execute(json!({"pattern": "[invalid"})).await.unwrap();

    assert_eq!(result.success, false, "invalid regex should fail");
    assert!(result.output.contains("Invalid regex"));
}

// --- ToolRegistry ---

#[test]
fn tool_registry_register_defaults_has_all_tools() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());

    assert!(registry.get("Bash").is_some(), "should have Bash tool");
    assert!(registry.get("Read").is_some(), "should have Read tool");
    assert!(registry.get("Write").is_some(), "should have Write tool");
    assert!(registry.get("Edit").is_some(), "should have Edit tool");
    assert!(registry.get("Glob").is_some(), "should have Glob tool");
    assert!(registry.get("Grep").is_some(), "should have Grep tool");
    assert!(registry.get("ListDirectory").is_some(), "should have ListDirectory tool");
    assert!(registry.get("Skill").is_some(), "should have Skill tool");
    assert!(registry.get("Todo").is_some(), "should have Todo tool");
}

#[test]
fn tool_registry_get_unknown_returns_none() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    assert!(registry.get("NonExistent").is_none(), "unknown tool should return None");
}

#[test]
fn tool_registry_all_schemas_returns_correct_count() {
    let tmp = TempDir::new().unwrap();
    let registry = ToolRegistry::register_defaults(tmp.path());
    let schemas = registry.all_schemas();

    assert_eq!(schemas.len(), 9, "should have exactly 9 tool schemas (Bash, Read, Write, Edit, Glob, Grep, ListDirectory, Skill, Todo)");

    for schema in &schemas {
        assert!(schema["name"].is_string(), "each schema should have a name");
        assert!(schema["description"].is_string(), "each schema should have a description");
        assert!(schema["input_schema"].is_object(), "each schema should have an input_schema");
    }
}

#[test]
fn tool_schema_has_required_fields() {
    let tmp = TempDir::new().unwrap();
    let tool = ReadTool::new(tmp.path());
    let schema = tool.parameters_schema();

    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["file_path"].is_object());
    let required = schema["required"].as_array().unwrap();
    assert!(
        required.contains(&json!("file_path")),
        "file_path should be in required"
    );
}
