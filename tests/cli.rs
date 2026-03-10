mod support;

use predicates::prelude::*;
use std::path::Path;
use support::{
    TestContext, count_backups, write_mock_claude_script, write_mock_codex_script,
    write_mock_codex_script_for, write_mock_mcp_id_schema_script, write_mock_mcp_no_schema_script,
    write_mock_mcp_script,
};

fn write_playwright_config(ctx: &TestContext, command: &Path, read_only: Option<bool>) {
    ctx.write_server_config(
        "playwright",
        command,
        Some("Read-only browser helpers"),
        read_only,
    );
}

fn write_admin_config(ctx: &TestContext, command: &Path) {
    ctx.write_server_config("admin", command, Some("Admin mutations"), None);
}

#[test]
fn test_mcpsmith_root_help_hides_plan_and_uses_standalone_config_wording() {
    let ctx = TestContext::new();

    ctx.cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("\n  plan").not())
        .stdout(predicate::str::contains(
            "config backend.preference when available",
        ))
        .stdout(predicate::str::contains("config convert.backend_preference").not());
}

#[test]
fn test_mcpsmith_discover_help_scopes_backend_flags_without_probe_flags() {
    let ctx = TestContext::new();

    ctx.cmd()
        .args(["discover", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--backend <BACKEND>"))
        .stdout(predicate::str::contains("--backend-auto"))
        .stdout(predicate::str::contains("--backend-health"))
        .stdout(predicate::str::contains("--allow-side-effects").not())
        .stdout(predicate::str::contains("--probe-timeout-seconds").not())
        .stdout(predicate::str::contains("--probe-retries").not());
}

#[test]
fn test_mcpsmith_discover_build_contract_apply() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let dossier_path = ctx.dossier_path();
    let report_path = ctx.report_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--json", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"server_gate\": \"ready\""));

    ctx.cmd()
        .args(["build", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success();

    ctx.cmd()
        .args(["verify", "playwright", "--json", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));

    ctx.cmd()
        .args(["contract-test", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--report"])
        .arg(&report_path)
        .args(["--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));

    ctx.cmd()
        .args(["apply", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--yes", "--json", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mcp_config_updated\": true"));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    let orchestrator_body =
        std::fs::read_to_string(ctx.orchestrator_skill_path("playwright")).unwrap();
    let workflow_body =
        std::fs::read_to_string(ctx.tool_skill_path("playwright", "execute")).unwrap();
    assert!(!updated.contains("playwright"));
    assert!(ctx.orchestrator_skill_path("playwright").exists());
    assert!(ctx.tool_skill_path("playwright", "execute").exists());
    assert!(ctx.manifest_path("playwright").exists());
    assert!(!skills_dir.join("playwright.md").exists());
    assert!(!skills_dir.join("playwright--execute.md").exists());
    assert!(!orchestrator_body.contains("mcp__"));
    assert!(!workflow_body.contains("mcp__"));
    assert!(!workflow_body.contains("maps to"));
    assert!(workflow_body.contains("## Native Steps"));
    assert!(report_path.exists());
    assert_eq!(count_backups(&config_path), 1);
}

#[test]
fn test_mcpsmith_apply_rolls_back_installed_skill_dirs_when_config_entry_is_missing() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let dossier_path = ctx.dossier_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    std::fs::write(&config_path, r#"{ "mcpServers": {} }"#).unwrap();

    ctx.cmd()
        .args(["apply", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--yes", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Rolled back generated skills to keep conversion atomic.",
        ));

    assert!(!ctx.orchestrator_skill_path("playwright").exists());
    assert!(!ctx.tool_skill_path("playwright", "execute").exists());
    assert_eq!(count_backups(&config_path), 0);
}

#[test]
fn test_mcpsmith_build_rejects_blocked_dossier() {
    let ctx = TestContext::new();
    let dossier_path = ctx.dossier_path();
    let skills_dir = ctx.skills_dir();
    std::fs::write(
        &dossier_path,
        r#"{
  "format_version": 5,
  "generated_at": "2026-03-10T00:00:00Z",
  "dossiers": [
    {
      "generated_at": "2026-03-10T00:00:00Z",
      "format_version": 5,
      "server": {
        "id": "fixture:playwright",
        "name": "playwright",
        "source_label": "fixture",
        "source_path": "/tmp/mcp.json",
        "purpose": "Read-only browser helpers",
        "command": "npx",
        "args": ["-y", "@acme/playwright-mcp"],
        "url": null,
        "env_keys": [],
        "declared_tool_count": 1,
        "permission_hints": ["read-only"],
        "inferred_permission": "read-only",
        "recommendation": "replace-candidate",
        "recommendation_reason": "read-only",
        "source_grounding": {
          "kind": "npm-package",
          "evidence_level": "config-only",
          "inspected": false
        }
      },
      "runtime_tools": [
        {
          "name": "execute",
          "description": "Execute action",
          "input_schema": {
            "type": "object",
            "properties": {}
          }
        }
      ],
      "runtime_validations": [
        {
          "tool_name": "execute",
          "contract_tests": [
            {
              "probe": "happy-path",
              "expected": "Returns success.",
              "method": "Run with valid input.",
              "applicable": true
            },
            {
              "probe": "invalid-input",
              "expected": "Returns validation error.",
              "method": "Run with invalid input.",
              "applicable": true
            },
            {
              "probe": "side-effect-safety",
              "expected": "Skips destructive path.",
              "method": "Do not execute side effects.",
              "applicable": false
            }
          ],
          "probe_inputs": {},
          "probe_input_source": "synthesized"
        }
      ],
      "workflow_skills": [
        {
          "id": "execute",
          "title": "Execute workflow",
          "goal": "Run the execute workflow without relying on the MCP server.",
          "when_to_use": "Use this when you need to run the execute workflow with native commands.",
          "trigger_phrases": ["run execute"],
          "origin_tools": ["execute"],
          "prerequisite_workflows": [],
          "followup_workflows": [],
          "required_context": [
            {
              "name": "query",
              "guidance": "Collect the exact query before running the workflow.",
              "required": true
            }
          ],
          "context_acquisition": ["Ask for the missing query instead of guessing it."],
          "branching_rules": ["If the query is missing, stop before running commands."],
          "stop_and_ask": ["Stop if the query is ambiguous or could mutate state."],
          "native_steps": [
            {
              "title": "Run execute",
              "command": "printf '%s\\n' 'execute:$QUERY'",
              "details": "Replace $QUERY with the exact query value."
            }
          ],
          "verification": ["Confirm the command completed successfully."],
          "return_contract": ["Return the command output and the exact query used."],
          "guardrails": ["Do not invent query values."],
          "evidence": ["runtime metadata"],
          "confidence": 0.5
        }
      ],
      "server_gate": "blocked",
      "gate_reasons": ["Backend dossier generation failed; fallback-only draft output."],
      "backend_used": "codex",
      "backend_fallback_used": false,
      "backend_diagnostics": []
    }
  ]
}"#,
    )
    .unwrap();

    ctx.cmd()
        .args(["build", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Cannot build standalone skills from blocked dossier",
        ));
}

#[test]
fn test_mcpsmith_discover_records_source_grounding_in_dossier() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let dossier_path = ctx.dossier_path();
    let tool_root = ctx.path("local-tool");
    let bin_dir = tool_root.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let mock_mcp = bin_dir.join("mock-mcp.sh");
    let mock_codex = ctx.path("mock-codex.sh");
    write_mock_mcp_script(&mock_mcp, &["navigate"]);
    write_mock_codex_script_for(&mock_codex, &["navigate"]);
    std::fs::write(
        tool_root.join("package.json"),
        r#"{
  "name": "@acme/local-mcp",
  "version": "1.2.3",
  "homepage": "https://example.com/local-mcp",
  "repository": {
    "type": "git",
    "url": "https://github.com/acme/local-mcp"
  }
}"#,
    )
    .unwrap();

    write_playwright_config(&ctx, &mock_mcp, None);

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--json", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    let dossier: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&dossier_path).unwrap()).unwrap();
    let server = &dossier["dossiers"][0]["server"];
    assert_eq!(server["source_grounding"]["kind"], "local-path");
    assert_eq!(
        server["source_grounding"]["package_name"],
        "@acme/local-mcp"
    );
    assert_eq!(server["source_grounding"]["package_version"], "1.2.3");
    assert_eq!(
        server["source_grounding"]["repository_url"],
        "https://github.com/acme/local-mcp"
    );

    let evidence = dossier["dossiers"][0]["workflow_skills"][0]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| item.as_str())
        .collect::<Vec<_>>();
    assert!(evidence.contains(&"evidence-level: source-inspected"));
    assert!(evidence.contains(&"source-package: @acme/local-mcp@1.2.3"));
    assert!(evidence.contains(&"source-homepage: https://example.com/local-mcp"));
}

#[test]
fn test_mcpsmith_one_shot_works_with_claude_only() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let skills_dir = ctx.skills_dir();
    let mock_mcp = ctx.path("mock-mcp.sh");
    let mock_claude = ctx.path("mock-claude.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_claude_script(&mock_claude);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", ctx.path("missing-codex"))
        .env("MCPSMITH_CLAUDE_COMMAND", &mock_claude)
        .args(["playwright", "--json", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"backend_used\": \"claude\""))
        .stdout(predicate::str::contains("\"mcp_config_updated\": true"));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    let orchestrator_body =
        std::fs::read_to_string(ctx.orchestrator_skill_path("playwright")).unwrap();
    let workflow_body =
        std::fs::read_to_string(ctx.tool_skill_path("playwright", "execute")).unwrap();
    assert!(!updated.contains("playwright"));
    assert!(ctx.orchestrator_skill_path("playwright").exists());
    assert!(ctx.manifest_path("playwright").exists());
    assert!(!orchestrator_body.contains("mcp__"));
    assert!(!workflow_body.contains("mcp__"));
    assert!(workflow_body.contains("## Native Steps"));
}

#[test]
fn test_mcpsmith_discover_fails_cleanly_when_no_backend_installed() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let mock_mcp = ctx.path("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", ctx.path("missing-codex"))
        .env("MCPSMITH_CLAUDE_COMMAND", ctx.path("missing-claude"))
        .args(["discover", "playwright", "--config"])
        .arg(&config_path)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "No supported backend is installed",
        ));
}

#[test]
fn test_mcpsmith_contract_test_blocks_on_schema_gap() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let dossier_path = ctx.dossier_path();
    let mock_mcp = ctx.path("mock-mcp-no-schema.sh");
    let mock_codex = ctx.path("mock-codex.sh");
    write_mock_mcp_no_schema_script(&mock_mcp, "execute");
    write_mock_codex_script_for(&mock_codex, &["execute"]);
    write_playwright_config(&ctx, &mock_mcp, Some(true));

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    ctx.cmd()
        .args(["contract-test", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": false"))
        .stdout(predicate::str::contains("\"error_kind\": \"schema-gap\""));
}

#[test]
fn test_mcpsmith_contract_test_allows_side_effect_probe_with_flag() {
    let ctx = TestContext::new();
    let config_path = ctx.config_path();
    let dossier_path = ctx.dossier_path();
    let mock_mcp = ctx.path("mock-mcp-delete.sh");
    let mock_codex = ctx.path("mock-codex.sh");
    write_mock_mcp_id_schema_script(&mock_mcp, "delete_item");
    write_mock_codex_script_for(&mock_codex, &["delete_item"]);
    write_admin_config(&ctx, &mock_mcp);

    ctx.cmd()
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "admin", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    ctx.cmd()
        .args(["contract-test", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--allow-side-effects", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));
}
