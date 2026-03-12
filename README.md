# mcpsmith

`mcpsmith` converts MCP servers into source-grounded, agent-native skill packs.
It resolves the real artifact behind an installed MCP, snapshots the source,
extracts tool evidence, synthesizes skills from that evidence, runs a second
agent review pass, verifies the generated artifacts, and can then atomically
install skills while removing the MCP config entry.

This repo is the standalone product. `distill` is historical context only.

## What mcpsmith does

- Uses the MCP actually installed on disk, not just registry metadata.
- Produces staged artifacts that other agents can consume without prompting.
- Supports a single one-shot command and an inspectable staged pipeline.
- Keeps generated skills under `~/.agents/skills/` by default.

## How it works

1. Find the MCP in local config and resolve its exact artifact identity.
2. Materialize a local source snapshot for that exact artifact.
3. Locate MCP definitions, tool handlers, tests, and docs to build evidence.
4. Ask a backend to synthesize grounded skills from the evidence.
5. Run a second review pass on the generated skills.
6. Verify the resulting skill artifacts for format, grounding, and references.
7. Optionally install the skills and remove the MCP entry atomically.

## One-shot flow

Use one-shot when you want the full conversion in a single run:

```bash
tmpdir="$(mktemp -d)"
HOME="$tmpdir/home" cargo run --quiet -- \
  playwright \
  --config "$tmpdir/mcp.json" \
  --skills-dir "$tmpdir/skills"
```

Add `--dry-run` to execute the full pipeline without mutating installed skills
or MCP config.

Useful one-shot flags:

- `--json` for machine-readable output.
- `--backend codex|claude` to force a backend.
- `--backend-auto` to allow fallback when a preferred backend is unavailable.
- `--skills-dir <PATH>` to send generated skills somewhere isolated.

## Staged flow

Use the staged flow when you want inspectable artifacts between phases:

```bash
tmpdir="$(mktemp -d)"

resolve_json="$(
  HOME="$tmpdir/home" cargo run --quiet -- \
    resolve playwright \
    --json \
    --config "$tmpdir/mcp.json"
)"
resolve_artifact="$(printf '%s\n' "$resolve_json" | sed -n 's/.*"artifact_path": "\(.*\)".*/\1/p' | head -n1)"

snapshot_json="$(
  HOME="$tmpdir/home" cargo run --quiet -- \
    snapshot \
    --json \
    --from-resolve "$resolve_artifact"
)"
snapshot_artifact="$(printf '%s\n' "$snapshot_json" | sed -n 's/.*"artifact_path": "\(.*\)".*/\1/p' | head -n1)"

evidence_json="$(
  HOME="$tmpdir/home" cargo run --quiet -- \
    evidence \
    --json \
    --from-snapshot "$snapshot_artifact"
)"
evidence_artifact="$(printf '%s\n' "$evidence_json" | sed -n 's/.*"artifact_path": "\(.*\)".*/\1/p' | head -n1)"

synthesis_json="$(
  HOME="$tmpdir/home" cargo run --quiet -- \
    synthesize \
    --json \
    --from-evidence "$evidence_artifact" \
    --backend codex
)"
synthesis_artifact="$(printf '%s\n' "$synthesis_json" | sed -n 's/.*"artifact_path": "\(.*\)".*/\1/p' | head -n1)"

review_json="$(
  HOME="$tmpdir/home" cargo run --quiet -- \
    review \
    --json \
    --from-bundle "$synthesis_artifact" \
    --backend codex
)"
review_artifact="$(printf '%s\n' "$review_json" | sed -n 's/.*"artifact_path": "\(.*\)".*/\1/p' | head -n1)"

HOME="$tmpdir/home" cargo run --quiet -- \
  verify \
  --json \
  --from-bundle "$review_artifact"
```

The staged artifact files are written under `.codex-runtime/stages/` and can be
reused by another agent with `--from-resolve`, `--from-snapshot`,
`--from-evidence`, and `--from-bundle`.

## Catalog and source resolution

`mcpsmith` has two source inputs:

- Local MCP config entries from `--config` or discovered config files.
- API-backed registry data used for catalog/census and fallback enrichment.

Catalog commands:

- `cargo run --quiet -- catalog sync`
- `cargo run --quiet -- catalog stats`

Resolution order is deterministic:

1. local path
2. npm package and version
3. PyPI package and version
4. repository URL and revision
5. cached catalog fallback only if direct identity is insufficient

Remote-only or source-unavailable servers are blocked instead of being
converted from metadata alone.

## Backend behavior

Backend selection order is:

1. explicit `--backend`
2. `backend.preference` from `~/.mcpsmith/config.yaml`
3. auto-detect installed backends in `codex`, then `claude` order

Use `MCPSMITH_CODEX_COMMAND` and `MCPSMITH_CLAUDE_COMMAND` for tests and local
backend overrides.

## Output and artifacts

Generated skills default to `~/.agents/skills/`:

```text
~/.agents/skills/
  playwright/
    SKILL.md
    .mcpsmith/
      manifest.json
  playwright--execute/
    SKILL.md
```

Each staged command writes a JSON artifact under `.codex-runtime/stages/`.
One-shot runs also emit a run report with paths for `resolve`, `snapshot`,
`evidence`, `synthesis`, `review`, and `verify`.

## Examples

Sample fixtures live under [`examples/`](examples):

- [`examples/sample-mcp-config.json`](examples/sample-mcp-config.json)
- [`examples/sample-dossier.json`](examples/sample-dossier.json)
- [`examples/sample-contract-report.json`](examples/sample-contract-report.json)
- [`examples/sample-skill-pack-tree.txt`](examples/sample-skill-pack-tree.txt)

The active follow-up plan for other agents lives in [`NEXT-STEPS.md`](NEXT-STEPS.md).

## Troubleshooting

- No servers resolved: pass `--config "$TMPDIR/mcp.json"` and confirm the file
  contains an `mcpServers` object.
- Artifact resolution blocked: inspect the `resolve` artifact to see whether
  the server is remote-only or missing exact source identity.
- Synthesis blocked: inspect the `evidence` artifact for missing handler/tests
  citations or unresolved tool locations.
- Review rejected a skill: inspect the `review` artifact and rerun synthesis
  with a better backend or better source evidence.
- Apply blocked: use `--dry-run` first, then inspect the staged artifacts
  before rerunning one-shot without `--dry-run`.

## Isolated verification

For local checks and smoke verification, keep all mutable state out of your real
home directory:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Smoke helpers in this repo:

- `./scripts/smoke/mock_fixture_flow.sh`
- `./scripts/smoke/live_public_mcp.sh --server memory`
- `./scripts/smoke/live_public_mcp.sh --server chrome-devtools`
- `./scripts/smoke/cli_verify_smoke.sh`

For live or visual verification, set an isolated `HOME`, pass an explicit
`--config`, and write skills into an isolated `--skills-dir`.
