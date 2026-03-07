use std::fs;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_repo_file(path: &str) -> String {
    fs::read_to_string(repo_root().join(path)).unwrap_or_else(|err| {
        panic!("failed to read {path}: {err}");
    })
}

#[test]
fn repo_bootstrap_files_exist() {
    for path in [
        "AGENTS.md",
        "llms.txt",
        "Makefile",
        "scripts/local-checks.sh",
    ] {
        assert!(repo_root().join(path).is_file(), "missing {path}");
    }
}

#[test]
fn agents_guide_documents_repo_workflow() {
    let agents = read_repo_file("AGENTS.md");

    for needle in [
        "## What mcpsmith does",
        "## Command matrix",
        "## Isolated runtime rules",
        "## cli-verify workflow",
        "## jj expectations",
        "## Live-MCP verification expectations",
        "Add or update tests first for behavior changes.",
    ] {
        assert!(agents.contains(needle), "AGENTS.md missing {needle}");
    }
}

#[test]
fn llms_summary_documents_agent_entrypoints() {
    let llms = read_repo_file("llms.txt");

    for needle in [
        "## Preferred for AI agents (non-interactive)",
        "One-shot:",
        "Stepwise:",
        "Config path: `~/.mcpsmith/config.yaml`",
        "Installed skills path: `~/.agents/skills/`",
        "Retained diagnostics for the standalone surface: `list`, `inspect`, `verify`",
    ] {
        assert!(llms.contains(needle), "llms.txt missing {needle}");
    }
}

#[test]
fn makefile_and_gitignore_reference_local_checks_state() {
    let makefile = read_repo_file("Makefile");
    assert!(makefile.contains("local-checks:"));
    assert!(makefile.contains("local-checks-fix:"));

    let gitignore = read_repo_file(".gitignore");
    assert!(gitignore.contains(".codex-runtime/"));
}

#[cfg(unix)]
#[test]
fn local_checks_script_is_executable() {
    use std::os::unix::fs::PermissionsExt;

    let path = repo_root().join("scripts/local-checks.sh");
    let mode = fs::metadata(path).unwrap().permissions().mode();
    assert_ne!(mode & 0o111, 0, "scripts/local-checks.sh is not executable");
}

#[test]
fn local_checks_script_exposes_fix_mode() {
    let script = read_repo_file("scripts/local-checks.sh");

    assert!(script.contains("Usage: scripts/local-checks.sh [--fix]"));
    assert!(script.contains("cargo fmt --all --check"));
    assert!(script.contains("cargo clippy --all-targets -- -D warnings"));
    assert!(script.contains("cargo test"));
}
