use quine_core::tool::ask_user::AskUserTool;
use quine_core::tool::bash::BashTool;
use quine_core::tool::edit::EditTool;
use quine_core::tool::glob::GlobTool;
use quine_core::tool::grep::GrepTool;
use quine_core::tool::list_directory::ListDirectoryTool;
use quine_core::tool::read::ReadTool;
use quine_core::tool::skill::SkillTool;
use quine_core::tool::write::WriteTool;
use quine_core::tool::{Tool, ToolRegistry};
use serde_json::json;
use std::sync::Arc;
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

// --- BashTool ---

#[tokio::test]
async fn bash_tool_runs_simple_command() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path());
    let result = tool.execute(json!({"command": "echo hello"})).await.unwrap();

    assert_eq!(result.success, true, "echo should succeed");
    assert_eq!(result.output, "hello\n", "should capture exact stdout of echo");
}

#[tokio::test]
async fn bash_tool_runs_in_working_directory() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "found_it").unwrap();

    let tool = BashTool::new(tmp.path());
    let result = tool.execute(json!({"command": "cat marker.txt"})).await.unwrap();

    assert_eq!(result.success, true, "cat should find the file in working dir");
    assert_eq!(result.output, "found_it", "should read the file content from working dir");
}

#[tokio::test]
async fn bash_tool_captures_stderr() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path());
    let result = tool
        .execute(json!({"command": "echo err_msg >&2"}))
        .await
        .unwrap();

    assert_eq!(result.success, true, "writing to stderr with exit 0 is still success");
    assert!(
        result.output.contains("STDERR:"),
        "output should contain STDERR label, got: {}",
        result.output
    );
    assert!(
        result.output.contains("err_msg"),
        "output should contain the stderr content"
    );
}

#[tokio::test]
async fn bash_tool_reports_failure_on_nonzero_exit() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path());
    let result = tool.execute(json!({"command": "exit 42"})).await.unwrap();

    assert_eq!(result.success, false, "nonzero exit code should report failure");
    assert!(
        result.output.contains("Exit code 42"),
        "output should contain the exact exit code 42, got: {}",
        result.output
    );
}

#[tokio::test]
async fn bash_tool_no_output_shows_exit_code() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path());
    let result = tool.execute(json!({"command": "true"})).await.unwrap();

    assert_eq!(result.success, true, "true command should succeed");
    assert_eq!(
        result.output, "(no output, exit code 0)",
        "should show exact no-output message with exit code 0"
    );
}

#[tokio::test]
async fn bash_tool_pipes_work() {
    let tmp = TempDir::new().unwrap();
    let tool = BashTool::new(tmp.path());
    let result = tool
        .execute(json!({"command": "echo 'line1\nline2\nline3' | wc -l"}))
        .await
        .unwrap();

    assert_eq!(result.success, true, "piped command should succeed");
    assert!(
        result.output.trim().contains("3"),
        "wc -l should count 3 lines, got: {}",
        result.output
    );
}

// --- ListDirectoryTool ---

#[tokio::test]
async fn list_directory_lists_files_and_dirs() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("file_a.txt"), "").unwrap();
    std::fs::write(tmp.path().join("file_b.rs"), "").unwrap();
    std::fs::create_dir(tmp.path().join("subdir")).unwrap();

    let tool = ListDirectoryTool::new(tmp.path());
    let result = tool.execute(json!({})).await.unwrap();

    assert_eq!(result.success, true, "listing existing directory should succeed");
    let lines: Vec<&str> = result.output.lines().collect();
    assert_eq!(lines.len(), 3, "should list exactly 3 entries");
    assert_eq!(lines[0], "file_a.txt", "first entry should be file_a.txt (sorted)");
    assert_eq!(lines[1], "file_b.rs", "second entry should be file_b.rs (sorted)");
    assert_eq!(lines[2], "subdir/", "directories should have trailing /");
}

#[tokio::test]
async fn list_directory_with_explicit_path() {
    let tmp = TempDir::new().unwrap();
    let sub = tmp.path().join("inner");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("nested.txt"), "").unwrap();

    let tool = ListDirectoryTool::new(tmp.path());
    let result = tool.execute(json!({"path": "inner"})).await.unwrap();

    assert_eq!(result.success, true);
    assert_eq!(result.output, "nested.txt", "should list the single file in the subdirectory");
}

#[tokio::test]
async fn list_directory_not_a_directory() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("afile.txt"), "").unwrap();

    let tool = ListDirectoryTool::new(tmp.path());
    let result = tool.execute(json!({"path": "afile.txt"})).await.unwrap();

    assert_eq!(result.success, false, "listing a file (not directory) should fail");
    assert!(
        result.output.contains("is not a directory"),
        "error should say 'is not a directory', got: {}",
        result.output
    );
}

#[tokio::test]
async fn list_directory_empty_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("empty")).unwrap();

    let tool = ListDirectoryTool::new(tmp.path());
    let result = tool.execute(json!({"path": "empty"})).await.unwrap();

    assert_eq!(result.success, true);
    assert_eq!(result.output, "", "empty directory should produce empty output");
}

#[tokio::test]
async fn list_directory_sorted_output() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("zebra"), "").unwrap();
    std::fs::write(tmp.path().join("alpha"), "").unwrap();
    std::fs::write(tmp.path().join("middle"), "").unwrap();

    let tool = ListDirectoryTool::new(tmp.path());
    let result = tool.execute(json!({})).await.unwrap();

    assert_eq!(result.success, true);
    let lines: Vec<&str> = result.output.lines().collect();
    assert_eq!(
        lines,
        vec!["alpha", "middle", "zebra"],
        "entries should be sorted alphabetically"
    );
}

// --- AskUserTool ---

#[tokio::test]
async fn ask_user_returns_user_response() {
    let ask_fn: quine_core::tool::ask_user::AskUserFn = Arc::new(|_question: String| {
        Box::pin(async { Ok("yes, proceed".to_string()) })
    });
    let tool = AskUserTool::new(ask_fn);
    let result = tool
        .execute(json!({"question": "Continue?"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert_eq!(result.output, "yes, proceed", "should return the exact user response");
}

#[tokio::test]
async fn ask_user_passes_question_to_callback() {
    let received = Arc::new(std::sync::Mutex::new(String::new()));
    let received_clone = Arc::clone(&received);

    let ask_fn: quine_core::tool::ask_user::AskUserFn = Arc::new(move |question: String| {
        let received = Arc::clone(&received_clone);
        Box::pin(async move {
            *received.lock().unwrap() = question;
            Ok("ok".to_string())
        })
    });

    let tool = AskUserTool::new(ask_fn);
    tool.execute(json!({"question": "What is your name?"}))
        .await
        .unwrap();

    assert_eq!(
        *received.lock().unwrap(),
        "What is your name?",
        "callback should receive the exact question text"
    );
}

#[tokio::test]
async fn ask_user_handles_callback_error() {
    let ask_fn: quine_core::tool::ask_user::AskUserFn = Arc::new(|_question: String| {
        Box::pin(async { Err(anyhow::anyhow!("connection lost")) })
    });

    let tool = AskUserTool::new(ask_fn);
    let result = tool
        .execute(json!({"question": "Hello?"}))
        .await
        .unwrap();

    assert_eq!(result.success, false, "callback error should report failure");
    assert!(
        result.output.contains("Failed to get user response"),
        "error should contain failure prefix, got: {}",
        result.output
    );
    assert!(
        result.output.contains("connection lost"),
        "error should contain the underlying error message"
    );
}

// --- SkillTool ---

#[tokio::test]
async fn skill_tool_list_no_skills() {
    let tmp = TempDir::new().unwrap();
    let tool = SkillTool::new(tmp.path());
    let result = tool.execute(json!({"action": "list"})).await.unwrap();

    assert_eq!(result.success, true);
    assert_eq!(
        result.output,
        "No skills found. Create skills in `skills/<name>/SKILL.md`.",
        "should return exact no-skills message"
    );
}

#[tokio::test]
async fn skill_tool_list_discovers_skills() {
    let tmp = TempDir::new().unwrap();
    let skill_dir = tmp.path().join("skills").join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: my-skill\ndescription: Does things.\n---\n\nBody.",
    )
    .unwrap();

    let tool = SkillTool::new(tmp.path());
    let result = tool.execute(json!({"action": "list"})).await.unwrap();

    assert_eq!(result.success, true);
    assert!(
        result.output.contains("Available skills:"),
        "should start with header, got: {}",
        result.output
    );
    assert!(
        result.output.contains("**my-skill**"),
        "should list skill name in bold"
    );
    assert!(
        result.output.contains("Does things."),
        "should include skill description"
    );
}

#[tokio::test]
async fn skill_tool_execute_loads_skill_into_context() {
    let tmp = TempDir::new().unwrap();
    let skill_dir = tmp.path().join("skills").join("deploy");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: deploy\ndescription: Deploy workflow.\n---\n\n# Deploy\n\nStep 1: Build.\nStep 2: Ship.",
    )
    .unwrap();

    let tool = SkillTool::new(tmp.path());
    let result = tool
        .execute(json!({"action": "execute", "skill_name": "deploy", "input": "to production"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert!(
        result.output.contains("# Skill: deploy"),
        "output should contain skill header"
    );
    assert!(
        result.output.contains("Step 1: Build."),
        "output should contain skill body"
    );
    assert!(
        result.output.contains("Step 2: Ship."),
        "output should contain full skill body"
    );
    assert!(
        result.output.contains("## User Input"),
        "output should contain user input section"
    );
    assert!(
        result.output.contains("to production"),
        "output should contain the user's input text"
    );
}

#[tokio::test]
async fn skill_tool_execute_includes_references() {
    let tmp = TempDir::new().unwrap();
    let skill_dir = tmp.path().join("skills").join("ref-skill");
    let refs_dir = skill_dir.join("references");
    std::fs::create_dir_all(&refs_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: ref-skill\ndescription: Has refs.\n---\n\n# Main.",
    )
    .unwrap();
    std::fs::write(refs_dir.join("01-setup.md"), "Setup step.").unwrap();
    std::fs::write(refs_dir.join("02-run.md"), "Run step.").unwrap();

    let tool = SkillTool::new(tmp.path());
    let result = tool
        .execute(json!({"action": "execute", "skill_name": "ref-skill"}))
        .await
        .unwrap();

    assert_eq!(result.success, true);
    assert!(
        result.output.contains("## Reference: 01-setup.md"),
        "output should contain first reference header"
    );
    assert!(
        result.output.contains("Setup step."),
        "output should contain first reference content"
    );
    assert!(
        result.output.contains("## Reference: 02-run.md"),
        "output should contain second reference header"
    );
    assert!(
        result.output.contains("Run step."),
        "output should contain second reference content"
    );
}

#[tokio::test]
async fn skill_tool_execute_not_found() {
    let tmp = TempDir::new().unwrap();
    let tool = SkillTool::new(tmp.path());
    let result = tool
        .execute(json!({"action": "execute", "skill_name": "nonexistent"}))
        .await
        .unwrap();

    assert_eq!(result.success, false, "executing unknown skill should fail");
    assert!(
        result.output.contains("Skill 'nonexistent' not found"),
        "error should name the missing skill, got: {}",
        result.output
    );
    assert!(
        result.output.contains("Available: none"),
        "should report no available skills, got: {}",
        result.output
    );
}

#[tokio::test]
async fn skill_tool_execute_not_found_lists_available() {
    let tmp = TempDir::new().unwrap();
    let skill_dir = tmp.path().join("skills").join("existing");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: existing\ndescription: Exists.\n---\n\nBody.",
    )
    .unwrap();

    let tool = SkillTool::new(tmp.path());
    let result = tool
        .execute(json!({"action": "execute", "skill_name": "wrong-name"}))
        .await
        .unwrap();

    assert_eq!(result.success, false);
    assert!(
        result.output.contains("Available: existing"),
        "should list available skills in error, got: {}",
        result.output
    );
}

#[tokio::test]
async fn skill_tool_unknown_action() {
    let tmp = TempDir::new().unwrap();
    let tool = SkillTool::new(tmp.path());
    let result = tool
        .execute(json!({"action": "delete"}))
        .await
        .unwrap();

    assert_eq!(result.success, false, "unknown action should fail");
    assert_eq!(
        result.output,
        "Unknown action 'delete'. Use: list, execute",
        "should return exact error message for unknown action"
    );
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
