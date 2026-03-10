# mcpsmith architecture

`mcpsmith` is split into a thin CLI crate and a core library crate:

- `src/` owns CLI parsing, app-config loading, and command dispatch.
- `crates/mcpsmith-core/` owns inventory discovery, runtime introspection,
  backend orchestration, dossier generation, skill rendering, contract testing,
  verification, and atomic apply.

One-shot conversion composes the same discovery, build, contract-test, and
apply primitives that the stepwise CLI exposes directly.

## Config discovery

`mcpsmith` reads two types of configuration:

- App config from `~/.mcpsmith/config.yaml` for backend and probe defaults.
- MCP inventory config from well-known Claude, Codex, shared, and Amp paths,
  plus any paths passed with `--config`.

Inventory discovery parses JSON or TOML sources, extracts server entries, and
normalizes them into `MCPServerProfile` values with purpose, permission hints,
recommendation, and source-grounding metadata.

## Runtime introspection

Runtime introspection is the first source of truth for server capabilities.
`mcpsmith` starts the target MCP process, sends `initialize`, then sends
`tools/list`, and records normalized tool names, descriptions, and input
schemas.

That runtime data drives:

- dossier generation
- skill naming and manifest parity
- verify checks
- contract-test probe execution

If runtime introspection fails, replacement flows stop before any config
mutation.

## Backend selection

Backend selection is intentionally narrow and deterministic:

1. explicit `--backend`
2. app config `backend.preference`
3. auto-detect `codex`, then `claude`

The CLI can emit a backend health report with `--backend-health`. Local test and
development flows can override backend executables with
`MCPSMITH_CODEX_COMMAND` and `MCPSMITH_CLAUDE_COMMAND`.

Backends never replace runtime truth. They enrich runtime metadata into
tool-level dossiers, recipes, evidence, and contract-test expectations.

## Source grounding

Source grounding supplements runtime metadata with local or declared source
evidence when it is reachable.

Current grounding sources include:

- local executable or script entrypoints
- nearby `package.json`
- nearby `pyproject.toml`
- explicit homepage or repository metadata in MCP config
- package specs declared through `npx` or `uvx`

The resulting `source_grounding` data is attached to the discovered server and
fed into backend prompts so dossier evidence can distinguish
`runtime-only` from `source-inspected`.

## Build

`build` transforms a dossier bundle into the installed skill-pack layout under
`~/.agents/skills/` or an explicit `--skills-dir`.

For each server bundle it writes:

- one orchestrator skill directory: `<server-slug>/SKILL.md`
- one capability skill directory per runtime tool:
  `<server-slug>--<tool-slug>/SKILL.md`
- one hidden parity manifest:
  `<server-slug>/.mcpsmith/manifest.json`

The hidden manifest tracks required tools without leaking internal metadata into
agent-facing skill text.

## Contract-test

`contract-test` is the live runtime gate. It loads a dossier bundle, executes
real `tools/call` probes, and emits a structured report with per-tool probe
results.

Probe behavior is controlled by:

- `probe.timeout_seconds`
- `probe.retries`
- `probe.allow_side_effects`

Safe defaults matter. Side-effectful probes stay off unless explicitly allowed.

## Apply

`apply` is the mutation boundary. It performs three jobs:

1. build installed skills
2. rerun the live contract gate
3. back up and edit the MCP config only after the full pass succeeds

If config mutation fails after skills were written, `apply` rolls the generated
skill directories back so the conversion remains atomic. `verify` can then be
used later to confirm that the generated skills still match the live server's
runtime tool list.
