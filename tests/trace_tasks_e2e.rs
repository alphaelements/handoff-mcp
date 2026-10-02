//! Real-binary E2E for `handoff_trace_tasks` / CLI `trace tasks`
//! (wiki/260-vmodel-m2-design.md §4.9/§5.3, t360.20.16/M2-16): preview vs
//! apply, idempotent skip, `require_estimate_hours` handling, `parent_id`,
//! `limit` truncation, `select.layers` filtering, and the CLI round trip.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config, TraceProfileConfig};
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

    fn call_raw(&mut self, name: &str, arguments: Value) -> (bool, String) {
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
        (is_error, text)
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call_raw(name, arguments);
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

/// REQ-001/REQ-002 (requirement, no task yet) in one document with
/// scope_paths `["src/auth/"]`.
fn build_project(server: &mut Server, dir: &std::path::Path) {
    let pd = dir.to_string_lossy().to_string();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-tasks-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-tasks-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "scope_paths": ["src/auth/"],
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\nAfter 5 failures the account locks.\n\n### REQ-002 Password reset\n\nA user can reset their password.\n",
        }),
    );
}

#[test]
fn preview_mode_never_writes_and_lists_both_items() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let before = snapshot(&handoff);
    let resp = server.call("handoff_trace_tasks", json!({ "project_dir": pd }));
    assert_eq!(resp["mode"], "preview", "{resp}");
    let planned = resp["planned"].as_array().unwrap();
    assert_eq!(planned.len(), 2, "{resp}");
    let items: Vec<&str> = planned
        .iter()
        .map(|p| p["item"].as_str().unwrap())
        .collect();
    assert!(items.contains(&"REQ-001"), "{resp}");
    assert!(items.contains(&"REQ-002"), "{resp}");
    for p in planned {
        assert_eq!(p["role"], "implements");
        assert!(p.get("task_id").is_none(), "{resp}");
    }
    assert!(resp["skipped"].as_array().unwrap().is_empty());

    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "preview mode must never write any byte under .handoff/"
    );
}

#[test]
fn apply_mode_creates_tasks_with_links_labels_scope_and_is_idempotent() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "mode": "apply", "estimate_hours": 2.0 }),
    );
    assert_eq!(resp["mode"], "apply", "{resp}");
    let created = resp["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{resp}");
    let task_id = created[0]["task_id"].as_str().unwrap().to_string();

    let task = server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": task_id }),
    );
    assert_eq!(task["title"], created[0]["title"]);
    assert_eq!(task["labels"], json!(["layer:requirement"]));
    assert_eq!(task["scope_paths"], json!(["src/auth/"]));

    // Second apply call: both items already have an `implements` task ->
    // skipped, nothing new created (idempotent).
    let second = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "mode": "apply", "estimate_hours": 2.0 }),
    );
    assert!(second["created"].as_array().unwrap().is_empty(), "{second}");
    assert_eq!(second["skipped"].as_array().unwrap().len(), 2, "{second}");
}

/// NFR-006 (wiki/270-vmodel-m3-design.md §2.7): the project default
/// profile's `max_generated_per_call = 2` produces a warning when
/// `mode="apply"` actually creates more tasks than that (3 requirement
/// items, no `limit` override) — real binary, real JSON-RPC, real
/// config.toml on disk.
#[test]
fn apply_warns_when_created_count_exceeds_profile_max_generated_per_call() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-tasks-nfr006-e2e" }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("test".to_string());
    config.trace.profiles.insert(
        "test".to_string(),
        TraceProfileConfig {
            extends: Some("standard".to_string()),
            max_generated_per_call: Some(2),
            ..Default::default()
        },
    );
    write_config(&config_path, &config).expect("write config");

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-tasks-nfr006-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n\
                ### REQ-002 B\n\nStatement.\n\n### REQ-003 C\n\nStatement.\n",
        }),
    );

    let resp = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "mode": "apply", "estimate_hours": 1.0 }),
    );
    let created = resp["created"].as_array().unwrap();
    assert_eq!(created.len(), 3, "{resp}");
    let warnings = resp["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|w| w
            .as_str()
            .unwrap_or("")
            .contains("Generated 3 items, exceeding profile limit of 2")),
        "expected a profile-limit warning, got: {warnings:?}"
    );
}

/// Same profile cap as above, but `limit=1` keeps the actually-created count
/// at/under it — no profile-limit warning.
#[test]
fn apply_does_not_warn_when_limit_keeps_created_count_within_profile_max() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-tasks-nfr006-ok-e2e" }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("test".to_string());
    config.trace.profiles.insert(
        "test".to_string(),
        TraceProfileConfig {
            extends: Some("standard".to_string()),
            max_generated_per_call: Some(2),
            ..Default::default()
        },
    );
    write_config(&config_path, &config).expect("write config");

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "requirements-tasks-nfr006-ok-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 A\n\nStatement.\n\n\
                ### REQ-002 B\n\nStatement.\n\n### REQ-003 C\n\nStatement.\n",
        }),
    );

    let resp = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "mode": "apply", "estimate_hours": 1.0, "limit": 1 }),
    );
    let created = resp["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "{resp}");
    let warnings = resp["warnings"].as_array().unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("exceeding profile limit")),
        "did not expect a profile-limit warning, got: {warnings:?}"
    );
}

#[test]
fn apply_requires_estimate_hours_when_the_project_requires_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "mode": "apply" }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("estimate_hours"), "{text}");
}

#[test]
fn parent_id_makes_generated_tasks_children() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "title": "Epic", "schedule": { "estimate_hours": 1.0 } },
        }),
    );

    let resp = server.call(
        "handoff_trace_tasks",
        json!({
            "project_dir": pd,
            "items": ["REQ-001"],
            "mode": "apply",
            "parent_id": "t1",
            "estimate_hours": 1.0,
        }),
    );
    let created = resp["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "{resp}");
    let task_id = created[0]["task_id"].as_str().unwrap();
    assert!(task_id.starts_with("t1."), "{resp}");
}

#[test]
fn limit_truncates_planned_entries_with_a_warning() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let resp = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "limit": 1 }),
    );
    assert_eq!(resp["planned"].as_array().unwrap().len(), 1, "{resp}");
    assert!(
        resp["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("truncated")),
        "{resp}"
    );
}

#[test]
fn select_layers_restricts_the_scan() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-tasks-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-001 Lockout check\n\n- verifies: REQ-001\n- method: manual\n\nCheck lockout.\n",
        }),
    );

    let resp = server.call(
        "handoff_trace_tasks",
        json!({ "project_dir": pd, "select": { "layers": ["acceptance"] } }),
    );
    let planned = resp["planned"].as_array().unwrap();
    assert_eq!(planned.len(), 1, "{resp}");
    assert_eq!(planned[0]["item"], "AT-001");
    assert_eq!(planned[0]["role"], "executes");
}

/// Reviewer feedback (round 1): a scalar `items` (e.g. `items: "REQ-001"`)
/// used to be silently collapsed to "no filter", so `mode="apply"` created a
/// task for every item in the project — reproduced here against the real
/// binary exactly as the reviewer did. It must now error, and must not
/// write any task.
#[test]
fn apply_with_scalar_items_errors_instead_of_mass_creating() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let before = snapshot(&handoff);
    let (is_error, text) = server.call_raw(
        "handoff_trace_tasks",
        json!({
            "project_dir": pd,
            "items": "REQ-001",
            "mode": "apply",
            "estimate_hours": 1.0,
        }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("items"), "{text}");

    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "a malformed 'items' must create nothing: {text}"
    );
}

#[test]
fn items_and_select_are_mutually_exclusive() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_tasks",
        json!({
            "project_dir": pd,
            "items": ["REQ-001"],
            "select": { "layers": ["requirement"] },
        }),
    );
    assert!(is_error, "{text}");
}

// ---------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------

#[test]
fn cli_trace_tasks_preview_then_apply_round_trip() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&["trace", "tasks", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("`trace tasks` must print JSON: {e}: {stdout}"));
    assert_eq!(parsed["mode"], "preview");
    assert_eq!(parsed["planned"].as_array().unwrap().len(), 2, "{parsed}");

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "tasks",
        "--project-dir",
        dir_str,
        "--items",
        "REQ-001",
        "--mode",
        "apply",
        "--estimate-hours",
        "1.5",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["mode"], "apply");
    let created = parsed["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "{parsed}");
    assert_eq!(created[0]["item"], "REQ-001");
}

#[test]
fn cli_trace_tasks_select_layers_flag_nests_into_select() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "tasks",
        "--project-dir",
        dir_str,
        "--layers",
        "requirement",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(parsed["planned"].as_array().unwrap().len(), 2, "{parsed}");
}

/// Reviewer feedback (round 1): `--gap-kinds`/`--dev-stage` had no CLI
/// nesting test at all. `--gap-kinds` must nest into `select.gap_kinds` and
/// actually filter the scan (not merely parse without error).
#[test]
fn cli_trace_tasks_gap_kinds_flag_nests_into_select_and_filters() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let pd = dir_str.to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "acceptance-gap-kinds-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "### AT-002 Password reset check\n\n- verifies: REQ-002\n- method: manual\n\nCheck reset.\n",
        }),
    );
    server.call(
        "handoff_trace_record",
        json!({ "project_dir": pd, "results": [{ "item": "AT-002", "result": "pass" }] }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "tasks",
        "--project-dir",
        dir_str,
        "--gap-kinds",
        "unverified",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items: Vec<&str> = parsed["planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["item"].as_str().unwrap())
        .collect();
    assert!(items.contains(&"REQ-001"), "{parsed}");
    assert!(!items.contains(&"REQ-002"), "{parsed}");
}

/// `--dev-stage` must nest into `select.dev_stage` and actually filter the
/// scan.
#[test]
fn cli_trace_tasks_dev_stage_flag_nests_into_select_and_filters() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let pd = dir_str.to_string();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": pd,
            "doc_id": "requirements-tasks-e2e",
            "action": "set_dev_stage",
            "sub_item_id": "REQ-001",
            "dev_stage": "in_progress",
        }),
    );
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "tasks",
        "--project-dir",
        dir_str,
        "--dev-stage",
        "in_progress",
    ]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let parsed: Value = serde_json::from_str(&stdout).unwrap();
    let items: Vec<&str> = parsed["planned"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["item"].as_str().unwrap())
        .collect();
    assert_eq!(items, vec!["REQ-001"], "{parsed}");
}

/// An unknown `--gap-kinds` value must error via the CLI round trip too (not
/// just the direct MCP call).
#[test]
fn cli_trace_tasks_unknown_gap_kind_errors() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "tasks",
        "--project-dir",
        dir_str,
        "--gap-kinds",
        "bogus",
    ]);
    assert_ne!(code, 0, "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("bogus"), "stdout={stdout} stderr={stderr}");
}

/// wiki/260 §4.9's own note: scanning is read-only (E6) — `mode="preview"`
/// (the CLI default) must never write any byte under `.handoff/`.
#[test]
fn cli_trace_tasks_preview_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let dir_str = dir.to_str().unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    build_project(&mut server, &dir);
    drop(server);

    let before = snapshot(&handoff);
    let (stdout, stderr, code) = run_cli(&["trace", "tasks", "--project-dir", dir_str]);
    assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "trace tasks preview must never write any byte under .handoff/: stdout={stdout} stderr={stderr}"
    );
}
