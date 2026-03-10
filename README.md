# mcpsmith

`mcpsmith` converts MCP servers into standalone skill packs using live runtime
truth instead of static assumptions. The CLI discovers MCP servers from config
files, introspects real `tools/list` output, optionally grounds dossiers with
local source metadata, contract-tests real `tools/call` behavior, and only then
applies an atomic replacement that writes installed skills and removes the MCP
config entry.

This repo is the standalone product. `distill` is historical context only.

## What mcpsmith does

- Turns one MCP server into one orchestrator skill plus one capability skill per
  runtime tool.
- Uses live MCP introspection and probes as the final gate before config
  mutation.
- Supports one-shot replacement or a stepwise dossier-driven workflow.
- Keeps generated output in the installed skill-pack layout under
  `~/.agents/skills/`.
- Preserves source evidence when available so dossiers are not runtime-only
  guesses.

## How it works

1. Discover MCP servers from known config locations plus any `--config` paths
   you pass explicitly.
2. Introspect the selected server with real `initialize` and `tools/list`
   requests.
3. Resolve backend selection and ask `codex` or `claude` to turn runtime
   metadata plus source evidence into tool dossiers.
4. Build installed skills from that dossier bundle.
5. Run `contract-test` against the live server with real `tools/call` probes.
6. Apply atomically: rebuild, rerun the contract gate, write skills, back up the
   MCP config, and only then remove the server entry.

`mcpsmith <server>` performs the full flow in one command. The stepwise commands
let you stop after discovery, inspect the dossier JSON, or rerun contract tests
before apply.

## One-shot flow

Use one-shot when you want the full conversion in a single isolated run:

```bash
tmpdir="$(mktemp -d)"
HOME="$tmpdir/home" cargo run --quiet -- \
  playwright \
  --config "$tmpdir/mcp.json" \
  --skills-dir "$tmpdir/skills"
```

Useful one-shot flags:

- `--backend codex|claude` to force a backend.
- `--backend-auto` to allow fallback when a preferred backend is unavailable.
- `--backend-health` to print backend diagnostics.
- `--allow-side-effects`, `--probe-timeout-seconds <N>`, and
  `--probe-retries <N>` to tune runtime probes.

## Stepwise flow

Use the stepwise flow when you want inspectable artifacts between phases:

```bash
tmpdir="$(mktemp -d)"
HOME="$tmpdir/home" cargo run --quiet -- \
  discover playwright \
  --out "$tmpdir/dossier.json" \
  --config "$tmpdir/mcp.json"

HOME="$tmpdir/home" cargo run --quiet -- \
  build --from-dossier "$tmpdir/dossier.json" \
  --skills-dir "$tmpdir/skills"

HOME="$tmpdir/home" cargo run --quiet -- \
  verify playwright \
  --config "$tmpdir/mcp.json" \
  --skills-dir "$tmpdir/skills"

HOME="$tmpdir/home" cargo run --quiet -- \
  contract-test --from-dossier "$tmpdir/dossier.json" \
  --report "$tmpdir/contract-report.json"

HOME="$tmpdir/home" cargo run --quiet -- \
  apply --from-dossier "$tmpdir/dossier.json" \
  --yes \
  --skills-dir "$tmpdir/skills"
```

Useful diagnostics that do not mutate state:

- `cargo run --quiet -- list --config "$tmpdir/mcp.json"`
- `cargo run --quiet -- inspect playwright --config "$tmpdir/mcp.json"`
- `cargo run --quiet -- verify playwright --config "$tmpdir/mcp.json" --skills-dir "$tmpdir/skills"`

## Config shape

`mcpsmith` has two distinct config inputs:

- App config at `~/.mcpsmith/config.yaml` for backend and probe defaults.
- MCP config sources discovered automatically or passed with `--config` for
  server inventory.

Canonical app config shape:

```yaml
backend:
  preference: auto
  timeout_seconds: 90
  chunk_size: 8

probe:
  timeout_seconds: 30
  retries: 0
  allow_side_effects: false
```

Legacy `convert.*` keys are still accepted as input-only compatibility, but new
docs and configs should use `backend.*` and `probe.*`.

## Backend behavior

Backend selection order is:

1. Explicit `--backend`.
2. `backend.preference` from `~/.mcpsmith/config.yaml`.
3. Auto-detect installed backends in `codex`, then `claude` order.

Use `--backend-health` when you need the CLI to explain why a backend was or
was not selected. For local development and tests, you can override backend
commands with `MCPSMITH_CODEX_COMMAND` and `MCPSMITH_CLAUDE_COMMAND`.

## Runtime probe semantics

`discover` gathers runtime tool metadata, but it does not prove live behavior on
its own. Runtime proof happens during `contract-test` and `apply`.

- `contract-test` executes real `tools/call` probes from the dossier bundle.
- Happy-path and invalid-input probes run against the live server when
  applicable.
- Side-effect probes stay disabled unless you opt in with
  `--allow-side-effects` or `probe.allow_side_effects: true`.
- `--probe-timeout-seconds` and `--probe-retries` control probe execution.
- `apply` reruns the contract gate before writing skills or editing MCP config.

## Output skill-pack layout

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

The orchestrator skill stays clean for agents. Internal parity data lives in
the hidden `.mcpsmith/manifest.json` file under the orchestrator directory.

## Examples

Sample artifacts live under [`examples/`](examples):

- [`examples/sample-mcp-config.json`](examples/sample-mcp-config.json): minimal
  MCP config fixture used for isolated runs.
- [`examples/sample-dossier.json`](examples/sample-dossier.json): discovery
  output with one server dossier and one tool dossier.
- [`examples/sample-contract-report.json`](examples/sample-contract-report.json):
  contract-test report showing executed and skipped probes.
- [`examples/sample-skill-pack-tree.txt`](examples/sample-skill-pack-tree.txt):
  the installed skill-pack directory shape.
- [`docs/architecture.md`](docs/architecture.md): high-level system design.

## Troubleshooting

- No servers discovered: pass `--config "$TMPDIR/mcp.json"` and confirm the file
  contains an `mcpServers` object.
- Ambiguous server name: rerun `list` and use the full `source:name` id.
- Backend not available: run with `--backend-health` and check `codex` or
  `claude` installation or the `MCPSMITH_*_COMMAND` overrides.
- Probe failures: inspect the dossier, contract report, and the target server's
  runtime expectations before retrying with different probe inputs.
- Apply blocked: `apply` requires `--yes`, and destructive probes remain off by
  default unless you explicitly allow them.

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

For every live or visual verification flow, set an isolated `HOME`, pass an
explicit `--config`, and write skills into an isolated `--skills-dir`.
