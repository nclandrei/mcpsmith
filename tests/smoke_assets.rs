use mcpsmith_core::DossierBundle;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_live_fixture(path: &str) -> DossierBundle {
    let fixture_path = repo_root().join(path);
    let body = fs::read_to_string(&fixture_path).unwrap_or_else(|err| {
        panic!("failed to read {}: {err}", fixture_path.display());
    });
    let config_path = repo_root()
        .join("tests")
        .join("fixtures")
        .join("dummy-config.json");
    let hydrated = body.replace("__CONFIG_PATH__", &config_path.display().to_string());
    serde_json::from_str(&hydrated).unwrap_or_else(|err| {
        panic!("failed to parse {}: {err}", fixture_path.display());
    })
}

fn assert_live_fixture(path: &str, expected_tool: &str) {
    let bundle = load_live_fixture(path);
    assert_eq!(
        bundle.dossiers.len(),
        1,
        "{path} should contain one dossier"
    );

    let dossier = &bundle.dossiers[0];
    assert_eq!(
        dossier.tool_dossiers.len(),
        1,
        "{path} should contain one tool"
    );
    assert_eq!(dossier.tool_dossiers[0].name, expected_tool);
    assert!(
        dossier.tool_dossiers[0].probe_inputs.happy_path.is_some(),
        "{path} is missing explicit happy-path probe input"
    );
    assert!(
        Path::new(&dossier.server.source_path).ends_with("dummy-config.json"),
        "{path} should hydrate source_path placeholder"
    );
}

#[test]
fn live_smoke_fixtures_deserialize_with_explicit_happy_path_inputs() {
    assert_live_fixture(
        "tests/fixtures/live/memory-smoke.dossier.json",
        "read_graph",
    );
    assert_live_fixture(
        "tests/fixtures/live/chrome-devtools-smoke.dossier.json",
        "list_pages",
    );
    assert_live_fixture(
        "tests/fixtures/live/xcodebuildmcp-smoke.dossier.json",
        "session_show_defaults",
    );
}
