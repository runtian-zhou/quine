#!/usr/bin/env bash

set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/run-docker-workspace.sh [--chat] [--image TAG] [--build|--no-build] [--] [command...]

Build a Quine Docker image from the current git workspace and run it with the
workspace mounted at /workspace. For linked worktrees, the script also mounts
the shared git common dir and preserves the original git metadata paths so git
branch operations work against the main repository metadata.

GitHub CLI auth is stored in a persistent Docker volume. On first use, run
`gh auth login -h github.com` inside the container to authorize it.

Options:
  --chat        Run `cargo run --bin quine -- chat --auto-approve` instead of opening a shell.
  --image TAG   Override the Docker image tag.
  --build       Force `docker build` before running.
  --no-build    Skip `docker build`.
  --help        Show this help.

Examples:
  scripts/run-docker-workspace.sh
  scripts/run-docker-workspace.sh --chat
  scripts/run-docker-workspace.sh -- cargo test -p quine-core
EOF
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "error: missing required command: $1" >&2
    exit 1
  fi
}

abs_path() {
  local path="$1"
  python3 -c 'import os,sys; print(os.path.realpath(sys.argv[1]))' "$path"
}

require_cmd docker
require_cmd git
require_cmd python3

warn() {
  echo "warning: $*" >&2
}

mode="shell"
image_tag=""
build_image="auto"
command_args=()
entrypoint_args=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --chat)
      mode="chat"
      shift
      ;;
    --image)
      [[ $# -ge 2 ]] || { echo "error: --image requires a value" >&2; exit 1; }
      image_tag="$2"
      shift 2
      ;;
    --build)
      build_image="yes"
      shift
      ;;
    --no-build)
      build_image="no"
      shift
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    --)
      shift
      command_args=("$@")
      break
      ;;
    *)
      command_args=("$@")
      break
      ;;
  esac
done

workspace_root="$(git rev-parse --show-toplevel)"
workspace_root="$(abs_path "$workspace_root")"

git_dir_abs="$(git -C "$workspace_root" rev-parse --path-format=absolute --git-dir)"
common_dir_abs="$(git -C "$workspace_root" rev-parse --path-format=absolute --git-common-dir)"
origin_url="$(git -C "$workspace_root" remote get-url origin 2>/dev/null || true)"
git_commit_hash="$(git -C "$workspace_root" rev-parse --short=12 HEAD 2>/dev/null || echo unknown)"

workspace_name="$(basename "$workspace_root")"
if [[ -z "$image_tag" ]]; then
  image_tag="quine-workspace:${workspace_name}"
fi
gh_auth_volume="${GH_AUTH_VOLUME:-quine-gh-auth-${workspace_name}}"
cargo_registry_volume="${CARGO_REGISTRY_VOLUME:-quine-cargo-registry-${workspace_name}}"
cargo_git_volume="${CARGO_GIT_VOLUME:-quine-cargo-git-${workspace_name}}"

if [[ ${#command_args[@]} -eq 0 ]]; then
  if [[ "$mode" == "chat" ]]; then
    entrypoint_args=(--entrypoint "cargo")
    command_args=("run" "--bin" "quine" "--" "chat" "--auto-approve")
  else
    entrypoint_args=(--entrypoint "/bin/bash")
    command_args=("-l")
  fi
else
  entrypoint_args=(--entrypoint "${command_args[0]}")
  command_args=("${command_args[@]:1}")
fi

if [[ "$build_image" == "yes" ]]; then
  echo "Building Docker image: $image_tag"
  docker build --build-arg "GIT_COMMIT_HASH=${git_commit_hash}" -t "$image_tag" "$workspace_root"
elif [[ "$build_image" == "auto" ]]; then
  if docker image inspect "$image_tag" >/dev/null 2>&1; then
    echo "Using cached Docker image: $image_tag"
  else
    echo "Building Docker image: $image_tag"
    docker build --build-arg "GIT_COMMIT_HASH=${git_commit_hash}" -t "$image_tag" "$workspace_root"
  fi
fi

tmpdir="$(mktemp -d "${TMPDIR:-/tmp}/quine-docker-workspace.XXXXXX")"
cleanup() {
  rm -rf "$tmpdir"
}
trap cleanup EXIT

mounts=(
  --mount "type=bind,src=${workspace_root},dst=/workspace"
  --mount "type=bind,src=${workspace_root},dst=${workspace_root}"
  --mount "type=bind,src=${common_dir_abs},dst=${common_dir_abs}"
  --mount "type=volume,src=${gh_auth_volume},dst=/root/.config/gh"
  --mount "type=volume,src=${cargo_registry_volume},dst=/usr/local/cargo/registry"
  --mount "type=volume,src=${cargo_git_volume},dst=/usr/local/cargo/git"
)
docker_env=(
  -e XDG_RUNTIME_DIR=/tmp/xdg-runtime
  -e XDG_STATE_HOME=/root/.quine
  -e GIT_CONFIG_GLOBAL=/root/.gitconfig
  -e GH_CONFIG_DIR=/root/.config/gh
  -e CARGO_TARGET_DIR=/opt/quine-target
  -e LLM_PROVIDER="${LLM_PROVIDER:-openai}"
  -e LLM_BASE_URL="${LLM_BASE_URL:-http://host.docker.internal:8000/v1}"
  -e LLM_API_KEY="${LLM_API_KEY:-}"
  -e LLM_MODEL="${LLM_MODEL:-gpt-5.4}"
  -e LLM_CONTEXT_WINDOW="${LLM_CONTEXT_WINDOW:-}"
  -e ANTHROPIC_API_KEY="${ANTHROPIC_API_KEY:-}"
  -e ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL:-}"
  -e PERMISSION_LLM_ENABLED="${PERMISSION_LLM_ENABLED:-false}"
  -e GIT_CONFIG_COUNT=10
  -e GIT_CONFIG_KEY_0=safe.directory
  -e GIT_CONFIG_VALUE_0=/workspace
  -e GIT_CONFIG_KEY_1=safe.directory
  -e GIT_CONFIG_VALUE_1="${workspace_root}"
  -e GIT_CONFIG_KEY_2=credential.https://github.com.helper
  -e GIT_CONFIG_VALUE_2=
  -e GIT_CONFIG_KEY_3=credential.https://github.com.helper
  -e "GIT_CONFIG_VALUE_3=!gh auth git-credential"
  -e GIT_CONFIG_KEY_4=credential.https://gist.github.com.helper
  -e GIT_CONFIG_VALUE_4=
  -e GIT_CONFIG_KEY_5=credential.https://gist.github.com.helper
  -e "GIT_CONFIG_VALUE_5=!gh auth git-credential"
  -e GIT_CONFIG_KEY_6=user.name
  -e GIT_CONFIG_VALUE_6="${GIT_AUTHOR_NAME:-runtianz}"
  -e GIT_CONFIG_KEY_7=user.email
  -e GIT_CONFIG_VALUE_7="${GIT_AUTHOR_EMAIL:-runtianz@users.noreply.github.com}"
  -e GIT_CONFIG_KEY_8=commit.author
  -e "GIT_CONFIG_VALUE_8=${GIT_AUTHOR_NAME:-runtianz} <${GIT_AUTHOR_EMAIL:-runtianz@users.noreply.github.com}>"
  -e GIT_CONFIG_KEY_9=commit.committer
  -e "GIT_CONFIG_VALUE_9=${GIT_COMMITTER_NAME:-${GIT_AUTHOR_NAME:-runtianz}} <${GIT_COMMITTER_EMAIL:-${GIT_AUTHOR_EMAIL:-runtianz@users.noreply.github.com}}>"
)

if [[ -f "${HOME}/.gitconfig" ]]; then
  mounts+=(--mount "type=bind,src=${HOME}/.gitconfig,dst=/root/.gitconfig,readonly")
fi

if [[ -d "${HOME}/.ssh" ]]; then
  mounts+=(--mount "type=bind,src=${HOME}/.ssh,dst=/root/.ssh,readonly")
fi

if [[ "$origin_url" == git@github.com:* || "$origin_url" == ssh://git@github.com/* ]]; then
  if [[ -n "${SSH_AUTH_SOCK:-}" && -S "${SSH_AUTH_SOCK}" ]]; then
    mounts+=(--mount "type=bind,src=${SSH_AUTH_SOCK},dst=/tmp/ssh-agent.sock")
    docker_env+=(-e SSH_AUTH_SOCK=/tmp/ssh-agent.sock)
  else
    warn "origin uses SSH but SSH_AUTH_SOCK is not available; git push in Docker may fail"
  fi
fi

echo "Starting container from workspace: $workspace_root"
echo "Git common dir mounted from: $common_dir_abs"
echo "GitHub auth volume: $gh_auth_volume"
echo "Cargo registry volume: $cargo_registry_volume"
echo "Cargo git volume: $cargo_git_volume"
if [[ "$origin_url" == https://github.com/* ]]; then
  echo "If this is the first run, authorize inside Docker with: gh auth login -h github.com"
fi

exec docker run --rm -it \
  --workdir /workspace \
  --add-host host.docker.internal:host-gateway \
  "${docker_env[@]}" \
  "${entrypoint_args[@]}" \
  "${mounts[@]}" \
  "$image_tag" \
  "${command_args[@]}"
