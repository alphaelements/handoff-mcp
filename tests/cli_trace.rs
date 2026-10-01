//! CLI E2E tests for the `trace` command group (t360.13,
//! wiki/220-vmodel-integration-design.md §3.4): `handoff-mcp trace
//! report|record|slice|history` must run without a shell
//! (`std::process::Command::new` + `.args`, never a string passed to `sh -c`)
//! and print JSON to stdout — the contract handoff-vscode's VSCode-side
//! caller relies on (it spawns the native binary directly, wiki/220 §3.4:
//! "シェルなしで起動できる形"). Project setup uses the real binary's stdio
//! JSON-RPC transport (same harness as `tests/trace_report_slice_e2e.rs`)
//! since there is no `doc`/`update_task` CLI group yet; the `trace`
//! subcommands themselves are always exercised through the plain CLI.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

fn binary() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent")
        .parent()
        .expect("parent")
        .to_path_buf();
    path.push("handoff-mcp");
    path
}

/// Runs the CLI binary directly (no shell) and returns (stdout, stderr, exit
/// code).
fn run_cli(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary())
        .args(args)
        .output()
        .expect("failed to run binary");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
}

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        }
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");

        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no response for {name} within 10s"));
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        assert_eq!(resp["id"], id, "response id must match request id");
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Builds a small V-model project: `requirement` doc with REQ-001, an
/// `acceptance` doc with AT-001 verifying it, and task `t1` implementing
/// REQ-001.
fn build_project(server: &mut Server, dir: &std::path::Path) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "cli-trace-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-cli-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\nAfter 5 failures the account locks.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "acceptance-cli-e2e",
            "title": "Acceptance tests",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout after 5 failures\n\n- verifies: REQ-001\n- method: manual\n\nFail login 5 times, then confirm the account is locked.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement lockout", "requirement_ids": ["REQ-001"] },
        }),
    );
}

#[test]
fn cli_trace_report_writes_the_derived_file_and_prints_json_over_stdout() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&["trace", "report", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`trace report` must print JSON to stdout: {e}: {stdout}"));
    assert!(parsed.get("trace_layers").is_some(), "{parsed}");
    assert!(parsed.get("coverage").is_some(), "{parsed}");

    let trace_report_path = dir.join(".handoff/docs/_trace_report.json");
    assert!(
        trace_report_path.exists(),
        "CLI `trace report` must (re)generate _trace_report.json"
    );
    let persisted: Value =
        serde_json::from_str(&std::fs::read_to_string(&trace_report_path).unwrap()).unwrap();
    // M2-07 (wiki/260-vmodel-m2-design.md §5.1/§11 Q2): schema_version 2.
    assert_eq!(persisted["schema_version"], 2);
    assert!(persisted["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|it| it["id"] == "REQ-001"));
}

#[test]
fn cli_trace_report_include_items_flag_is_parsed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--include-items",
        "true",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items = parsed["items"]
        .as_array()
        .expect("include-items=true must add items[] to the CLI response");
    let req_001 = items
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 present");
    // t360.20.25 (wiki/260-vmodel-m2-design.md §5.1, M2-07): `items[].coverage`
    // (`TraceGraph::item_horizontal`/`item_vertical`) is wired into the
    // `handoff_trace_report`/CLI `trace report` response itself, not just
    // the persisted `_trace_report.json`.
    assert!(
        req_001["coverage"]["horizontal"].is_string(),
        "items[].coverage.horizontal must be present: {req_001}"
    );
}

#[test]
fn cli_trace_record_records_a_result_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "record",
        "--project-dir",
        dir_str,
        "--results",
        r#"[{"item":"AT-001","result":"pass"}]"#,
        "--task-id",
        "t1",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["recorded"], 1);
    assert!(!parsed["run_id"].as_str().unwrap().is_empty());

    let latest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(".handoff/runs/_latest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(latest["items"]["AT-001"]["result"], "pass");
}

#[test]
fn cli_trace_slice_returns_the_neighborhood_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--item",
        "REQ-001",
        "--direction",
        "down",
        "--max-items",
        "5",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let ids: Vec<String> = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids.contains(&"REQ-001".to_string()));
    assert!(ids.contains(&"AT-001".to_string()), "{ids:?}");

    // task_id-based slice + numeric --depth flag also parse correctly.
    let (stdout2, stderr2, code2) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--task-id",
        "t1",
        "--depth",
        "1",
    ]);
    assert_eq!(code2, 0, "stdout={stdout2} stderr={stderr2}");
    let parsed2: Value = serde_json::from_str(&stdout2).unwrap();
    let ids2: Vec<String> = parsed2["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|it| it["id"].as_str().unwrap().to_string())
        .collect();
    assert!(ids2.contains(&"REQ-001".to_string()), "{ids2:?}");
}

#[test]
fn cli_trace_history_lists_recorded_results_newest_first_without_a_shell() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": dir.to_string_lossy(), "results": [{"item": "AT-001", "result": "fail"}] }),
    );
    std::thread::sleep(Duration::from_millis(5));
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": dir.to_string_lossy(), "results": [{"item": "AT-001", "result": "pass"}] }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "history",
        "--project-dir",
        dir_str,
        "--item",
        "AT-001",
        "--limit",
        "10",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items = parsed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{items:?}");
    assert_eq!(items[0]["result"], "pass", "newest first: {items:?}");
    assert_eq!(items[1]["result"], "fail");
}

/// A single-value array flag (`--expand ID`, `--gap-kinds KIND`, `--layers
/// L` with no comma) must still reach the handler as a one-element array —
/// previously it arrived as a bare string and the handler's `as_array()`
/// read silently dropped it (reviewer finding, t360.13).
#[test]
fn cli_trace_single_value_array_flags_are_not_silently_dropped() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "slice",
        "--project-dir",
        dir_str,
        "--item",
        "REQ-001",
        "--expand",
        "REQ-001",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let req = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| it["id"] == "REQ-001")
        .expect("REQ-001 in slice");
    assert!(
        req.get("statement").is_some(),
        "--expand REQ-001 (single value) must expand REQ-001: {req}"
    );

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--layers",
        "requirement",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(
        parsed["trace_layers"]["in_use"],
        json!(["requirement"]),
        "--layers requirement (single value) must override the layer set: {parsed}"
    );
}

/// A `layers` override shapes only the calling request's response; it must
/// never be persisted into `_trace_report.json`, whose `inputs` fingerprint
/// does not record the override and would otherwise make an ad-hoc layer
/// view look like a fresh canonical report (reviewer finding, t360.13).
#[test]
fn cli_trace_report_with_layers_override_does_not_persist_the_override() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let path = dir.join(".handoff/docs/_trace_report.json");
    let (stdout, stderr, code) = run_cli(&["trace", "report", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let canonical = std::fs::read(&path).unwrap();

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "report",
        "--project-dir",
        dir_str,
        "--layers",
        "requirement",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["trace_layers"]["in_use"], json!(["requirement"]));

    assert_eq!(
        std::fs::read(&path).unwrap(),
        canonical,
        "a --layers override must leave _trace_report.json as the canonical view"
    );
}

#[test]
fn cli_trace_help_lists_all_four_actions() {
    let (stdout, _stderr, code) = run_cli(&["trace", "--help"]);
    assert_eq!(code, 0);
    for action in ["report", "record", "slice", "history"] {
        assert!(
            stdout.contains(action),
            "trace --help must list {action}: {stdout}"
        );
    }
}

/// Extracts the comma-separated tokens inside a description's trailing
/// `(a, b, c)`, if it has one — `None` for a description with no trailing
/// parenthesized list at all (e.g. `"Project metrics"`).
fn trailing_paren_list(desc: &str) -> Option<Vec<String>> {
    let desc = desc.trim_end();
    if !desc.ends_with(')') {
        return None;
    }
    let open = desc.rfind('(')?;
    let inner = &desc[open + 1..desc.len() - 1];
    let tokens: Vec<String> = inner
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens)
    }
}

/// t360.20.33 (M2-S9 tester/reviewer): the top-level `--help` summary for
/// each group enumerates that group's actions in a trailing parenthesized
/// list (e.g. `trace ... (report, record, slice, history, ingest, scaffold,
/// suspect, impact, lint, propose, matrix)`) — a hand-maintained list kept
/// separate from `print_group_help`'s own per-action table. These drifted
/// apart twice in a row for the `trace` group (`propose` silently missing
/// from the detailed `trace --help` table while still listed in the
/// top-level summary) without any test catching it, because no existing
/// test checked more than a handful of `trace`'s own action names. This
/// parses both listings out of the real binary's own `--help` output (no
/// shell) and asserts, for every group whose summary enumerates one, that
/// the two name exactly the same set of actions — removing `propose` or
/// `matrix` from `print_group_help`'s `"trace"` action table (the
/// regression this guards against) makes this fail.
#[test]
fn top_level_help_group_action_lists_match_print_group_help_exactly() {
    let (top_stdout, stderr, code) = run_cli(&["--help"]);
    assert_eq!(code, 0, "stdout={top_stdout} stderr={stderr}");

    // `print_cli_help` prints one `"    {name:<16}{desc}"` line per GROUPS
    // entry between `COMMANDS:` and `GLOBAL OPTIONS:`.
    let commands_block = top_stdout
        .split("COMMANDS:\n")
        .nth(1)
        .expect("--help must have a COMMANDS: section")
        .split("\nGLOBAL OPTIONS:")
        .next()
        .expect("COMMANDS: section must be followed by GLOBAL OPTIONS:");

    let mut groups_checked: Vec<String> = Vec::new();

    for line in commands_block.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let group = parts.next().unwrap();
        let desc = parts.next().unwrap_or("").trim_start();

        // Only groups whose summary enumerates actions in a trailing
        // `(a, b, c)` are in scope here — groups like `metrics` (a single
        // default action) don't list one at all.
        let Some(summary_actions) = trailing_paren_list(desc) else {
            continue;
        };
        groups_checked.push(group.to_string());

        let (group_stdout, group_stderr, group_code) = run_cli(&[group, "--help"]);
        assert_eq!(
            group_code, 0,
            "{group} --help must succeed: stdout={group_stdout} stderr={group_stderr}"
        );
        let actions_block = group_stdout
            .split("ACTIONS:\n")
            .nth(1)
            .unwrap_or_else(|| {
                panic!("{group} --help must have an ACTIONS: section: {group_stdout}")
            })
            .split("\nGLOBAL OPTIONS:")
            .next()
            .unwrap();

        let table_actions: std::collections::BTreeSet<String> = actions_block
            .lines()
            .filter_map(|l| {
                let t = l.trim_start();
                let name = t.split_whitespace().next()?;
                if name == "(default)" {
                    return None;
                }
                Some(name.to_string())
            })
            .collect();

        let summary_set: std::collections::BTreeSet<String> = summary_actions.into_iter().collect();

        assert_eq!(
            summary_set, table_actions,
            "`{group}`'s top-level --help summary action list must match its own \
             `--help` action table exactly: summary={summary_set:?} table={table_actions:?}"
        );
    }

    assert!(
        groups_checked.contains(&"trace".to_string()),
        "fixture precondition: the trace group's summary must enumerate its actions: \
         {top_stdout}"
    );
    assert!(
        groups_checked.len() > 1,
        "fixture precondition: more than one group's summary must enumerate actions \
         (e.g. task, timer, trace): checked={groups_checked:?}"
    );
}

/// M2-08 (wiki/260-vmodel-m2-design.md §4.3/§5.3): `trace lint` exit code 0 —
/// a fully-covered project (REQ-001 verified by AT-001, both with a linked
/// task) has no `fail_on=error` (default) finding at all.
#[test]
fn cli_trace_lint_exit_code_0_on_a_clean_project() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&["trace", "lint", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("trace lint must print JSON");
    assert_eq!(parsed["exit_code"], 0);
    assert_eq!(parsed["counts"]["error"], 0, "{parsed}");
}

/// Exit code 1: a dangling `refines` reference is an `error`-severity
/// finding by default, so `fail_on=error` (the CLI default) makes `trace
/// lint` exit 1.
#[test]
fn cli_trace_lint_exit_code_1_on_a_dangling_reference() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "spec-lint-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout\n\n- refines: REQ-999\n\nBody.\n",
        }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&["trace", "lint", "--project-dir", dir_str]);
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("trace lint must print JSON");
    assert_eq!(parsed["exit_code"], 1);
    assert!(
        parsed["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "dangling" && f["item"] == "SPEC-001"),
        "{parsed}"
    );
}

/// `--limit` only bounds the response size: the exit code must still reflect
/// every matching finding (same as `counts`), so `--limit 0` on a project
/// with an error-severity finding exits 1, not 0.
#[test]
fn cli_trace_lint_exit_code_ignores_the_limit_truncation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-limit-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "spec-lint-limit-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-001 Lockout\n\n- refines: REQ-999\n\nBody.\n",
        }),
    );
    drop(server);

    let (stdout, stderr, code) =
        run_cli(&["trace", "lint", "--project-dir", dir_str, "--limit", "0"]);
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("trace lint must print JSON");
    assert_eq!(parsed["exit_code"], 1, "{parsed}");
    assert_eq!(parsed["counts"]["error"], 1, "{parsed}");
    assert!(
        parsed["findings"].as_array().unwrap().is_empty(),
        "{parsed}"
    );
}

/// `--fail-on warning` makes a warning-severity-only project (an unverified
/// left-side item, no errors) also exit 1, not just 0.
#[test]
fn cli_trace_lint_fail_on_warning_catches_a_warning_only_project() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-warn-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "req-lint-warn-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-500 Needs a verifier\n\nBody.\n",
        }),
    );
    // Nudge `acceptance` into use (so REQ-500's horizontal axis reads
    // `uncovered`/`unverified` rather than `na` for lack of any
    // verification-layer document at all).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "at-lint-warn-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-900 Unrelated\n\n- verifies: REQ-999\n\nBody.\n",
        }),
    );
    drop(server);

    let (stdout_default, _stderr, code_default) =
        run_cli(&["trace", "lint", "--project-dir", dir_str]);
    assert_eq!(
        code_default, 1,
        "the dangling AT-900 reference is itself an error, so even the default fail_on=error \
         must exit 1: {stdout_default}"
    );

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "lint",
        "--project-dir",
        dir_str,
        "--rules",
        "unverified",
        "--fail-on",
        "warning",
    ]);
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).expect("trace lint must print JSON");
    assert_eq!(parsed["counts"]["error"], 0, "{parsed}");
    assert!(
        parsed["counts"]["warning"].as_i64().unwrap() > 0,
        "{parsed}"
    );
}

/// Exit code 2: an invalid `--fail-on` value is a usage/config error, not the
/// generic exit-1 every other CLI action uses for an `Err`.
#[test]
fn cli_trace_lint_exit_code_2_on_an_invalid_fail_on_value() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-usage-e2e" }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "lint",
        "--project-dir",
        dir_str,
        "--fail-on",
        "not-a-severity",
    ]);
    assert_eq!(code, 2, "stdout={stdout} stderr={stderr}");
    // t360.20.32 (M2-S8 reviewer): exit 2 alone doesn't prove *this* was the
    // cause — assert on the actual error cause so a regression that makes
    // `trace lint` exit 2 for an unrelated reason (e.g. a config.toml it
    // can no longer read) doesn't pass this test by accident.
    assert!(
        stdout.contains("fail_on") && stdout.contains("not-a-severity"),
        "error must name the offending fail_on value: {stdout}"
    );
}

/// Exit code 2 (M2-08 rework, reviewer round 1 MAJOR finding): a
/// `config.toml` that exists but fails to parse (here, `priority` written as
/// a bare string instead of the array `[[trace.lint.require]].when.priority`
/// requires) must make `trace lint` bail with exit 2, not silently fall back
/// to defaults and exit 0 with an empty `warnings: []` — a config this broken
/// means the project's whole `[[trace.lint.require]]` policy silently
/// vanished, which a CI gate must never read as "clean".
#[test]
fn cli_trace_lint_exit_code_2_on_a_malformed_config_toml() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-malformed-config-e2e" }),
    );
    drop(server);

    let config_path = dir.join(".handoff").join("config.toml");
    let mut content = std::fs::read_to_string(&config_path).unwrap();
    content.push_str(
        "\n[[trace.lint.require]]\nid = \"p0-needs-verification\"\n\
         need = \"verified_by\"\n\n[trace.lint.require.when]\npriority = \"P0\"\n",
    );
    std::fs::write(&config_path, content).unwrap();

    let (stdout, stderr, code) = run_cli(&["trace", "lint", "--project-dir", dir_str]);
    assert_eq!(code, 2, "stdout={stdout} stderr={stderr}");
    // t360.20.32: confirm this is actually a config-parse failure, not some
    // other exit-2 cause (e.g. an invalid `--fail-on`/`--rules`/`--format`
    // this invocation never even passed).
    assert!(
        stdout.contains("Failed to parse config"),
        "error must name the config.toml parse failure: {stdout}"
    );
}

/// Exit code 2: a `[[trace.lint.require]]` entry with a `need` that doesn't
/// name one of the recognized policy checks must be rejected outright, not
/// silently kept at its default severity/ignored (M2-08 rework, reviewer
/// round 1 MAJOR finding).
#[test]
fn cli_trace_lint_exit_code_2_on_an_invalid_require_rule_entry() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-invalid-require-e2e" }),
    );
    drop(server);

    let config_path = dir.join(".handoff").join("config.toml");
    let mut content = std::fs::read_to_string(&config_path).unwrap();
    content.push_str(
        "\n[[trace.lint.require]]\nid = \"p0-needs-verification\"\n\
         need = \"verified\"\n", // typo of "verified_by"
    );
    std::fs::write(&config_path, content).unwrap();

    let (stdout, stderr, code) = run_cli(&["trace", "lint", "--project-dir", dir_str]);
    assert_eq!(code, 2, "stdout={stdout} stderr={stderr}");
    // t360.20.32: confirm the cause is the invalid `need`, not an unrelated
    // config-parse failure (both would otherwise satisfy a bare `code == 2`).
    assert!(
        stdout.contains("unknown `need`") && stdout.contains("p0-needs-verification"),
        "error must name the offending `need` value and rule id: {stdout}"
    );
}

/// Exit code 2: an unknown `--rules` id must be rejected, not silently
/// filter out every finding and read as a clean (exit 0) run (M2-08 rework,
/// reviewer round 1 MAJOR finding (d), previously mis-rated a NIT).
#[test]
fn cli_trace_lint_exit_code_2_on_an_unknown_rules_id() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-unknown-rules-id-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "spec-lint-unknown-rules-id-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-910 Lockout\n\n- refines: REQ-999\n\nBody.\n",
        }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "lint",
        "--project-dir",
        dir_str,
        "--rules",
        "unverfied",
    ]);
    assert_eq!(code, 2, "stdout={stdout} stderr={stderr}");
    // t360.20.32: confirm the cause is the unknown rule id itself, not some
    // other exit-2 path (e.g. a config.toml parse failure).
    assert!(
        stdout.contains("unknown rule id") && stdout.contains("unverfied"),
        "error must name the offending rule id: {stdout}"
    );
}

/// Exit code 2 (t360.20.32, M2-S8 reviewer): `--rules ""` must be rejected
/// outright, not silently treated the same as omitting `--rules` entirely.
/// The CLI's comma-split flag parser (`cli.rs::parse_value`, `ARRAY_FIELDS`)
/// strips an all-empty-string value down to `rules: []` before it ever
/// reaches the handler, so the handler itself must reject an empty `rules`
/// array rather than reading "no ids survived" the same as "no filter was
/// requested at all" — otherwise a typo'd empty `--rules` value would
/// silently run with every rule enabled instead of failing loudly.
#[test]
fn cli_trace_lint_exit_code_2_on_an_empty_rules_value() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-empty-rules-e2e" }),
    );
    drop(server);

    let (stdout, stderr, code) =
        run_cli(&["trace", "lint", "--project-dir", dir_str, "--rules", ""]);
    assert_eq!(code, 2, "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("rules") && stdout.contains("empty"),
        "error must explain the empty `rules` value: {stdout}"
    );
}

/// `--format text` renders a human-readable line per finding (CLI-oriented,
/// §4.3) instead of printing the raw JSON.
#[test]
fn cli_trace_lint_format_text_prints_rendered_lines_not_raw_json() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir_str, "project_name": "cli-trace-lint-text-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir_str,
            "slug": "spec-lint-text-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-900 Lockout\n\n- refines: REQ-999\n\nBody.\n",
        }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "lint",
        "--project-dir",
        dir_str,
        "--format",
        "text",
    ]);
    assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
    assert!(
        serde_json::from_str::<Value>(&stdout).is_err(),
        "format=text must not print raw JSON: {stdout}"
    );
    assert!(
        stdout.contains("error[dangling]") && stdout.contains("SPEC-900"),
        "{stdout}"
    );
}

/// M2-08 (wiki/260 §4.3's E6 contract): `trace lint` must never write any
/// byte under `.handoff/`.
#[test]
fn cli_trace_lint_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    // One real recorded run, so `runs/_latest.json` exists and the run file
    // copied below is a genuinely "externally added" one it predates.
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir_str,
            "results": [{ "item": "AT-001", "result": "pass" }],
        }),
    );
    drop(server);

    fn snapshot(handoff: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, out);
                    } else {
                        out.push(path);
                    }
                }
            }
        }
        let mut paths = Vec::new();
        walk(handoff, &mut paths);
        paths.sort();
        paths
            .into_iter()
            .map(|p| {
                let bytes = std::fs::read(&p).unwrap();
                (p, bytes)
            })
            .collect()
    }

    // Directly edit the requirement body (bypassing doc_save/sync) so the
    // in-memory resync has real work to do, not just a no-op pass.
    handoff_mcp::storage::docs::write_doc_body(
        &handoff,
        "requirements-cli-e2e",
        "# Requirements\n\n### REQ-001 Account lockout\n\n- priority: P0\n\n\
         Directly edited text.\n",
    )
    .expect("write_doc_body");

    // wiki/260 §12 M2-08's done criterion names all three E6 conditions:
    // besides the direct body edit above, (2) a run file added from outside
    // (e.g. `git pull`) that `runs/_latest.json` predates, and (3) a stored
    // `SubItem.task_ids` that disagrees with the task side.
    let runs_dir = handoff.join("runs");
    let recorded = std::fs::read_dir(&runs_dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()) != Some("_latest.json"))
        .expect("trace_record must have written one run file");
    let run: Value = serde_json::from_str(&std::fs::read_to_string(&recorded).unwrap()).unwrap();
    let external_id = "20991231-000000-000-000001";
    let external = std::fs::read_to_string(&recorded)
        .unwrap()
        .replace(run["run_id"].as_str().unwrap(), external_id)
        .replace("\"pass\"", "\"fail\"");
    std::fs::write(runs_dir.join(format!("{external_id}.json")), external).unwrap();

    let req_doc_path = handoff.join("docs/_doc.requirements-cli-e2e.md");
    let req_doc = std::fs::read_to_string(&req_doc_path).unwrap();
    assert!(
        req_doc.contains("task_ids:\n      - t1\n"),
        "fixture precondition: REQ-001's SubItem.task_ids lists t1: {req_doc}"
    );
    std::fs::write(
        &req_doc_path,
        req_doc.replacen("task_ids:\n      - t1\n", "task_ids:\n      - t9\n", 1),
    )
    .unwrap();

    let before = snapshot(&handoff);
    let (stdout, stderr, _code) = run_cli(&["trace", "lint", "--project-dir", dir_str]);
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "trace lint must never write any byte under .handoff/: stdout={stdout} stderr={stderr}"
    );
    // The drift conditions were real (not a no-op pass): lint saw them.
    let parsed: Value = serde_json::from_str(&stdout).expect("trace lint must print JSON");
    let rules: Vec<&str> = parsed["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["rule"].as_str())
        .collect();
    assert!(rules.contains(&"unsynced_body"), "{parsed}");
    assert!(rules.contains(&"task_ids_drift"), "{parsed}");
}
