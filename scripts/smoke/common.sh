#!/usr/bin/env bash
set -euo pipefail

smoke_repo_root() {
  git rev-parse --show-toplevel 2>/dev/null || pwd
}

smoke_abs_path() {
  local path="$1"
  if [[ "$path" = /* ]]; then
    printf '%s\n' "$path"
  else
    printf '%s/%s\n' "$(pwd)" "$path"
  fi
}

smoke_timestamp_utc() {
  date -u +"%Y%m%dT%H%M%SZ"
}

smoke_default_run_root() {
  local category="$1"
  printf '%s/.codex-runtime/%s/%s\n' "$(smoke_repo_root)" "$category" "$(smoke_timestamp_utc)"
}

smoke_die() {
  echo "Error: $*" >&2
  exit 1
}

smoke_note() {
  echo "+ $*" >&2
}

smoke_require_cmd() {
  command -v "$1" >/dev/null 2>&1 || smoke_die "required command not found: $1"
}

smoke_json_escape() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  value=${value//$'\n'/\\n}
  printf '%s' "$value"
}

smoke_escape_sed_replacement() {
  local value="$1"
  value=${value//\\/\\\\}
  value=${value//&/\\&}
  printf '%s' "$value"
}

smoke_prepare_run_root() {
  local run_root
  run_root="$(smoke_abs_path "$1")"
  mkdir -p "$run_root"
  printf '%s\n' "$run_root"
}

smoke_init_sandbox() {
  local root
  root="$(smoke_abs_path "$1")"
  mkdir -p "$root"/home "$root"/skills "$root"/logs
  SMOKE_SANDBOX_ROOT="$root"
  SMOKE_HOME="$root/home"
  SMOKE_CONFIG="$root/mcp.json"
  SMOKE_SKILLS_DIR="$root/skills"
  SMOKE_DOSSIER="$root/dossier.json"
  SMOKE_REPORT="$root/contract-report.json"
  SMOKE_LOG_DIR="$root/logs"
}

smoke_capture_mcpsmith() {
  local step="$1"
  shift
  local stdout="$SMOKE_LOG_DIR/${step}.stdout"
  local stderr="$SMOKE_LOG_DIR/${step}.stderr"

  smoke_note "HOME=$SMOKE_HOME cargo run --quiet -- $*"
  (
    cd "$(smoke_repo_root)"
    HOME="$SMOKE_HOME" cargo run --quiet -- "$@"
  ) >"$stdout" 2>"$stderr"
}

smoke_capture_mcpsmith_expect_fail() {
  local step="$1"
  shift
  local stdout="$SMOKE_LOG_DIR/${step}.stdout"
  local stderr="$SMOKE_LOG_DIR/${step}.stderr"

  smoke_note "HOME=$SMOKE_HOME cargo run --quiet -- $* (expect failure)"
  set +e
  (
    cd "$(smoke_repo_root)"
    HOME="$SMOKE_HOME" cargo run --quiet -- "$@"
  ) >"$stdout" 2>"$stderr"
  local status=$?
  set -e

  if [[ "$status" -eq 0 ]]; then
    smoke_die "expected failure for step '$step', but command succeeded"
  fi
}

smoke_assert_file() {
  [[ -f "$1" ]] || smoke_die "expected file not found: $1"
}

smoke_assert_dir() {
  [[ -d "$1" ]] || smoke_die "expected directory not found: $1"
}

smoke_assert_contains() {
  local file="$1"
  local needle="$2"
  grep -Fq "$needle" "$file" || smoke_die "expected '$needle' in $file"
}

smoke_assert_not_contains() {
  local file="$1"
  local needle="$2"
  if grep -Fq "$needle" "$file"; then
    smoke_die "did not expect '$needle' in $file"
  fi
}

smoke_save_skills_tree() {
  local skills_dir="$1"
  local output="$2"
  find "$skills_dir" -print | sort >"$output"
}

smoke_write_mock_mcp_script() {
  local path="$1"
  shift
  local tools=()
  for name in "$@"; do
    tools+=("{\"name\":\"$name\",\"description\":\"Tool $name\",\"inputSchema\":{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{\"query\":{\"type\":\"string\"}}}}")
  done
  local joined
  joined="$(IFS=,; printf '%s' "${tools[*]}")"
  cat >"$path" <<EOF
#!/bin/sh
while IFS= read -r line; do
  case "\$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{"jsonrpc":"2.0","id":2,"result":{"tools":[${joined}]}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "\$line" | grep -q '"query":"'; then
        printf '{"jsonrpc":"2.0","id":2,"result":{"content":[{"type":"text","text":"ok"}],"isError":false}}\n'
      else
        printf '{"jsonrpc":"2.0","id":2,"error":{"code":-32602,"message":"invalid query"}}\n'
      fi
      ;;
  esac
done
EOF
  chmod +x "$path"
}

smoke_write_mock_codex_script() {
  local path="$1"
  shift
  local confidence="${1:-0.9}"
  shift || true
  local workflows=()
  for name in "$@"; do
    workflows+=("{\"id\":\"$name\",\"title\":\"$name workflow\",\"goal\":\"Perform $name operations without relying on the MCP server.\",\"when_to_use\":\"Use this when you need to run the $name workflow with native commands.\",\"trigger_phrases\":[\"run $name\",\"use $name\"],\"origin_tools\":[\"$name\"],\"stop_and_ask\":[\"Stop if the required inputs are ambiguous or the native command would mutate state unexpectedly.\"],\"native_steps\":[{\"title\":\"Run the native command\",\"command\":\"printf '%s\\\\n' 'collected query goes here'\",\"details\":\"Replace 'collected query goes here' with the exact collected query value before running the command.\"}],\"verification\":[\"Confirm the native command completed successfully and produced output.\"],\"return_contract\":[\"Return the command output together with the exact query value used.\"],\"confidence\":${confidence}}")
  done
  local payload
  payload="$(IFS=,; printf '%s' "${workflows[*]}")"
  cat >"$path" <<EOF
#!/bin/sh
if [ "\$1" = "--version" ] || [ "\$1" = "-v" ] || [ "\$1" = "version" ]; then
  echo "mock-codex"
  exit 0
fi
last_message_file=""
while [ \$# -gt 0 ]; do
  case "\$1" in
    --output-last-message|-o)
      last_message_file="\$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
cat >/dev/null
[ -n "\$last_message_file" ] || exit 12
cat >"\$last_message_file" <<'JSON'
{"workflow_skills":[${payload}]}
JSON
EOF
  chmod +x "$path"
}

smoke_write_mock_claude_script() {
  local path="$1"
  shift
  local workflows=()
  local name
  for name in "$@"; do
    workflows+=("{\"id\":\"$name\",\"title\":\"$name workflow\",\"goal\":\"Perform $name operations without relying on the MCP server.\",\"when_to_use\":\"Use this when you need to run the $name workflow with native commands.\",\"trigger_phrases\":[\"run $name\",\"use $name\"],\"origin_tools\":[\"$name\"],\"stop_and_ask\":[\"Stop if the required inputs are ambiguous or the native command would mutate state unexpectedly.\"],\"native_steps\":[{\"title\":\"Run the native command\",\"command\":\"printf '%s\\\\n' 'collected query goes here'\",\"details\":\"Replace 'collected query goes here' with the exact collected query value before running the command.\"}],\"verification\":[\"Confirm the native command completed successfully and produced output.\"],\"return_contract\":[\"Return the command output together with the exact query value used.\"],\"confidence\":0.85}")
  done
  local payload
  payload="$(IFS=,; printf '%s' "${workflows[*]}")"
  cat >"$path" <<EOF
#!/bin/sh
if [ "\$1" = "--version" ] || [ "\$1" = "-v" ] || [ "\$1" = "version" ]; then
  echo "mock-claude"
  exit 0
fi
cat >/dev/null
cat <<'JSON'
{"workflow_skills":[${payload}]}
JSON
EOF
  chmod +x "$path"
}

smoke_write_server_config() {
  local path="$1"
  local server_name="$2"
  local command="$3"
  local description="$4"
  local read_only="$5"
  shift 5

  local args_json=""
  if [[ "$#" -gt 0 ]]; then
    local pieces=()
    local arg
    for arg in "$@"; do
      pieces+=("\"$(smoke_json_escape "$arg")\"")
    done
    args_json="$(IFS=,; printf '%s' "${pieces[*]}")"
    args_json=$(printf '[%s]' "$args_json")
  fi

  {
    printf '{\n'
    printf '  "mcpServers": {\n'
    printf '    "%s": {\n' "$(smoke_json_escape "$server_name")"
    printf '      "command": "%s"' "$(smoke_json_escape "$command")"
    if [[ -n "$args_json" ]]; then
      printf ',\n      "args": %s' "$args_json"
    fi
    if [[ -n "$description" ]]; then
      printf ',\n      "description": "%s"' "$(smoke_json_escape "$description")"
    fi
    case "$read_only" in
      true)
        printf ',\n      "readOnly": true'
        ;;
      false)
        printf ',\n      "readOnly": false'
        ;;
    esac
    printf '\n    }\n'
    printf '  }\n'
    printf '}\n'
  } >"$path"
}

smoke_render_live_dossier() {
  local template_path="$1"
  local config_path="$2"
  local output_path="$3"
  local escaped
  escaped="$(smoke_escape_sed_replacement "$(smoke_abs_path "$config_path")")"
  sed "s|__CONFIG_PATH__|$escaped|g" "$template_path" >"$output_path"
}

smoke_resolve_cli_verify_script() {
  local candidate
  for candidate in \
    "$HOME/.agents/skills/cli-verify/scripts/cli_verify_session.sh" \
    "$HOME/.claude/skills/cli-verify/scripts/cli_verify_session.sh" \
    "$HOME/.codex/skills/cli-verify/scripts/cli_verify_session.sh"
  do
    if [[ -x "$candidate" ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done
  smoke_die "cli-verify helper not found"
}

smoke_require_ghostty() {
  if command -v ghostty >/dev/null 2>&1; then
    return 0
  fi
  if open -Ra Ghostty >/dev/null 2>&1; then
    return 0
  fi
  smoke_die "Ghostty is unavailable. Install it or expose it on PATH before running cli_verify_smoke.sh."
}

smoke_cli_verify_send_line() {
  local script="$1"
  local state_file="$2"
  local command="$3"
  "$script" send --state-file "$state_file" C-c
  "$script" send --state-file "$state_file" "$command" Enter
}

smoke_cli_verify_wait_for_text() {
  local script="$1"
  local state_file="$2"
  local needle="$3"
  local timeout_seconds="${4:-30}"
  local pane
  local elapsed=0

  while (( elapsed < timeout_seconds )); do
    pane="$("$script" pane --state-file "$state_file" --lines 500)"
    if printf '%s' "$pane" | grep -Fq "$needle"; then
      return 0
    fi
    sleep 1
    elapsed=$((elapsed + 1))
  done

  smoke_die "timed out waiting for '$needle' in cli-verify pane"
}

smoke_cli_verify_capture_pair() {
  local script="$1"
  local state_file="$2"
  local output_root="$3"
  local label="$4"
  mkdir -p "$output_root/panes" "$output_root/screenshots"
  "$script" pane --state-file "$state_file" --lines 500 >"$output_root/panes/${label}.txt"
  "$script" screenshot --state-file "$state_file" --out "$output_root/screenshots/${label}.png" >/dev/null
}

smoke_print_artifacts() {
  local root="$1"
  smoke_note "Artifacts under $root"
  find "$root" \
    \( -path "*/home" -o -path "*/home/*" \) -prune -o \
    -print | sort
}
