use quine_core::prompt::{build_system_prompt, discover_claude_md};
use tempfile::TempDir;

#[test]
fn discover_claude_md_finds_file_in_current_dir() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "# Instructions").unwrap();

    let results = discover_claude_md(tmp.path());
    assert_eq!(results.len(), 1, "should find exactly 1 CLAUDE.md");
    assert_eq!(
        results[0],
        tmp.path().join("CLAUDE.md"),
        "should return the correct path"
    );
}

#[test]
fn discover_claude_md_returns_empty_when_no_file() {
    let tmp = TempDir::new().unwrap();
    // Don't create a nested CLAUDE.md, just check a subdirectory
    let sub = tmp.path().join("deep").join("nested");
    std::fs::create_dir_all(&sub).unwrap();

    // We can only reliably test that it doesn't crash and returns something
    // (it might find CLAUDE.md files higher in the filesystem)
    let results = discover_claude_md(&sub);
    // The sub directory itself has no CLAUDE.md
    let has_sub_claude = results.iter().any(|p| p.starts_with(&sub));
    assert_eq!(
        has_sub_claude, false,
        "should not find CLAUDE.md in the subdirectory"
    );
}

#[test]
fn discover_claude_md_returns_root_first_order() {
    let tmp = TempDir::new().unwrap();
    let child = tmp.path().join("child");
    std::fs::create_dir_all(&child).unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "root").unwrap();
    std::fs::write(child.join("CLAUDE.md"), "child").unwrap();

    let results = discover_claude_md(&child);

    // Filter to only files within our tmp dir
    let within_tmp: Vec<_> = results
        .iter()
        .filter(|p| p.starts_with(tmp.path()))
        .collect();

    assert_eq!(within_tmp.len(), 2, "should find exactly 2 CLAUDE.md files within tmpdir");
    // Root should come before child (root-first order)
    assert!(
        within_tmp[0].ends_with("CLAUDE.md") && !within_tmp[0].starts_with(&child),
        "first result should be the root CLAUDE.md"
    );
    assert!(
        within_tmp[1].starts_with(&child),
        "second result should be the child CLAUDE.md"
    );
}

#[test]
fn build_system_prompt_contains_base_instructions() {
    let tmp = TempDir::new().unwrap();
    let prompt = build_system_prompt(tmp.path());

    assert!(
        prompt.contains("Quine"),
        "system prompt should mention Quine"
    );
    assert!(
        prompt.contains("Read"),
        "system prompt should mention file tools"
    );
}

#[test]
fn build_system_prompt_includes_claude_md_content() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("CLAUDE.md"), "CUSTOM_INSTRUCTION_XYZ").unwrap();

    let prompt = build_system_prompt(tmp.path());

    assert!(
        prompt.contains("CUSTOM_INSTRUCTION_XYZ"),
        "system prompt should include CLAUDE.md content"
    );
}
