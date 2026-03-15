# mcpsmith

[![CI](https://github.com/nclandrei/mcpsmith/actions/workflows/ci.yml/badge.svg)](https://github.com/nclandrei/mcpsmith/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/mcpsmith.svg)](https://crates.io/crates/mcpsmith)
[![License: MIT](https://img.shields.io/badge/license-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

`mcpsmith` turns installed Model Context Protocol servers into source-grounded, agent-native skill packs.

It resolves the exact artifact behind a configured MCP, snapshots the real source, extracts per-tool evidence, synthesizes grounded skills, runs a second review pass, verifies the result, and can install the skills while removing the MCP config entry.

If an MCP mostly wraps local code or repeatable workflows, `mcpsmith` gives you something inspectable, versionable, and easier for agents to reuse than a live server dependency.

This repo is the standalone product. `distill` is historical context only.

## Installation

```bash
# Homebrew
brew tap nclandrei/tap
brew install nclandrei/tap/mcpsmith

# crates.io
cargo install mcpsmith
```

The Homebrew formula installs from the published `mcpsmith` crate on crates.io. That keeps installation working even though release automation renders the formula from the crate tarball rather than the GitHub source checkout.

## Quick start

Use the installed `mcpsmith` binary below. Point `--config` at an MCP config file you already use. If you are running from a checkout, replace `mcpsmith` with `cargo run --quiet --`.

```bash
tmpdir="$(mktemp -d)"
config="/path/to/mcp.json"

HOME="$tmpdir/home" mcpsmith discover --config "$config"

HOME="$tmpdir/home" mcpsmith playwright \
  --dry-run \
  --config "$config" \
  --skills-dir "$tmpdir/skills" \
  --backend codex
```

Start with `--dry-run`. It runs the full pipeline, writes the staged artifacts, and leaves your live MCP config and installed skills untouched.

## What mcpsmith does

- Converts the MCP you actually have installed, not just a registry listing.
- Resolves exact local, npm, PyPI, or repository-backed source before conversion.
- Uses runtime `tools/list` output plus source inspection to ground each tool.
- Prefers deterministic evidence extraction, with a narrow mapper fallback only for low-confidence tools.
- Produces inspectable staged artifacts that other agents and CI jobs can chain without prompts.
- Installs generated skills under `~/.agents/skills/` by default.

## How it works

`mcpsmith` treats MCP replacement as a grounding problem, not a prompt-writing problem. It first proves what server is installed and what source it comes from, then it builds skills from evidence it can cite.

```mermaid
flowchart LR
  A["Discover installed MCP"] --> B["Resolve exact artifact"]
  B --> C["Snapshot source"]
  C --> D["Extract tool evidence"]
  D --> E["Synthesize skills"]
  E --> F["Review with second pass"]
  F --> G["Verify grounding and references"]
  G --> H["Install skills and update config"]
```

1. Resolve the MCP to an exact local path, npm package, PyPI package, or repository revision.
2. Snapshot the source for that exact artifact.
3. Inspect runtime tools and map them to handlers, tests, and docs.
4. Synthesize skill drafts from that evidence bundle.
5. Review the drafts with a second agent pass.
6. Verify format, grounding, and references.
7. Optionally install the skills and remove the MCP config entry atomically.

Remote-only or source-unavailable servers are blocked instead of being converted from metadata alone.

## One-shot flow

Use one-shot when you want the full pipeline in a single command:

```bash
tmpdir="$(mktemp -d)"
config="/path/to/mcp.json"

HOME="$tmpdir/home" mcpsmith run playwright \
  --dry-run \
  --config "$config" \
  --skills-dir "$tmpdir/skills" \
  --backend codex
```

The positional shorthand is equivalent:

```bash
mcpsmith playwright --dry-run --config /tmp/mcp.json --skills-dir /tmp/skills --backend codex
```

Useful one-shot flags:

- `--json` for machine-readable output.
- `--backend codex|claude` to force a synthesis and review backend.
- `--backend-auto` to allow fallback when a preferred backend is unavailable.
- `--skills-dir <PATH>` to write preview or installed skills somewhere isolated.
- `--config <PATH>` to inspect one or more explicit MCP config files.

## Staged flow

Use the staged flow when you want inspectable artifacts between steps:

```bash
mcpsmith resolve playwright --json --config /tmp/mcp.json
mcpsmith snapshot --json --from-resolve .codex-runtime/stages/resolve-playwright.json
mcpsmith evidence --json --from-snapshot .codex-runtime/stages/snapshot-playwright.json
mcpsmith synthesize --json --from-evidence .codex-runtime/stages/evidence-playwright.json --backend codex
mcpsmith review --json --from-bundle .codex-runtime/stages/synthesize-playwright.json --backend codex
mcpsmith verify --json --from-bundle .codex-runtime/stages/review-playwright.json
```

Each stage also accepts a server name directly. Staged artifacts are written under `.codex-runtime/stages/`, and they can be passed between agents with `--from-resolve`, `--from-snapshot`, `--from-evidence`, and `--from-bundle`.

## CLI quick reference

Common entrypoints:

```bash
mcpsmith --help
mcpsmith discover --help
mcpsmith resolve --help
mcpsmith run --help
```

Common commands:

```bash
mcpsmith discover --json --config /tmp/mcp.json
mcpsmith playwright --dry-run --config /tmp/mcp.json --skills-dir /tmp/skills
mcpsmith run playwright --json --config /tmp/mcp.json --skills-dir /tmp/skills
mcpsmith resolve playwright --json --config /tmp/mcp.json
mcpsmith snapshot --json --from-resolve .codex-runtime/stages/resolve-playwright.json
mcpsmith evidence --json --from-snapshot .codex-runtime/stages/snapshot-playwright.json
mcpsmith synthesize --json --from-evidence .codex-runtime/stages/evidence-playwright.json --backend codex
mcpsmith review --json --from-bundle .codex-runtime/stages/synthesize-playwright.json --backend codex
mcpsmith verify --json --from-bundle .codex-runtime/stages/review-playwright.json
mcpsmith catalog sync
mcpsmith catalog stats
```

## Catalog and source resolution

`mcpsmith` has two source inputs:

- Local MCP config entries from discovered config files or explicit `--config` paths.
- Public catalog data for census work and limited resolution fallback.

Catalog sync defaults to the `official` and `smithery` providers.

Resolution order is deterministic:

1. Local path
2. npm package and version
3. PyPI package and version
4. Repository URL and revision
5. Cached catalog fallback only when direct identity is insufficient

That order is intentional. `mcpsmith` prefers exact source identity over registry prose, and it refuses to generate skills when the source cannot be grounded.

## Backend behavior

`mcpsmith` currently supports `codex` and `claude` for the synthesize and review stages.

Backend selection order is:

1. Explicit `--backend`
2. `backend.preference` from `~/.mcpsmith/config.yaml`
3. Auto-detect installed backends in `codex`, then `claude` order

Use `--backend-auto` when you want the CLI to fall back automatically.

Minimal config example:

```yaml
backend:
  preference: codex
```

Use `MCPSMITH_CODEX_COMMAND` and `MCPSMITH_CLAUDE_COMMAND` for tests or local command overrides.

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

Staged commands write JSON artifacts under `.codex-runtime/stages/`. One-shot runs also emit a run report under `.codex-runtime/runs/` with paths for `resolve`, `snapshot`, `evidence`, `synthesis`, `review`, and `verify`.

Evidence artifacts include:

- The chosen registration and handler snippets for each tool
- Confidence and diagnostics for deterministic matching
- Test and documentation citations when they exist
- Mapper fallback output only for low-confidence tools

`run` installs reviewed skills and removes the MCP config entry unless `--dry-run` is set. In `--dry-run` mode it still writes the full staged artifact set plus a preview skill tree.

## Examples

Sample fixtures live under [`examples/`](examples/):

- [`examples/sample-mcp-config.json`](examples/sample-mcp-config.json): minimal MCP config input
- [`examples/sample-review.json`](examples/sample-review.json): reviewed skill bundle artifact
- [`examples/sample-run-report.json`](examples/sample-run-report.json): one-shot run report
- [`examples/sample-skill-pack-tree.txt`](examples/sample-skill-pack-tree.txt): installed skill tree preview

## Release Automation

Pushing to `main` triggers the `Release` workflow, and the same workflow can be run manually with `workflow_dispatch`.

When the version in `Cargo.toml` has not been published yet, the workflow will:

- Build release artifacts
- Publish a GitHub release
- Publish `mcpsmith-core` and `mcpsmith` to crates.io
- Update `nclandrei/homebrew-tap` from the published `mcpsmith-<version>.crate` tarball

The workflow creates the `v<version>` tag automatically. Do not push release tags manually.

Required GitHub Actions secrets:

- `CARGO_REGISTRY_TOKEN`
- `HOMEBREW_TAP_TOKEN`

## Troubleshooting

- No servers resolved: pass `--config /path/to/mcp.json` and confirm the file contains an `mcpServers` object.
- Backend not found: install `codex` or `claude`, or pass `--backend` and `--backend-auto` explicitly.
- Artifact resolution blocked: inspect the `resolve` artifact to see whether the server is remote-only or missing exact source identity.
- Synthesis blocked: inspect the `evidence` artifact for missing handler, test, or documentation citations.
- Review rejected a skill: inspect the `review` artifact and rerun synthesis with a better backend or better source evidence.
- One-shot blocked: rerun with `--dry-run` first, inspect the staged artifacts, then retry without `--dry-run`.

## Isolated verification

Never verify against your real home directory. Keep mutable state isolated with a temporary `HOME`, an explicit `--config`, and an isolated `--skills-dir`.

Repo-local checks:

```bash
./scripts/local-checks.sh
```

Smoke helpers in this repo:

- `./scripts/smoke/smoke-test-installed-mcpsmith.sh`
- `./scripts/smoke/mock_fixture_flow.sh`
- `./scripts/smoke/live_public_mcp.sh --server memory`
- `./scripts/smoke/live_public_mcp.sh --server chrome-devtools`
- `./scripts/smoke/cli_verify_smoke.sh`

For manual verification, the baseline rule is simple: set an isolated `HOME`, point at an explicit config file, and write generated skills into an isolated directory.

## License

MIT
