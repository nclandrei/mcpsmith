#!/usr/bin/env bash
set -euo pipefail

input="${1:-}"
if [ -z "$input" ]; then
  echo "usage: $0 <mcpsmith-binary-or-tarball>" >&2
  exit 64
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=./common.sh
source "$REPO_ROOT/scripts/smoke/common.sh"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

resolve_binary() {
  local source="$1"
  if [ -d "$source" ]; then
    find "$source" -type f -name mcpsmith -perm -u+x | head -n 1
    return
  fi

  case "$source" in
    *.tar.xz)
      local extracted="$workdir/extracted"
      mkdir -p "$extracted"
      tar -xJf "$source" -C "$extracted"
      find "$extracted" -type f -name mcpsmith -perm -u+x | head -n 1
      ;;
    *)
      printf '%s\n' "$source"
      ;;
  esac
}

bin_path="$(resolve_binary "$input")"
if [ -z "$bin_path" ] || [ ! -x "$bin_path" ]; then
  echo "could not find executable mcpsmith binary from: $input" >&2
  exit 1
fi

home_dir="$workdir/home"
skills_dir="$workdir/skills"
config_path="$workdir/mcp.json"
mock_mcp="$workdir/mock-mcp.sh"
mkdir -p "$home_dir" "$skills_dir"

smoke_write_mock_mcp_script "$mock_mcp" execute
smoke_write_server_config \
  "$config_path" \
  "playwright" \
  "$mock_mcp" \
  "Read-only browser helpers" \
  true

export HOME="$home_dir"

"$bin_path" --help >"$workdir/help.out"
grep -q "One-shot conversion:" "$workdir/help.out"
grep -q "Catalog sync defaults to official + smithery." "$workdir/help.out"

"$bin_path" resolve --help >"$workdir/resolve-help.out"
grep -q "Writes a resolve artifact that snapshot can consume with --from-resolve." \
  "$workdir/resolve-help.out"

resolve_json="$("$bin_path" resolve playwright --json --config "$config_path")"
printf '%s\n' "$resolve_json" >"$workdir/resolve.json"
resolve_artifact="$(sed -n 's/^[[:space:]]*"artifact_path":[[:space:]]*"\(.*\)",$/\1/p' "$workdir/resolve.json" | head -n1)"
[ -n "$resolve_artifact" ] && [ -f "$resolve_artifact" ]

snapshot_json="$("$bin_path" snapshot --json --from-resolve "$resolve_artifact")"
printf '%s\n' "$snapshot_json" >"$workdir/snapshot.json"
snapshot_artifact="$(sed -n 's/^[[:space:]]*"artifact_path":[[:space:]]*"\(.*\)",$/\1/p' "$workdir/snapshot.json" | head -n1)"
[ -n "$snapshot_artifact" ] && [ -f "$snapshot_artifact" ]

"$bin_path" evidence --json --from-snapshot "$snapshot_artifact" >"$workdir/evidence.json"
grep -q '"tool_evidence"' "$workdir/evidence.json"
grep -q '"tool_name": "execute"' "$workdir/evidence.json"

echo "smoke test passed for $bin_path"
