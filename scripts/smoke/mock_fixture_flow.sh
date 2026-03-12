#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=./common.sh
source "$SCRIPT_DIR/common.sh"

usage() {
  cat <<'EOF'
Usage: scripts/smoke/mock_fixture_flow.sh [--run-dir PATH]

Runs a deterministic mock MCP smoke flow with isolated HOME, config, skills,
and artifact capture under .codex-runtime.
EOF
}

RUN_ROOT=""
while [[ $# -gt 0 ]]; do
  case "$1" in
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

smoke_require_cmd cargo

RUN_ROOT="${RUN_ROOT:-$(smoke_default_run_root "smoke/mock")}"
RUN_ROOT="$(smoke_prepare_run_root "$RUN_ROOT")"

stepwise_root="$RUN_ROOT/stepwise"
oneshot_root="$RUN_ROOT/one-shot"

smoke_init_sandbox "$stepwise_root"
stepwise_mock_mcp="$stepwise_root/mock-mcp.sh"
stepwise_mock_codex="$stepwise_root/mock-codex.sh"
smoke_write_mock_mcp_script "$stepwise_mock_mcp" execute
smoke_write_mock_codex_script "$stepwise_mock_codex" 0.9 execute
smoke_write_server_config \
  "$SMOKE_CONFIG" \
  "playwright" \
  "$stepwise_mock_mcp" \
  "Read-only browser helpers" \
  true

export MCPSMITH_CODEX_COMMAND="$stepwise_mock_codex"

smoke_capture_mcpsmith resolve resolve playwright --json --config "$SMOKE_CONFIG"
resolve_artifact="$(smoke_json_artifact_path "$SMOKE_LOG_DIR/resolve.stdout")"
smoke_assert_contains "$SMOKE_LOG_DIR/resolve.stdout" "\"blocked\": false"

smoke_capture_mcpsmith snapshot snapshot --json --from-resolve "$resolve_artifact"
snapshot_artifact="$(smoke_json_artifact_path "$SMOKE_LOG_DIR/snapshot.stdout")"
smoke_assert_contains "$SMOKE_LOG_DIR/snapshot.stdout" "\"source_root\""

smoke_capture_mcpsmith evidence evidence --json --from-snapshot "$snapshot_artifact"
evidence_artifact="$(smoke_json_artifact_path "$SMOKE_LOG_DIR/evidence.stdout")"
smoke_assert_contains "$SMOKE_LOG_DIR/evidence.stdout" "\"tool_evidence\""

smoke_capture_mcpsmith synthesize synthesize --json --from-evidence "$evidence_artifact" --backend codex
synthesis_artifact="$(smoke_json_artifact_path "$SMOKE_LOG_DIR/synthesize.stdout")"
smoke_assert_contains "$SMOKE_LOG_DIR/synthesize.stdout" "\"blocked\": false"

smoke_capture_mcpsmith review review --json --from-bundle "$synthesis_artifact" --backend codex
review_artifact="$(smoke_json_artifact_path "$SMOKE_LOG_DIR/review.stdout")"
smoke_assert_contains "$SMOKE_LOG_DIR/review.stdout" "\"approved\": true"

smoke_capture_mcpsmith verify verify --json --from-bundle "$review_artifact"
smoke_assert_contains "$SMOKE_LOG_DIR/verify.stdout" "\"passed\": true"

smoke_init_sandbox "$oneshot_root"
oneshot_mock_mcp="$oneshot_root/mock-mcp.sh"
oneshot_mock_codex="$oneshot_root/mock-codex.sh"
smoke_write_mock_mcp_script "$oneshot_mock_mcp" execute
smoke_write_mock_codex_script "$oneshot_mock_codex" 0.9 execute
smoke_write_server_config \
  "$SMOKE_CONFIG" \
  "playwright" \
  "$oneshot_mock_mcp" \
  "Read-only browser helpers" \
  true

export MCPSMITH_CODEX_COMMAND="$oneshot_mock_codex"

smoke_capture_mcpsmith one-shot playwright --json --backend codex --config "$SMOKE_CONFIG" --skills-dir "$SMOKE_SKILLS_DIR"
smoke_assert_contains "$SMOKE_LOG_DIR/one-shot.stdout" "\"status\": \"applied\""
smoke_assert_contains "$SMOKE_LOG_DIR/one-shot.stdout" "\"mcp_config_updated\": true"
smoke_assert_not_contains "$SMOKE_CONFIG" "\"playwright\""
smoke_assert_file "$SMOKE_SKILLS_DIR/playwright/SKILL.md"
smoke_assert_file "$SMOKE_SKILLS_DIR/playwright/.mcpsmith/manifest.json"
smoke_save_skills_tree "$SMOKE_SKILLS_DIR" "$oneshot_root/skills-tree.txt"

unset MCPSMITH_CODEX_COMMAND

smoke_print_artifacts "$RUN_ROOT"
