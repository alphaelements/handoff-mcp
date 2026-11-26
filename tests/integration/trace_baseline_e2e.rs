//! Real-binary MCP-tool E2E for `handoff_trace_baseline`
//! (wiki/270-vmodel-m3-design.md §2.4/§4.1, M3-06, FR-405 create/list part).

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

fn run_git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git invocation");
    assert!(status.success(), "git {args:?} failed");
}

/// `trace_baseline create` must write `.handoff/trace/baselines/<id>.json`
/// (§2.4 shape) and append an `_index.json` entry; a subsequent `list` call
/// must then return it.
#[test]
fn create_writes_a_baseline_file_and_list_returns_it() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-baseline-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-800 Something\n\nBody.\n",
        }),
    );

    let created = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "Sprint 1" }),
    );
    let baseline_id = created["baseline_id"]
        .as_str()
        .expect("baseline_id present")
        .to_string();
    assert!(!baseline_id.is_empty());
    assert!(created["coverage_summary"].is_object(), "{created}");
    assert!(created["items_count"].as_u64().unwrap() >= 1, "{created}");

    let baseline_path = dir
        .join(".handoff")
        .join("trace")
        .join("baselines")
        .join(format!("{baseline_id}.json"));
    assert!(baseline_path.exists(), "baseline file must exist on disk");
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(&baseline_path).unwrap()).unwrap();
    assert_eq!(on_disk["label"], "Sprint 1");
    assert!(on_disk["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["id"] == "REQ-800"));
    assert!(on_disk["coverage_summary"].is_object());
    assert!(on_disk["gap_counts"].is_object());
    assert!(on_disk["state_summary"].is_object());

    let listed = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let baselines = listed["baselines"].as_array().expect("baselines array");
    assert_eq!(baselines.len(), 1, "{listed}");
    assert_eq!(baselines[0]["baseline_id"], baseline_id);
    assert_eq!(baselines[0]["label"], "Sprint 1");
}

/// Two `create` calls must produce two distinct baseline files, and `list`
/// must return both, newest first.
#[test]
fn two_creates_then_list_returns_two_entries_newest_first() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-e2e-2" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-baseline-e2e-2",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-801 Something\n\nBody.\n",
        }),
    );

    let first = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "first" }),
    );
    std::thread::sleep(Duration::from_millis(10));
    let second = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "second" }),
    );

    let first_id = first["baseline_id"].as_str().unwrap().to_string();
    let second_id = second["baseline_id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);

    let listed = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let baselines = listed["baselines"].as_array().unwrap();
    assert_eq!(baselines.len(), 2, "{listed}");
    assert_eq!(baselines[0]["baseline_id"], second_id, "newest first");
    assert_eq!(baselines[1]["baseline_id"], first_id);
}

/// `tag?` omitted falls back to `resolve_tags_at_head` (§2.4) — a repo whose
/// HEAD carries a tag at create time must have that tag recorded.
#[test]
fn create_without_tag_arg_auto_resolves_from_git_head() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    run_git(&dir, &["init", "-q", "-b", "main"]);
    run_git(&dir, &["config", "user.email", "test@example.com"]);
    run_git(&dir, &["config", "user.name", "Test"]);

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-e2e-tag" }),
    );
    run_git(&dir, &["add", "-A"]);
    run_git(&dir, &["commit", "-q", "-m", "initial"]);
    run_git(&dir, &["tag", "v9.9.9"]);

    let created = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create" }),
    );
    assert_eq!(created["tag"], "v9.9.9", "{created}");
}

/// `trace_baseline diff` (M3-07, FR-405 diff part): creating a baseline,
/// adding a new requirement item, creating a second baseline, then diffing
/// the two must report the new item under `added` and the resulting
/// coverage/state shift under `state_changes`.
#[test]
fn diff_between_two_baselines_reports_added_items_and_state_changes() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-diff-e2e" }),
    );
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-baseline-diff-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 First\n\nBody.\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    let first = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "first" }),
    );
    let first_id = first["baseline_id"].as_str().unwrap().to_string();

    // Add a second requirement item between the two baselines.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "doc_id": doc_id,
            "body": "# Requirements\n\n### REQ-900 First\n\nBody.\n\n### REQ-901 Second\n\nBody.\n",
        }),
    );
    let second = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "second" }),
    );
    let second_id = second["baseline_id"].as_str().unwrap().to_string();
    assert_ne!(first_id, second_id);

    let diff = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "diff", "from": first_id, "to": second_id }),
    );
    let added = diff["added"].as_array().expect("added array");
    assert_eq!(
        added
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["REQ-901"],
        "{diff}"
    );
    assert!(diff["removed"].as_array().unwrap().is_empty(), "{diff}");
    assert!(diff["changed"].as_array().unwrap().is_empty(), "{diff}");
    // REQ-901 starts with no verifier -> uncovered, so state_summary's
    // uncovered tally must have grown by 1 between the two baselines.
    let state_changes = diff["state_changes"].as_object().expect("state_changes");
    assert_eq!(state_changes.get("uncovered"), Some(&json!(1)), "{diff}");
    assert!(diff["warnings"].as_array().unwrap().is_empty(), "{diff}");
}

/// `diff` against an unresolvable `from`/`to` (unknown baseline_id) must
/// report a warning rather than erroring the whole call.
#[test]
fn diff_with_unknown_baseline_id_reports_a_warning_not_an_error() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-diff-e2e-unknown" }),
    );

    let diff = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "diff", "from": "does-not-exist", "to": "current" }),
    );
    let warnings = diff["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("not found")),
        "{diff}"
    );
    assert!(diff["added"].as_array().unwrap().is_empty(), "{diff}");
}

/// An explicit `tag` argument is stored verbatim, with no check against git.
#[test]
fn create_with_explicit_tag_arg_is_stored_verbatim() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-baseline-e2e-explicit-tag" }),
    );

    let created = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "tag": "not-a-real-tag" }),
    );
    assert_eq!(created["tag"], "not-a-real-tag", "{created}");
}
