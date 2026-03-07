#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/local-checks.sh [--fix]

Runs the standard local verification suite for mcpsmith.

Options:
  --fix   Run cargo fmt --all before clippy/test.
  -h      Show this help text.
EOF
}

fix_mode=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --fix)
      fix_mode=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown option: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [[ "$fix_mode" -eq 1 ]]; then
  echo "+ cargo fmt --all"
  cargo fmt --all
else
  echo "+ cargo fmt --all --check"
  cargo fmt --all --check
fi

echo "+ cargo clippy --all-targets -- -D warnings"
cargo clippy --all-targets -- -D warnings

echo "+ cargo test"
cargo test
