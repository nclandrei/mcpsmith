#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/smoke/live_public_mcp.sh --server memory|chrome-devtools|all|xcodebuildmcp [--run-dir PATH]

Runs live public-MCP smoke verification with isolated HOME, config, skills, and
captured artifacts under .codex-runtime.
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
  local fixture_path="$2"
  local command="$3"
  local description="$4"
  local read_only="$5"
  shift 5
  local args=("$@")

  local target_root="$RUN_ROOT/$target_name"
  local verify_root="$target_root/verify"
  local apply_root="$target_root/apply"
  local verify_dossier
  local apply_dossier
  local discover_path

  verify_dossier="$verify_root/live-smoke.dossier.json"
  apply_dossier="$apply_root/live-smoke.dossier.json"
  discover_path="$verify_root/discover.dossier.json"

  smoke_init_sandbox "$verify_root"
  smoke_write_server_config \
    "$SMOKE_CONFIG" \
    "$target_name" \
    "$command" \
    "$description" \
    "$read_only" \
    "${args[@]}"
  smoke_render_live_dossier "$fixture_path" "$SMOKE_CONFIG" "$verify_dossier"

  smoke_capture_mcpsmith list list --json --config "$SMOKE_CONFIG"
  smoke_capture_mcpsmith inspect inspect "$target_name" --json --config "$SMOKE_CONFIG"
  smoke_capture_mcpsmith discover discover "$target_name" --json --out "$discover_path" --config "$SMOKE_CONFIG"
  smoke_capture_mcpsmith contract contract-test --from-dossier "$verify_dossier" --report "$SMOKE_REPORT" --json --probe-timeout-seconds 30 --probe-retries 1
  smoke_capture_mcpsmith build build --from-dossier "$verify_dossier" --skills-dir "$SMOKE_SKILLS_DIR" --json
  smoke_capture_mcpsmith verify verify "$target_name" --json --config "$SMOKE_CONFIG" --skills-dir "$SMOKE_SKILLS_DIR"

  smoke_assert_file "$discover_path"
  smoke_assert_file "$SMOKE_REPORT"
  smoke_assert_contains "$SMOKE_LOG_DIR/contract.stdout" "\"passed\": true"
  smoke_assert_contains "$SMOKE_LOG_DIR/verify.stdout" "\"passed\": true"
  smoke_assert_file "$SMOKE_SKILLS_DIR/$target_name/SKILL.md"
  smoke_save_skills_tree "$SMOKE_SKILLS_DIR" "$verify_root/skills-tree.txt"

  smoke_init_sandbox "$apply_root"
  smoke_write_server_config \
    "$SMOKE_CONFIG" \
    "$target_name" \
    "$command" \
    "$description" \
    "$read_only" \
    "${args[@]}"
  smoke_render_live_dossier "$fixture_path" "$SMOKE_CONFIG" "$apply_dossier"

  smoke_capture_mcpsmith apply apply --from-dossier "$apply_dossier" --yes --json --skills-dir "$SMOKE_SKILLS_DIR" --probe-timeout-seconds 30 --probe-retries 1
  smoke_assert_contains "$SMOKE_LOG_DIR/apply.stdout" "\"applied\": true"
  smoke_assert_not_contains "$SMOKE_CONFIG" "\"$target_name\""
  smoke_assert_file "$SMOKE_SKILLS_DIR/$target_name/SKILL.md"
  smoke_save_skills_tree "$SMOKE_SKILLS_DIR" "$apply_root/skills-tree.txt"
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
    "$(smoke_repo_root)/tests/fixtures/live/xcodebuildmcp-smoke.dossier.json" \
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
    "$(smoke_repo_root)/tests/fixtures/live/memory-smoke.dossier.json" \
    "npx" \
    "Memory and knowledge graph workflows" \
    true \
    "-y" \
    "@modelcontextprotocol/server-memory"
fi

if [[ "$SERVER" == "chrome-devtools" || "$SERVER" == "all" ]]; then
  run_live_target \
    "chrome-devtools" \
    "$(smoke_repo_root)/tests/fixtures/live/chrome-devtools-smoke.dossier.json" \
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
