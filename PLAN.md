# mcpsmith Master Plan

This plan is for `mcpsmith` only. `distill` is out of scope except as historical context for what was extracted. Do not add this plan to `distill`, do not reference `distill convert` as an active feature, and do not re-couple the two repos.

## Summary

`mcpsmith` is the standalone MCP-to-skill tool extracted from `distill`. It already supports:

```bash
mcpsmith <server>

mcpsmith discover <server> --out dossier.json
mcpsmith build --from-dossier dossier.json
mcpsmith contract-test --from-dossier dossier.json
mcpsmith apply --from-dossier dossier.json --yes
```

Current working behavior already includes:
- backend-agnostic discovery via `codex` or `claude`
- runtime `tools/list` introspection
- real runtime `tools/call` probes
- atomic apply
- output targeting `~/.agents/skills/`

What is still missing is standalone productization:
- better modular boundaries
- real source-grounded dossiers
- real installed-skill output format
- reusable live MCP test harness
- CI/release readiness

All work below must happen in `mcpsmith`.

## Parallel Workboard

Use one `jj` change per task family. Each task should be independently ownable by a separate agent whenever dependencies allow.

| ID | Status | Repo | Can Start | Depends On | Deliverable |
|---|---|---|---:|---|---|
| MS-00 | completed | `mcpsmith` | done | none | repo bootstrap files and agent instructions |
| MS-01 | pending | `mcpsmith` | now | none | standalone public CLI/config surface freeze |
| MS-02 | pending | `mcpsmith` | after MS-01 | MS-01 | internal module decomposition |
| MS-03 | pending | `mcpsmith` | after MS-02 starts | MS-01, MS-02 | source-grounded dossier pipeline |
| MS-04 | pending | `mcpsmith` | after MS-02 starts | MS-01, MS-02 | real installed skill-pack output |
| MS-05 | pending | `mcpsmith` | now for scaffold | MS-01 for final shape | reusable test harness and live MCP matrix |
| MS-06 | pending | `mcpsmith` | after MS-01 | MS-00, MS-01 | docs, examples, AGENTS, llms |
| MS-07 | pending | `mcpsmith` | after MS-00 | MS-00, MS-05, MS-06 | CI and release readiness |

## Global Working Rules

- Every task is implemented in `/Users/anicolae/code/mcpsmith`.
- Never modify `distill` as part of these tasks.
- Use latest `main` in `mcpsmith` before starting each task.
- Keep commits task-scoped and direct on `main` if that remains the repo policy.
- For every user-visible CLI or output change, run:
  - `cargo fmt --all`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
- For every user-visible CLI change, also verify with [$cli-verify](/Users/anicolae/code/dotfiles/config/skills/cli-verify/SKILL.md) using Ghostty + tmux.
- All MCP/config verification must use isolated state:
  - `HOME="$TMPDIR/..."`
  - `--config "$TMPDIR/mcp.json"`
  - `--skills-dir "$TMPDIR/skills"`
- Add `.codex-runtime/` to `mcpsmith/.gitignore` and use it for `cli-verify` session state.

## Required cli-verify Workflow For mcpsmith

Use this exact baseline in `mcpsmith`:

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

Minimum visual proofs required:
- `mcpsmith --help`
- `mcpsmith discover --help`
- one real error path
- one stepwise success flow
- one one-shot success flow

Every screenshot must be paired with pane capture output.

## Task Specs

### MS-00 [completed] Repo Bootstrap And Agent Context
Create the baseline operating files in `mcpsmith`:
- `/Users/anicolae/code/mcpsmith/AGENTS.md`
- `/Users/anicolae/code/mcpsmith/llms.txt`
- `/Users/anicolae/code/mcpsmith/PLAN.md`
- `/Users/anicolae/code/mcpsmith/Makefile` or `/Users/anicolae/code/mcpsmith/scripts/local-checks.sh`

Completed on 2026-03-07 with:
- `/Users/anicolae/code/mcpsmith/AGENTS.md`
- `/Users/anicolae/code/mcpsmith/llms.txt`
- `/Users/anicolae/code/mcpsmith/Makefile`
- `/Users/anicolae/code/mcpsmith/scripts/local-checks.sh`
- bootstrap coverage in `/Users/anicolae/code/mcpsmith/tests/repo_bootstrap.rs`

`AGENTS.md` must include:
- what `mcpsmith` does
- command matrix
- isolated `HOME` rules
- `cli-verify` workflow
- `jj` expectations
- live-MCP verification expectations

`llms.txt` must include:
- one-shot flow
- stepwise flow
- config path
- output path
- backend behavior
- probe behavior
- diagnostic commands retained

Add `.codex-runtime/` to `.gitignore`.

Done when:
- a new agent can open `mcpsmith` and work without needing `distill` context

### MS-01 [pending] Standalone Public Surface Freeze
Lock the v1 standalone interface:
- primary commands:
  - `mcpsmith <server>`
  - `discover`
  - `build`
  - `contract-test`
  - `apply`
- retained diagnostics:
  - `list`
  - `inspect`
  - `verify`
- remove public `plan`
- remove public `hybrid` / `replace-candidate` / `keep-mcp` product language from help and docs

Replace config shape with standalone naming:
- canonical config file stays `~/.mcpsmith/config.yaml`
- canonical keys become:
  - `backend.preference`
  - `backend.timeout_seconds`
  - `backend.chunk_size`
  - `probe.timeout_seconds`
  - `probe.retries`
  - `probe.allow_side_effects`

Compatibility rule:
- read old extracted key names only as temporary input compatibility
- do not document legacy keys

CLI cleanup:
- backend flags only on commands that use backends
- probe flags only on `contract-test`, `apply`, and one-shot
- `discover --help` must not expose probe flags

Done when:
- `mcpsmith --help` reads like a product, not an extracted subcommand tree

### MS-02 [pending] Internal Module Decomposition
Split `crates/mcpsmith-core` into explicit modules:
- `inventory`
- `runtime`
- `backend`
- `dossier`
- `source`
- `skillset`
- `contract`
- `apply`
- `diagnostics`

Rules:
- no business logic in CLI layer
- no single giant inherited file remains
- internal naming should stop implying “embedded convert subcommand”

Keep external behavior frozen to MS-01.

Done when:
- separate agents can own modules with minimal conflict risk

### MS-03 [pending] Source-Grounded Dossier Pipeline
Add real source grounding before or during dossier generation:
- resolve executable/package source from MCP config
- support:
  - local executable/path
  - `npx`/npm package
  - `uvx`/PyPI package
  - explicit URL/repository when present
- capture:
  - homepage
  - repo URL
  - package/version if known

When source is reachable:
- inspect real tool definitions/implementation sites
- feed runtime tools + source evidence into backend prompts
- record source-derived evidence in dossier

When source is not reachable:
- fallback to runtime metadata + runtime contract tests only
- mark evidence level clearly

Done when:
- dossier quality is driven by runtime truth plus source grounding where possible

### MS-04 [pending] Real Skill-Pack Output
Generate real installed skill directories under `~/.agents/skills/`:
- orchestrator:
  - `~/.agents/skills/<server-slug>/SKILL.md`
- per-tool capability skills:
  - `~/.agents/skills/<server-slug>--<tool-slug>/SKILL.md`

Rules:
- keep internal manifest, but store it in a non-user-facing file under the orchestrator skill directory
- skill text must stay clean:
  - no MCP server metadata section
  - no `mcp__...` hints
  - no internal mode/recommendation leakage

`verify` must check:
- orchestrator skill exists
- all tool skill directories exist
- manifest matches runtime coverage

`apply` must remain atomic:
- build skills
- rerun contract gate
- back up config
- remove MCP entry only after full pass

Done when:
- output is directly usable by agents as installed skills

### MS-05 [pending] Test Harness And Live MCP Matrix
Extract reusable mock MCP helpers into shared test support.

Add reusable smoke scripts for:
- mock MCP fixture flow
- live public MCP flow

Required live matrix:
- `memory=@modelcontextprotocol/server-memory`
- `chrome-devtools=chrome-devtools-mcp@latest`

Optional machine-specific smoke:
- `xcodebuildmcp@latest`

Rules:
- always isolated `HOME`
- temp config
- temp skills dir
- never touch real user config

Add one `cli-verify` smoke workflow that proves:
- help output
- a stepwise success path
- a one-shot success path

Done when:
- another agent can run deterministic mocks and at least two live MCP packages

### MS-06 [pending] Docs And Examples
Expand `/Users/anicolae/code/mcpsmith/README.md` into full standalone documentation:
- what `mcpsmith` does
- how it works
- one-shot flow
- stepwise flow
- config shape
- backend behavior
- runtime probe semantics
- output skill-pack layout
- troubleshooting
- isolated verification

Add examples:
- sample dossier JSON
- sample contract report JSON
- sample MCP config fixture
- sample generated skill-pack tree

Add one architecture doc describing:
- config discovery
- runtime introspection
- backend selection
- source grounding
- build
- contract-test
- apply

Done when:
- a new engineer can understand the product without reading extracted code history

### MS-07 [pending] CI And Release Readiness
Add CI for:
- `cargo fmt --all --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

Add separate manual or scheduled live-smoke workflow.

Complete package metadata in `Cargo.toml`:
- description
- repository
- license
- optional keywords/categories

Add release checklist:
- version bump
- release notes
- CI green
- live smoke green
- publish readiness

Done when:
- `mcpsmith` can be released independently from `distill`

## Recommended Parallel Order

### Wave 1
- MS-00
- MS-01
- MS-05 scaffold

### Wave 2
- MS-02
- MS-06 skeleton
- MS-05 live-smoke skeleton

### Wave 3
- MS-03
- MS-04
- MS-05 full live matrix

### Wave 4
- MS-06 final docs
- MS-07 CI/release

## Required Test Matrix

- Unit:
  - backend selector
  - config parsing and legacy-key compatibility
  - source resolvers
  - dossier merge logic
  - skill-pack rendering
  - probe synthesis/execution
- Integration:
  - codex-only
  - claude-only
  - auto fallback
  - no backend installed
  - schema-gap hard fail
  - unsafe destructive tool blocked
  - safe guarded write tool passes
- CLI:
  - top-level help
  - subcommand help
  - JSON output shape
  - one-shot success
  - one-shot blocked
  - `apply` requires `--yes`
  - `discover --help` does not show probe-only flags
- Live smoke:
  - memory
  - chrome-devtools
  - optional xcodebuild
- Visual with `cli-verify`:
  - help screen
  - error path
  - stepwise success
  - one-shot success

## Acceptance Criteria

- all planning and implementation lives in `mcpsmith`
- `distill` is not referenced as an active feature host
- `mcpsmith` has its own `AGENTS.md`, `llms.txt`, `PLAN.md`, and quality scripts
- public CLI is standalone and stable
- output is real installed skill directories
- runtime probes remain the final gate
- dossiers are source-grounded when possible
- live MCP verification is reusable and isolated
- CI and release flow are independent

## Assumptions And Defaults

- atomic per-server replacement remains the rule
- `codex` and `claude` remain interchangeable backends
- backend auto-detect order remains `codex` then `claude`
- output target remains `~/.agents/skills/`
- `list`, `inspect`, and `verify` stay as diagnostics for now
- `plan` is removed from the public standalone CLI
- `cli-verify` is mandatory for user-visible CLI verification
