# mcpsmith

`mcpsmith` extracts MCP server behavior into standalone skill packs.

Core flow:

```bash
mcpsmith discover <server> --out dossier.json
mcpsmith build --from-dossier dossier.json
mcpsmith contract-test --from-dossier dossier.json
mcpsmith apply --from-dossier dossier.json --yes
```

One-shot flow:

```bash
mcpsmith <server>
```

Backend selection is backend-agnostic:

- `--backend codex|claude`
- `--backend-auto`
- `--backend-health`

Runtime probe controls:

- `--allow-side-effects`
- `--probe-timeout-seconds <N>`
- `--probe-retries <N>`

Config path:

- `~/.mcpsmith/config.yaml`

Generated skills default to:

- `~/.agents/skills/`

Optional backend command overrides for tests and local development:

- `MCPSMITH_CODEX_COMMAND`
- `MCPSMITH_CLAUDE_COMMAND`
