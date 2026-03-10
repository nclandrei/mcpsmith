# mcpsmith Agent Guide

## What mcpsmith does

`mcpsmith` converts MCP servers into standalone skill packs. The current flow discovers live MCP tools with `tools/list`, probes real behavior with `tools/call`, builds dossier-driven skills, contract-tests them, and can atomically write skills while removing the MCP config entry.

All work for this plan stays in `mcpsmith`. `distill` is historical context only and must not be modified from this repo workflow.

## Preferred change flow

1. Add or update tests first for behavior changes.
2. Keep changes task-scoped and local to `mcpsmith`.
3. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before finishing user-visible CLI or output changes.
4. Use `$cli-verify` in Ghostty for CLI/help/output verification and keep both pane captures and screenshots.
5. Keep commits small and directly tied to the task in progress.

## Command matrix

- Overview: `cargo run --quiet --`
- One-shot conversion: `cargo run --quiet -- <server>`
- Discover dossier JSON: `cargo run --quiet -- discover <server|--all> --out dossier.json`
- Build skills from dossier: `cargo run --quiet -- build --from-dossier dossier.json`
- Contract-test dossier: `cargo run --quiet -- contract-test --from-dossier dossier.json`
- Apply passing dossier: `cargo run --quiet -- apply --from-dossier dossier.json --yes`
- Diagnostics: `cargo run --quiet -- list`, `cargo run --quiet -- inspect <server>`, `cargo run --quiet -- plan <server>`, `cargo run --quiet -- verify <server>`
- Help: `cargo run --quiet -- --help`, `cargo run --quiet -- <command> --help`

## Isolated runtime rules

- Never verify against a real user home directory.
- Set `HOME="$TMPDIR/<session-home>"` for tests and manual verification.
- Pass `--config "$TMPDIR/mcp.json"` when exercising MCP config discovery.
- Pass `--skills-dir "$TMPDIR/skills"` when generating output.
- Keep transient repo-local state under `.codex-runtime/`.
- Default config path is `~/.mcpsmith/config.yaml`.
- Default installed skill path is `~/.agents/skills/`.

## Backend and probe behavior

- Backend selection today is: explicit `--backend`, then config `backend.preference`, then auto-detect installed backends in `codex` then `claude` order. Legacy `convert.*` keys are input-only compatibility.
- Use `--backend-health` when debugging backend availability.
- Runtime probe controls are `--allow-side-effects`, `--probe-timeout-seconds <N>`, and `--probe-retries <N>`.
- Test and local backend overrides use `MCPSMITH_CODEX_COMMAND` and `MCPSMITH_CLAUDE_COMMAND`.

## cli-verify workflow

Use this exact baseline when verifying `mcpsmith` in Ghostty:

```bash
REPO_ROOT="/Users/anicolae/code/mcpsmith"
STATE="$REPO_ROOT/.codex-runtime/cli-verify-session.env"
SCRIPT="$HOME/.agents/skills/cli-verify/scripts/cli_verify_session.sh"
[ -x "$SCRIPT" ] || SCRIPT="$HOME/.codex/skills/cli-verify/scripts/cli_verify_session.sh"

APP_CMD='cd /Users/anicolae/code/mcpsmith && cargo run --quiet --'

"$SCRIPT" init \
  --repo "$REPO_ROOT" \
  --state-file "$STATE" \
  --socket cli-verify \
  --session cli-verify \
  --command "$APP_CMD"

"$SCRIPT" pane --state-file "$STATE" --lines 200
"$SCRIPT" screenshot --state-file "$STATE"
```

Minimum visual proofs for CLI changes:

- `mcpsmith --help`
- `mcpsmith discover --help`
- one real error path
- one stepwise success flow
- one one-shot success flow

Every screenshot must be paired with pane capture output.

## jj expectations

- Start each task from the latest `main`.
- Use one `jj` change per task family whenever dependencies allow.
- If the user asks for git commits, keep commit boundaries aligned with the active task.
- Do not recouple `mcpsmith` with `distill`.

## Live-MCP verification expectations

- Discovery and verification work should use real runtime `tools/list` introspection.
- Contract testing should exercise real runtime `tools/call` probes, with side effects disabled unless explicitly allowed.
- Keep live MCP validation isolated with temp `HOME`, temp config files, and temp skills output.
- Preserve dossier JSON and contract-test reports when they help explain failures or confirm behavior.
