# Release checklist

Use this checklist when cutting an independent `mcpsmith` release.

## Version bump

1. Update the version in [`Cargo.toml`](../Cargo.toml).
2. Update the version in [`crates/mcpsmith-core/Cargo.toml`](../crates/mcpsmith-core/Cargo.toml).
3. Keep the root dependency on `mcpsmith-core` versioned and in sync.

## Release notes

1. Summarize user-visible CLI, dossier, skill-output, or probe changes.
2. Call out any new MCP coverage, compatibility changes, or migration notes.
3. Link the relevant README/examples/docs updates when behavior changed.

## CI green

Confirm the `CI` workflow is green for the release commit:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`

## Live smoke green

Confirm the `Live Smoke` workflow is green for:

- `memory`
- `chrome-devtools`

If the release materially touches Apple-specific behavior, also run the optional
`xcodebuildmcp` smoke job from `workflow_dispatch`.

## Publish readiness

Before tagging, verify the workspace is package-ready:

1. Run `cargo package --allow-dirty --no-verify -p mcpsmith-core`.
2. Run `cargo package --allow-dirty --no-verify -p mcpsmith`.
3. Run `cargo publish --dry-run -p mcpsmith-core`.
4. Run `cargo publish --dry-run -p mcpsmith`.
5. Publish `mcpsmith-core` before `mcpsmith` if publishing to crates.io.

## Release cut

1. Tag the release commit.
2. Publish crates if needed.
3. Attach release notes.
4. Keep the README, examples, and plan status aligned with the shipped version.
