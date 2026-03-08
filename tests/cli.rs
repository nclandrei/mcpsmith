use assert_cmd::Command;
use predicates::prelude::*;
use std::path::Path;

fn mcpsmith_cmd(home: &Path) -> Command {
    let mut cmd = assert_cmd::cargo::cargo_bin_cmd!("mcpsmith");
    cmd.env("HOME", home);
    cmd
}

#[test]
fn test_mcpsmith_discover_build_contract_apply() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let dossier_path = dir.path().join("dossier.json");
    let report_path = dir.path().join("contract-report.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    let mock_codex = dir.path().join("mock-codex.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--json", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"server_gate\": \"ready\""));

    mcpsmith_cmd(dir.path())
        .args(["build", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success();

    mcpsmith_cmd(dir.path())
        .args(["verify", "playwright", "--json", "--config"])
        .arg(&config_path)
        .args(["--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));

    mcpsmith_cmd(dir.path())
        .args(["contract-test", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--report"])
        .arg(&report_path)
        .args(["--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));

    mcpsmith_cmd(dir.path())
        .args(["apply", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--yes", "--json", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .success()
        .stdout(predicate::str::contains("\"mcp_config_updated\": true"));

    let updated = std::fs::read_to_string(&config_path).unwrap();
    assert!(!updated.contains("playwright"));
    assert!(skills_dir.join("playwright").join("SKILL.md").exists());
    assert!(
        skills_dir
            .join("playwright--execute")
            .join("SKILL.md")
            .exists()
    );
    assert!(
        skills_dir
            .join("playwright")
            .join(".mcpsmith")
            .join("manifest.json")
            .exists()
    );
    assert!(!skills_dir.join("playwright.md").exists());
    assert!(!skills_dir.join("playwright--execute.md").exists());
    assert!(report_path.exists());

    let backup_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .count();
    assert_eq!(backup_count, 1);
}

#[test]
fn test_mcpsmith_apply_rolls_back_installed_skill_dirs_when_config_entry_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let dossier_path = dir.path().join("dossier.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    let mock_codex = dir.path().join("mock-codex.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_codex_script(&mock_codex);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    std::fs::write(&config_path, r#"{ "mcpServers": {} }"#).unwrap();

    mcpsmith_cmd(dir.path())
        .args(["apply", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--yes", "--skills-dir"])
        .arg(&skills_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(
            "Rolled back generated skills to keep conversion atomic.",
        ));

    assert!(!skills_dir.join("playwright").exists());
    assert!(!skills_dir.join("playwright--execute").exists());

    let backup_count = std::fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("settings.json.bak-")
        })
        .count();
    assert_eq!(backup_count, 0);
}

#[test]
fn test_mcpsmith_one_shot_works_with_claude_only() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let skills_dir = dir.path().join("skills");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    let mock_claude = dir.path().join("mock-claude.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);
    write_mock_claude_script(&mock_claude);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", dir.path().join("missing-codex"))
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
    assert!(!updated.contains("playwright"));
    assert!(skills_dir.join("playwright").join("SKILL.md").exists());
    assert!(
        skills_dir
            .join("playwright")
            .join(".mcpsmith")
            .join("manifest.json")
            .exists()
    );
}

#[test]
fn test_mcpsmith_discover_fails_cleanly_when_no_backend_installed() {
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let mock_mcp = dir.path().join("mock-mcp.sh");
    write_mock_mcp_script(&mock_mcp, &["execute"]);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", dir.path().join("missing-codex"))
        .env("MCPSMITH_CLAUDE_COMMAND", dir.path().join("missing-claude"))
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
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let dossier_path = dir.path().join("dossier.json");
    let mock_mcp = dir.path().join("mock-mcp-no-schema.sh");
    let mock_codex = dir.path().join("mock-codex.sh");
    write_mock_mcp_no_schema_script(&mock_mcp, "execute");
    write_mock_codex_script_for(&mock_codex, &["execute"]);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "playwright": {{
      "command": "{}",
      "description": "Read-only browser helpers",
      "readOnly": true
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "playwright", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    mcpsmith_cmd(dir.path())
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
    let dir = tempfile::tempdir().unwrap();
    let config_path = dir.path().join("settings.json");
    let dossier_path = dir.path().join("dossier.json");
    let mock_mcp = dir.path().join("mock-mcp-delete.sh");
    let mock_codex = dir.path().join("mock-codex.sh");
    write_mock_mcp_id_schema_script(&mock_mcp, "delete_item");
    write_mock_codex_script_for(&mock_codex, &["delete_item"]);

    std::fs::write(
        &config_path,
        format!(
            r#"{{
  "mcpServers": {{
    "admin": {{
      "command": "{}",
      "description": "Admin mutations"
    }}
  }}
}}"#,
            mock_mcp.display()
        ),
    )
    .unwrap();

    mcpsmith_cmd(dir.path())
        .env("MCPSMITH_CODEX_COMMAND", &mock_codex)
        .args(["discover", "admin", "--out"])
        .arg(&dossier_path)
        .args(["--config"])
        .arg(&config_path)
        .assert()
        .success();

    mcpsmith_cmd(dir.path())
        .args(["contract-test", "--from-dossier"])
        .arg(&dossier_path)
        .args(["--allow-side-effects", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("\"passed\": true"));
}

fn write_mock_mcp_script(path: &Path, tool_names: &[&str]) {
    let tools = tool_names
        .iter()
        .map(|name| {
            format!(
                r#"{{\"name\":\"{name}\",\"description\":\"Tool {name}\",\"inputSchema\":{{\"type\":\"object\",\"required\":[\"query\"],\"properties\":{{\"query\":{{\"type\":\"string\"}}}}}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{tools}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"query":"'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"invalid query"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

fn write_mock_codex_script(path: &Path) {
    write_mock_codex_script_for(path, &["execute"]);
}

fn write_mock_codex_script_for(path: &Path, tools: &[&str]) {
    let dossiers = tools
        .iter()
        .map(|name| {
            format!(
                r#"{{"name":"{name}","explanation":"Run {name}","recipe":["validate input","execute {name}","verify output"],"evidence":["runtime metadata"],"confidence":0.9,"contract_tests":[{{"probe":"happy-path","expected":"valid output","method":"run valid request","applicable":true}},{{"probe":"invalid-input","expected":"returns validation error","method":"run malformed request","applicable":true}},{{"probe":"side-effect-safety","expected":"requires confirmation for mutations","method":"run check/dry-run first","applicable":true}}]}}"#,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let payload = format!(r#"{{"tool_dossiers":[{dossiers}]}}"#);
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-codex"
  exit 0
fi
last_message_file=""
while [ $# -gt 0 ]; do
  case "$1" in
    --output-last-message|-o)
      last_message_file="$2"
      shift 2
      ;;
    *)
      shift
      ;;
  esac
done
cat > /dev/null
[ -n "$last_message_file" ] || exit 12
cat > "$last_message_file" <<'JSON'
{payload}
JSON
"#
    );
    write_agent_script(path, &body);
}

fn write_mock_claude_script(path: &Path) {
    write_mock_claude_script_for(path, &["execute"]);
}

fn write_mock_claude_script_for(path: &Path, tools: &[&str]) {
    let dossiers = tools
        .iter()
        .map(|name| {
            format!(
                r#"{{"name":"{name}","explanation":"Run {name}","recipe":["validate input","execute {name}","verify output"],"evidence":["runtime metadata"],"confidence":0.85,"contract_tests":[{{"probe":"happy-path","expected":"valid output","method":"run valid request","applicable":true}},{{"probe":"invalid-input","expected":"returns validation error","method":"run malformed request","applicable":true}},{{"probe":"side-effect-safety","expected":"requires confirmation for mutations","method":"run check/dry-run first","applicable":true}}]}}"#,
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let payload = format!(r#"{{"tool_dossiers":[{dossiers}]}}"#);
    let body = format!(
        r#"#!/bin/sh
if [ "$1" = "--version" ] || [ "$1" = "-v" ] || [ "$1" = "version" ]; then
  echo "mock-claude"
  exit 0
fi
cat > /dev/null
cat <<'JSON'
{payload}
JSON
"#
    );
    write_agent_script(path, &body);
}

fn write_mock_mcp_no_schema_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}"}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

fn write_mock_mcp_id_schema_script(path: &Path, tool_name: &str) {
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '{{"jsonrpc":"2.0","id":1,"result":{{"protocolVersion":"2025-03-26","capabilities":{{}}}}}}\n'
      ;;
    *'"method":"tools/list"'*)
      printf '{{"jsonrpc":"2.0","id":2,"result":{{"tools":[{{"name":"{tool_name}","description":"delete item","inputSchema":{{"type":"object","required":["id"],"properties":{{"id":{{"type":"string"}}}}}}}}]}}}}\n'
      ;;
    *'"method":"tools/call"'*)
      if echo "$line" | grep -q '"id":"'; then
        printf '{{"jsonrpc":"2.0","id":2,"result":{{"content":[{{"type":"text","text":"ok"}}],"isError":false}}}}\n'
      else
        printf '{{"jsonrpc":"2.0","id":2,"error":{{"code":-32602,"message":"missing id"}}}}\n'
      fi
      ;;
  esac
done
"#
    );
    write_agent_script(path, &body);
}

fn write_agent_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut perms = std::fs::metadata(path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(path, perms).unwrap();
    }
}
