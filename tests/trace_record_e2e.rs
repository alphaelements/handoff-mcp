//! Real-binary E2E test for M1 execution records (t360.8,
//! wiki/220-vmodel-integration-design.md §2.6/§3.1): spawns the actual
//! `handoff-mcp` binary and drives `handoff_trace_record` over real stdio
//! JSON-RPC (same harness style as `tests/task_link_role_e2e.rs`), then
//! reads `.handoff/runs/<run_id>.json` and `.handoff/runs/_latest.json`
//! directly off disk to verify their actual on-disk contents — not just the
//! tool's JSON response.

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

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        Self::spawn_inner(None)
    }

    /// Same as [`Server::spawn`], but sets `HANDOFF_MCP_DERIVED_WRITE_LOG` so
    /// derived-file writes (here: `runs/_latest.json`) can be counted via
    /// `tests/derived_summary_write_discipline.rs`'s log-bracketing
    /// technique (see that file's module doc comment for why this beats a
    /// `stat` before/after).
    fn spawn_with_derived_log(log_path: &std::path::Path) -> Self {
        Self::spawn_inner(Some(log_path))
    }

    fn spawn_inner(derived_log: Option<&std::path::Path>) -> Self {
        let mut cmd = Command::new(binary());
        if let Some(log_path) = derived_log {
            cmd.env("HANDOFF_MCP_DERIVED_WRITE_LOG", log_path);
        }
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

#[test]
fn trace_record_writes_a_run_file_and_updates_the_latest_cache_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-record-e2e" }),
    );

    // A layer document with one item — trace_record must fill body_hash
    // from this SubItem's own body_hash automatically.
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "system-test-trace-e2e",
            "title": "System tests (trace_record E2E)",
            "body": "# System tests\n\n### ST-901 Lockout end to end\n\nLock the account end to end.\n",
            "layer": "system_test",
        }),
    );
    assert!(saved.get("doc_id").is_some(), "doc_save failed: {saved}");

    // 1. A known item: recorded with no warnings, body_hash filled in.
    let result = server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [
                {"item": "ST-901", "result": "pass", "note": "ran locally", "evidence": ["tests/e2e.rs::lockout"]}
            ],
            "executor_kind": "human",
            "executor_id": "reviewer-1",
            "task_id": "t1",
        }),
    );
    let run_id = result["run_id"].as_str().expect("run_id").to_string();
    assert_eq!(result["recorded"], 1);
    assert_eq!(
        result["warnings"].as_array().unwrap().len(),
        0,
        "a known stable_id must not warn: {result}"
    );

    let run_path = handoff.join("runs").join(format!("{run_id}.json"));
    let run_json: Value = serde_json::from_str(
        &std::fs::read_to_string(&run_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", run_path.display())),
    )
    .unwrap();
    assert_eq!(run_json["run_id"], run_id);
    assert_eq!(run_json["executor"]["kind"], "human");
    assert_eq!(run_json["executor"]["id"], "reviewer-1");
    assert_eq!(run_json["task_id"], "t1");
    assert!(
        run_json["commit"].is_string(),
        "commit must always be a string (empty on failure): {run_json}"
    );
    assert_eq!(run_json["results"][0]["item"], "ST-901");
    assert_eq!(run_json["results"][0]["result"], "pass");
    assert_eq!(run_json["results"][0]["note"], "ran locally");
    assert_eq!(
        run_json["results"][0]["evidence"][0],
        "tests/e2e.rs::lockout"
    );
    assert!(
        run_json["results"][0]["body_hash"].is_string(),
        "a resolvable layer item's body_hash must be auto-filled: {run_json}"
    );

    let latest_path = handoff.join("runs").join("_latest.json");
    let latest_json: Value =
        serde_json::from_str(&std::fs::read_to_string(&latest_path).unwrap()).unwrap();
    assert_eq!(latest_json["count"], 1);
    assert_eq!(latest_json["max_run_id"], run_id);
    assert_eq!(latest_json["items"]["ST-901"]["result"], "pass");
    assert_eq!(latest_json["items"]["ST-901"]["run_id"], run_id);

    // .handoff/.gitignore must have been created/appended idempotently.
    let gitignore = std::fs::read_to_string(handoff.join(".gitignore")).unwrap();
    assert!(gitignore.contains("/runs/_latest.json"));

    // 2. An unknown item is still saved, with a warning naming it.
    let result2 = server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "GHOST-404", "result": "fail"}],
        }),
    );
    let run_id2 = result2["run_id"].as_str().expect("run_id").to_string();
    assert_ne!(run_id2, run_id, "each call must allocate a distinct run_id");
    let warnings2 = result2["warnings"].as_array().unwrap();
    assert!(
        warnings2
            .iter()
            .any(|w| w.as_str().unwrap().contains("GHOST-404")),
        "unknown item must produce a warning naming it: {result2}"
    );

    let latest_after_second: Value = serde_json::from_str(
        &std::fs::read_to_string(handoff.join("runs").join("_latest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(latest_after_second["count"], 2);
    assert_eq!(latest_after_second["max_run_id"], run_id2);
    // The first run's item is still present — the second sync is
    // incremental, not a destructive rebuild.
    assert_eq!(latest_after_second["items"]["ST-901"]["result"], "pass");
    assert_eq!(latest_after_second["items"]["GHOST-404"]["result"], "fail");

    // 3. An invalid `result` value is rejected before anything is written.
    let (is_error, text) = server.call_raw(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "ST-901", "result": "maybe"}],
        }),
    );
    assert!(is_error, "an invalid result value must be rejected: {text}");
}

/// t360.8 (wiki/220 §2.6, wiki/240-performance-design.md §6 PR-8 revision):
/// `runs/_latest.json` is a derived cache — like
/// `docs/_requirements_summary.json`, it must be written at most once per
/// request (`tests/derived_summary_write_discipline.rs`'s discipline, "1
/// リクエスト 1 ファイル 1 回まで"). Filters the write log by filename
/// (mirrors `tests/trace_report_slice_e2e.rs`'s `summary_write_count`)
/// rather than counting every logged line, since other requests in this
/// process's lifetime could in principle log other derived files.
#[test]
fn trace_record_writes_the_latest_cache_at_most_once_per_request() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = tmp.path().join("derived_writes.log");

    let mut server = Server::spawn_with_derived_log(&log_path);
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-record-write-discipline" }),
    );

    let before = derived_write_count(&log_path, "_latest.json");
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "ST-1", "result": "pass"}],
        }),
    );
    let after = derived_write_count(&log_path, "_latest.json");

    assert_eq!(
        after - before,
        1,
        "a single handoff_trace_record call must write runs/_latest.json exactly once"
    );
}

/// t360.13 (wiki/220 §3.4, manager decision after measurement): an earlier
/// revision of this task wired `handoff_trace_record` to also rebuild/write
/// `.handoff/docs/_trace_report.json` — measured p50 ~271ms at L scale once
/// that was in place, blowing this op's own ~100ms PR-4 budget
/// (`tests/perf_budgets.toml`'s `trace_record` entry) by ~2.7x. It was
/// reverted: `handoff_trace_record` must never write `_trace_report.json` at
/// all (only `handoff_trace_report`/CLI `trace report` do) — a fresh result
/// leaves the derived file's `inputs` fingerprint stale until the next
/// `trace report` call, which is exactly the staleness handoff-vscode's own
/// design already detects and reacts to (wiki/100 §3.3).
#[test]
fn trace_record_never_writes_the_trace_report_derived_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = tmp.path().join("derived_writes.log");

    let mut server = Server::spawn_with_derived_log(&log_path);
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-record-no-trace-report-write" }),
    );

    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "ST-1", "result": "pass"}],
        }),
    );

    assert_eq!(
        derived_write_count(&log_path, "_trace_report.json"),
        0,
        "handoff_trace_record must never write _trace_report.json (PR-4 budget, see doc comment)"
    );
    assert!(
        !dir.join(".handoff/docs/_trace_report.json").exists(),
        "_trace_report.json must not even exist after a trace_record-only session"
    );
}

/// Number of derived-file write lines logged so far at `log_path`
/// (`HANDOFF_MCP_DERIVED_WRITE_LOG`'s `"{path}\t{bytes}\n"` format) whose
/// path ends in `filename` — lets a test isolate one derived file's write
/// count from the log even when other derived files are also written on the
/// same request.
fn derived_write_count(log_path: &std::path::Path, filename: &str) -> usize {
    std::fs::read_to_string(log_path)
        .map(|s| s.lines().filter(|line| line.contains(filename)).count())
        .unwrap_or(0)
}
