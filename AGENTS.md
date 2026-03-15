# mcpsmith Agent Guide

## What mcpsmith does

`mcpsmith` converts MCP servers into standalone skill packs from real source
artifacts. The current flow resolves the installed MCP, snapshots the exact
source, builds tool evidence, synthesizes grounded skills, runs a second review
pass, verifies the skill artifacts, and can atomically write skills while
removing the MCP config entry.

All work stays in `mcpsmith`. `distill` is historical context
only and must not be modified from this repo workflow.

## Finished scope

- There is no standing roadmap file in the repo.
- The shipped product is the current one-shot flow plus the staged `resolve -> snapshot -> evidence -> synthesize -> review -> verify` pipeline.
- Deterministic evidence extraction is the default path. The mapper fallback is intentionally narrow and only for low-confidence tools.
- Default catalog scope is `official` plus `smithery`. Expand providers or resolver coverage only for real blocked conversions.

## Preferred change flow

1. Add or update tests first for behavior changes.
2. Keep changes task-scoped and local to `mcpsmith`.
3. Run `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before finishing user-visible CLI or output changes.
4. Use `$cli-verify` in Ghostty for CLI/help/output verification and keep both pane captures and screenshots.
5. Keep commits small and directly tied to the task in progress.

## Command matrix

- Overview: `cargo run --quiet --`
- Local discovery: `cargo run --quiet -- discover --json`
- Root help: `cargo run --quiet -- --help`
- One-shot conversion: `cargo run --quiet -- <server>`
- Full one-shot with explicit subcommand: `cargo run --quiet -- run <server>`
- Catalog: `cargo run --quiet -- catalog sync`, `cargo run --quiet -- catalog stats`
- Staged resolve: `cargo run --quiet -- resolve <server> --json`
- Staged snapshot: `cargo run --quiet -- snapshot <server|--from-resolve artifact.json> --json`
- Staged evidence: `cargo run --quiet -- evidence <server|--from-snapshot artifact.json> --json`
- Staged synthesis: `cargo run --quiet -- synthesize <server|--from-evidence artifact.json> --json`
- Staged review: `cargo run --quiet -- review <server|--from-bundle artifact.json> --json`
- Staged verify: `cargo run --quiet -- verify <server|--from-bundle artifact.json> --json`
- Command help: `cargo run --quiet -- <command> --help`

What each stage emits:

- `discover`: local MCP inventory from discovered config files and any explicit `--config` paths
- `resolve`: exact artifact identity and block reason if source is unavailable
- `snapshot`: local source root for the pinned artifact
- `evidence`: per-tool registration, handler, test/doc citations, and confidence
- `synthesize`: drafted skills from the evidence bundle
- `review`: second-pass approval, findings, and revised drafts when applicable
- `verify`: final format/grounding/reference checks

## Isolated runtime rules

- Never verify against a real user home directory.
- Set `HOME="$TMPDIR/<session-home>"` for tests and manual verification.
- Pass `--config "$TMPDIR/mcp.json"` when exercising MCP config discovery.
- Pass `--skills-dir "$TMPDIR/skills"` when generating output.
- Keep transient repo-local state under `.codex-runtime/`.
- Default config path is `~/.mcpsmith/config.yaml`.
- Default installed skill path is `~/.agents/skills/`.

## Backend behavior

- Backend selection today is: explicit `--backend`, then config `backend.preference`, then auto-detect installed backends in `codex` then `claude` order.
- Use `--backend-auto` when you want the CLI to fall back automatically.
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
- `mcpsmith resolve --help`
- `mcpsmith run --help`
- one real error path
- one staged success flow
- one one-shot success flow

Every screenshot must be paired with pane capture output.

## jj expectations

- Start each task from the latest `main`.
- Use one `jj` change per task family whenever dependencies allow.
- If the user asks for git commits, keep commit boundaries aligned with the active task.
- Do not recouple `mcpsmith` with `distill`.

## Live-MCP verification expectations

- Runtime discovery should use real `tools/list` introspection.
- Live smoke is confidence evidence, not a hard gate for skill generation.
- Keep live MCP validation isolated with temp `HOME`, temp config files, and temp skills output.
- Preserve staged artifacts and run reports when they help explain failures or confirm behavior.
- For catalog verification, remember the default scope is only `official` and `smithery`.

## Release workflow

- `Release` workflow runs on push to `main` and on `workflow_dispatch`.
- Successful runs publish GitHub release artifacts, publish `mcpsmith-core` and `mcpsmith` to crates.io, and update `nclandrei/homebrew-tap`.
- The workflow auto-creates the `v<version>` tag from `Cargo.toml`; do not push release tags manually.
- When changing packaging or release logic, run `./scripts/smoke/smoke-test-installed-mcpsmith.sh <binary-or-tarball>` locally against a built artifact.
- The tap formula is rendered from the published `mcpsmith-<version>.crate` tarball on crates.io because the GitHub repo is private.
- The renderer lives at `scripts/release/render-homebrew-formula.sh`.
- Required GitHub Actions secrets are `CARGO_REGISTRY_TOKEN` and `HOMEBREW_TAP_TOKEN`.
