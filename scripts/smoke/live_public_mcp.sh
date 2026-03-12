#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/smoke/live_public_mcp.sh --server memory|chrome-devtools|all|xcodebuildmcp [--run-dir PATH]

Runs isolated live MCP smoke verification with dry-run and one-shot
flows, storing captured artifacts under .codex-runtime.
EOF
}

SERVER="all"
RUN_ROOT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --server)
      SERVER="$2"
      shift 2
      ;;
    --run-dir)
      RUN_ROOT="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      smoke_die "unknown option: $1"
      ;;
  esac
done

case "$SERVER" in
  memory|chrome-devtools|all|xcodebuildmcp)
    ;;
  *)
    smoke_die "unsupported --server value: $SERVER"
    ;;
esac

smoke_require_cmd cargo
smoke_require_cmd npx

RUN_ROOT="${RUN_ROOT:-$(smoke_default_run_root "smoke/live-public-mcp")}"
RUN_ROOT="$(smoke_prepare_run_root "$RUN_ROOT")"

run_live_target() {
  local target_name="$1"
  local command="$2"
  local description="$3"
  local read_only="$4"
  shift 4
  local args=("$@")

  local dry_run_root="$RUN_ROOT/$target_name/dry-run"
  local run_root="$RUN_ROOT/$target_name/run"

  smoke_init_sandbox "$dry_run_root"
  smoke_write_server_config \
    "$SMOKE_CONFIG" \
    "$target_name" \
    "$command" \
    "$description" \
    "$read_only" \
    "${args[@]}"

  smoke_capture_mcpsmith dry-run "$target_name" --json --dry-run --config "$SMOKE_CONFIG" --skills-dir "$SMOKE_SKILLS_DIR"
  smoke_assert_contains "$SMOKE_LOG_DIR/dry-run.stdout" "\"status\": \"dry-run\""
  smoke_assert_contains "$SMOKE_LOG_DIR/dry-run.stdout" "\"review\""
  smoke_assert_contains "$SMOKE_LOG_DIR/dry-run.stdout" "\"verify\""
  smoke_assert_file "$SMOKE_SKILLS_DIR/$target_name/SKILL.md"
  smoke_save_skills_tree "$SMOKE_SKILLS_DIR" "$dry_run_root/skills-tree.txt"

  smoke_init_sandbox "$run_root"
  smoke_write_server_config \
    "$SMOKE_CONFIG" \
    "$target_name" \
    "$command" \
    "$description" \
    "$read_only" \
    "${args[@]}"

  smoke_capture_mcpsmith run "$target_name" --json --config "$SMOKE_CONFIG" --skills-dir "$SMOKE_SKILLS_DIR"
  smoke_assert_contains "$SMOKE_LOG_DIR/run.stdout" "\"status\": \"applied\""
  smoke_assert_contains "$SMOKE_LOG_DIR/run.stdout" "\"mcp_config_updated\": true"
  smoke_assert_not_contains "$SMOKE_CONFIG" "\"$target_name\""
  smoke_assert_file "$SMOKE_SKILLS_DIR/$target_name/SKILL.md"
  smoke_save_skills_tree "$SMOKE_SKILLS_DIR" "$run_root/skills-tree.txt"
}

run_xcode_optional() {
  local target_root="$RUN_ROOT/xcodebuildmcp"
  if [[ "$(uname -s)" != "Darwin" ]]; then
    mkdir -p "$target_root"
    printf 'Skipped: xcodebuildmcp smoke requires macOS.\n' >"$target_root/SKIP.txt"
    return 0
  fi
  if ! command -v xcodebuild >/dev/null 2>&1; then
    mkdir -p "$target_root"
    printf 'Skipped: xcodebuild not found on PATH.\n' >"$target_root/SKIP.txt"
    return 0
  fi

  run_live_target \
    "xcodebuildmcp" \
    "npx" \
    "Xcode build, simulator, and iOS debug workflows" \
    "" \
    "-y" \
    "xcodebuildmcp@latest" \
    "mcp"
}

if [[ "$SERVER" == "memory" || "$SERVER" == "all" ]]; then
  run_live_target \
    "memory" \
    "npx" \
    "Memory and knowledge graph workflows" \
    true \
    "-y" \
    "@modelcontextprotocol/server-memory"
fi

if [[ "$SERVER" == "chrome-devtools" || "$SERVER" == "all" ]]; then
  run_live_target \
    "chrome-devtools" \
    "npx" \
    "Browser inspection and debugging workflows" \
    "" \
    "-y" \
    "chrome-devtools-mcp@latest"
fi

if [[ "$SERVER" == "xcodebuildmcp" || "$SERVER" == "all" ]]; then
  run_xcode_optional
fi

smoke_print_artifacts "$RUN_ROOT"
