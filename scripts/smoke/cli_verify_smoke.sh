#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/smoke/cli_verify_smoke.sh [--run-dir PATH] [--state-file PATH]

Runs the required Ghostty + tmux visual verification flow for mcpsmith and
stores pane/screenshot pairs under .codex-runtime.
EOF
}

RUN_ROOT=""
STATE_FILE=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --run-dir)
      RUN_ROOT="$2"
      shift 2
      ;;
    --state-file)
      STATE_FILE="$2"
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

smoke_require_cmd cargo
smoke_require_cmd tmux
smoke_require_ghostty

REPO_ROOT="$(smoke_repo_root)"
RUN_ROOT="${RUN_ROOT:-$(smoke_default_run_root "cli-verify-smoke")}"
RUN_ROOT="$(smoke_prepare_run_root "$RUN_ROOT")"
STATE_FILE="${STATE_FILE:-$RUN_ROOT/cli-verify-state.env}"
STATE_FILE="$(smoke_abs_path "$STATE_FILE")"
CLI_VERIFY_SCRIPT="$(smoke_resolve_cli_verify_script)"
EVIDENCE_ROOT="$RUN_ROOT/evidence"

smoke_init_sandbox "$RUN_ROOT/stepwise"
STEP_HOME="$SMOKE_HOME"
STEP_CONFIG="$SMOKE_CONFIG"
STEP_SKILLS_DIR="$SMOKE_SKILLS_DIR"
STEP_DOSSIER="$SMOKE_DOSSIER"
STEP_REPORT="$SMOKE_REPORT"
STEP_MOCK_MCP="$RUN_ROOT/stepwise/mock-mcp.sh"
STEP_MOCK_CODEX="$RUN_ROOT/stepwise/mock-codex.sh"
smoke_write_mock_mcp_script "$STEP_MOCK_MCP" execute
smoke_write_mock_codex_script "$STEP_MOCK_CODEX" 0.9 execute
smoke_write_server_config \
  "$STEP_CONFIG" \
  "playwright" \
  "$STEP_MOCK_MCP" \
  "Read-only browser helpers" \
  true

smoke_init_sandbox "$RUN_ROOT/one-shot"
ONE_HOME="$SMOKE_HOME"
ONE_CONFIG="$SMOKE_CONFIG"
ONE_SKILLS_DIR="$SMOKE_SKILLS_DIR"
ONE_MOCK_MCP="$RUN_ROOT/one-shot/mock-mcp.sh"
ONE_MOCK_CODEX="$RUN_ROOT/one-shot/mock-codex.sh"
smoke_write_mock_mcp_script "$ONE_MOCK_MCP" execute
smoke_write_mock_codex_script "$ONE_MOCK_CODEX" 0.9 execute
smoke_write_server_config \
  "$ONE_CONFIG" \
  "playwright" \
  "$ONE_MOCK_MCP" \
  "Read-only browser helpers" \
  true

"$CLI_VERIFY_SCRIPT" init --repo "$REPO_ROOT" --state-file "$STATE_FILE" --restart

run_visual_step() {
  local label="$1"
  local command="$2"
  local needle="$3"
  local timeout="${4:-60}"

  "$CLI_VERIFY_SCRIPT" send --state-file "$STATE_FILE" C-c
  "$CLI_VERIFY_SCRIPT" send --state-file "$STATE_FILE" "clear" Enter
  sleep 1
  smoke_cli_verify_send_line "$CLI_VERIFY_SCRIPT" "$STATE_FILE" "$command"
  smoke_cli_verify_wait_for_text "$CLI_VERIFY_SCRIPT" "$STATE_FILE" "$needle" "$timeout"
  smoke_cli_verify_capture_pair "$CLI_VERIFY_SCRIPT" "$STATE_FILE" "$EVIDENCE_ROOT" "$label"
}

run_visual_step \
  "help-root" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- --help" \
  "Usage: mcpsmith [OPTIONS] [SERVER] [COMMAND]"

run_visual_step \
  "help-discover" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- discover --help" \
  "Usage: mcpsmith discover [OPTIONS] [SERVER]"

run_visual_step \
  "stepwise-discover" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" MCPSMITH_CODEX_COMMAND=\"$STEP_MOCK_CODEX\" cargo run --quiet -- discover playwright --json --out \"$STEP_DOSSIER\" --config \"$STEP_CONFIG\"" \
  "\"server_gate\": \"ready\""

run_visual_step \
  "stepwise-build" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- build --from-dossier \"$STEP_DOSSIER\" --skills-dir \"$STEP_SKILLS_DIR\" --json" \
  "\"orchestrator_skill_path\""

run_visual_step \
  "error-apply-missing-yes" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- apply --from-dossier \"$STEP_DOSSIER\" --skills-dir \"$STEP_SKILLS_DIR\"" \
  "apply requires --yes"

run_visual_step \
  "stepwise-verify" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- verify playwright --json --config \"$STEP_CONFIG\" --skills-dir \"$STEP_SKILLS_DIR\"" \
  "\"passed\": true"

run_visual_step \
  "stepwise-contract" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- contract-test --from-dossier \"$STEP_DOSSIER\" --report \"$STEP_REPORT\" --json" \
  "\"passed\": true"

run_visual_step \
  "stepwise-apply" \
  "cd \"$REPO_ROOT\" && HOME=\"$STEP_HOME\" cargo run --quiet -- apply --from-dossier \"$STEP_DOSSIER\" --yes --json --skills-dir \"$STEP_SKILLS_DIR\"" \
  "\"mcp_config_updated\": true"

run_visual_step \
  "one-shot-success" \
  "cd \"$REPO_ROOT\" && HOME=\"$ONE_HOME\" MCPSMITH_CODEX_COMMAND=\"$ONE_MOCK_CODEX\" cargo run --quiet -- playwright --json --config \"$ONE_CONFIG\" --skills-dir \"$ONE_SKILLS_DIR\"" \
  "\"mcp_config_updated\": true"

smoke_print_artifacts "$RUN_ROOT"
