use mcpsmith_core::ReviewReport;
use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn load_live_fixture(path: &str) -> ReviewReport {
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
    let report = load_live_fixture(path);
    assert!(report.approved, "{path} should be approved");
    assert_eq!(
        report.bundle.tool_conversions.len(),
        1,
        "{path} should contain one reviewed tool draft"
    );

    let draft = &report.bundle.tool_conversions[0];
    assert_eq!(draft.tool_name, expected_tool);
    assert!(
        draft
            .workflow_skill
            .origin_tools
            .iter()
            .any(|tool| tool == expected_tool),
        "{path} should wire the expected runtime tool into the workflow"
    );
    assert!(
        !draft.semantic_summary.citations.is_empty(),
        "{path} should include grounded citations"
    );
    assert!(
        Path::new(&report.bundle.evidence.server.source_path).ends_with("dummy-config.json"),
        "{path} should hydrate source_path placeholder"
    );
}

#[test]
fn live_smoke_fixtures_deserialize_with_explicit_happy_path_inputs() {
    assert_live_fixture("tests/fixtures/live/memory-smoke.review.json", "read_graph");
    assert_live_fixture(
        "tests/fixtures/live/chrome-devtools-smoke.review.json",
        "list_pages",
    );
    assert_live_fixture(
        "tests/fixtures/live/xcodebuildmcp-smoke.review.json",
        "screenshot",
    );
}
